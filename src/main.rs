//! Terminal BitTorrent client built on librqbit and ratatui.
//!
//! The process runs as two halves that never share memory. `run_app` owns the
//! terminal, the [`app::App`] state and every key press; `run_engine` runs in
//! its own tokio task and owns the librqbit `Session`. They talk over four mpsc
//! channels: commands down (`EngineCommand`, 32 slots), torrent snapshots up
//! (`Vec<TorrentInfo>`, 4 slots), status-bar strings up (16 slots),
//! and one-shot engine facts up (`EngineInfo`, 4 slots). Nothing in `ui` or
//! `app` may touch the session directly — that separation keeps rendering off
//! the engine's critical path and lets the UI notice an engine panic instead of
//! rendering frozen state forever.
//!
//! Because the channels are bounded, the UI must never fan out one send per
//! torrent: the batch variants (`PauseMany`, `DeleteMany`) exist so a bulk
//! action cannot fill the command queue while the engine is blocked pushing
//! state the UI has not drained yet.

mod app;
mod config;
mod engine;
mod player;
mod search;
mod types;
mod ui;

use std::io;
use std::path::Path;

use anyhow::Result;
use app::App;
use clap::Parser;
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use engine::torrent::{EngineCommand, EngineInfo};
use futures::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};
use tokio::sync::mpsc;
use tracing_subscriber::EnvFilter;
use types::{AppMode, DetailTab, Pane};
use ui::input::{validate_torrent_source, InputWidget};

/// Speed-limit cap in KB/s (10 GB/s). Applied by [`clamped_speed_limit`] to
/// every entry point: the throttle dialog, the values read from `config.toml`,
/// and the limits the engine enforces.
const MAX_SPEED_LIMIT_KBPS: u64 = 10_485_760;

/// Clamp a user-supplied speed limit. The single choke point for the cap —
/// the status bar multiplies by 1024 without saturating, and the engine casts
/// to `i64`, so a value that skips this can overflow the display or invert the
/// upload bucket and pause every torrent forever.
pub(crate) fn clamped_speed_limit(kbps: u64) -> u64 {
    kbps.min(MAX_SPEED_LIMIT_KBPS)
}

/// Size at which the log is rotated on startup. The default filter is
/// `torrenttui=warn`, which writes almost nothing, so this only bites people
/// running with `RUST_LOG` turned up.
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

/// Maximum digits accepted in the throttle input dialog. Combined with the
/// numeric cap above, anything past this gets truncated visually.
const MAX_THROTTLE_INPUT_DIGITS: usize = 8;

#[derive(Parser)]
#[command(name = "torrenttui", version, about = "Terminal BitTorrent client")]
struct Cli {
    /// Magnet link or .torrent file path to add on startup
    torrent_source: Option<String>,

    /// Download directory override
    #[arg(short, long)]
    download_dir: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Set up panic hook to restore terminal. Disable mouse capture too —
    // otherwise the user's terminal will keep emitting mouse-event escape
    // codes after a crash until they `reset(1)`.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic_info| {
        let _ = disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            DisableMouseCapture,
            DisableBracketedPaste
        );
        original_hook(panic_info);
    }));

    // Parse before touching the filesystem. clap exits from inside `parse()`
    // for --version and --help, so doing this first keeps those from creating
    // a config directory and writing a startup marker to the log of a machine
    // that may never have run the app (#44).
    let cli = Cli::parse();

    // Set up logging to file. Default filter is "torrenttui=warn" so librqbit
    // internals (peer IPs, tracker URLs, info hashes) don't get persisted to
    // disk by default. Users who want verbose logs can set RUST_LOG.
    let log_dir = config::Config::config_dir();
    std::fs::create_dir_all(&log_dir)?;
    let log_file = open_log_file(&log_dir)?;
    // The log is cumulative now, so mark where each run begins. Written
    // straight to the file rather than through tracing: the default filter is
    // `torrenttui=warn` and a startup notice is not a warning, so it would be
    // filtered out for exactly the users who need it. Timestamps come from the
    // tracing lines that follow.
    {
        use std::io::Write;
        let _ = writeln!(
            &log_file,
            "=== torrenttui {} ===",
            env!("CARGO_PKG_VERSION")
        );
    }
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("torrenttui=warn"));
    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();

    // Load config. A parse error returns `(default, Some(warning))` so we can
    // surface it; an Err is an I/O failure either reading an existing config or
    // — on first run, when `load` writes the defaults out — creating the config
    // directory, serializing, fsyncing or renaming the new file.
    let (mut config, config_warning) = match config::Config::load() {
        Ok(pair) => pair,
        Err(e) => {
            tracing::warn!("Failed to load config, using defaults: {e}");
            (
                config::Config::default(),
                Some(format!("Config load failed: {e}")),
            )
        }
    };
    // A quoted `--download-dir '~/dl'` dodges shell tilde expansion, so
    // expand here like the config loader does.
    if let Some(ref dir) = cli.download_dir {
        config.general.download_dir = config::expand_tilde(dir);
    }

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Bracketed paste is best-effort: terminals that don't support it (e.g.
    // some Windows consoles) just won't send Event::Paste, and that's fine.
    let _ = execute!(stdout, EnableBracketedPaste);
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, cli, config, config_warning).await;

    let _ = execute!(terminal.backend_mut(), DisableBracketedPaste);
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        eprintln!("Error: {}", e);
    }

    Ok(())
}

/// Send an engine command, surfacing failure to the user instead of dropping
/// it silently. If the engine task has died, the channel send returns an Err
/// and the user gets a status-bar message instead of an inert key press.
async fn send_cmd(tx: &mpsc::Sender<EngineCommand>, cmd: EngineCommand, app: &mut App) {
    if let Err(e) = tx.send(cmd).await {
        tracing::error!("engine channel send failed: {e}");
        app.set_error("Engine stopped responding".to_string());
    }
}

/// Open `torrenttui.log` for appending, rotating it first if it has grown past
/// [`MAX_LOG_BYTES`].
///
/// Appending rather than truncating matters because the failures people most
/// want logs for — a crash, a hang, a torrent misbehaving — are usually
/// followed immediately by restarting the app, and truncating on launch
/// destroyed exactly the evidence they came back for (#42). One previous
/// generation is kept as `torrenttui.log.1`.
fn open_log_file(log_dir: &Path) -> io::Result<std::fs::File> {
    let path = log_dir.join("torrenttui.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_BYTES) {
        // A failed rotation is not worth refusing to start over; the append
        // below just carries on in the oversized file.
        let _ = std::fs::rename(&path, log_dir.join("torrenttui.log.1"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
}

/// The UI loop. Spawns the engine task, then alternates between draining the
/// engine's channels, rendering, and awaiting the next event.
///
/// Rendering is change-driven: `needs_render` is set by anything that alters
/// what is on screen, and the frame interval exists only to age out timed
/// messages and advance the metadata spinner. An idle session with no torrents
/// therefore draws nothing at all.
///
/// Each pass drains the state/message/info channels with `try_recv` *before*
/// the `select!`, so a burst of engine pushes collapses into a single repaint
/// instead of one per message. The engine's `JoinHandle` is retained rather
/// than detached purely so a panicked engine is noticed here instead of leaving
/// the UI rendering frozen state forever.
///
/// On quit it sends `Shutdown` and waits up to 5 s for the engine to flush
/// librqbit's persisted state and finish any watch-folder cleanup.
async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cli: Cli,
    config: config::Config,
    config_warning: Option<String>,
) -> Result<()> {
    let mut app = App::new();
    let mut input_widget = InputWidget::new();

    // Clamp here, not just in the throttle dialog: these reach an unchecked
    // `* 1024` in the status bar, which overflows on an absurd config value
    // (#49).
    app.speed_limit_download_kbps = clamped_speed_limit(config.network.max_download_speed_kbps);
    app.speed_limit_upload_kbps = clamped_speed_limit(config.network.max_upload_speed_kbps);
    app.confirm_on_quit = config.general.confirm_on_quit;
    app.player_config = config.player.clone();
    app.watch_dir_configured = config.general.watch_dir.is_some();
    app.theme = config.theme.clone();
    app.config_theme = config.theme.clone();
    // The rotation `T` cycles through starts on the "config" slot, whatever
    // preset/overrides that resolved to.
    app.theme_name = "config".to_string();
    // Group names for the search view's `g` key; dirs stay engine-side.
    app.group_names = config.groups.iter().map(|g| g.name.clone()).collect();
    // Split orientation for the search pane; `|` flips it at runtime.
    app.split_vertical = config.ui.search_split == "right";

    if let Some(msg) = config_warning {
        app.set_error(msg);
    }

    let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCommand>(32);
    let (state_tx, mut state_rx) = mpsc::channel::<Vec<types::TorrentInfo>>(4);
    let (msg_tx, mut msg_rx) = mpsc::channel::<String>(16);
    // Engine-to-UI one-shot facts (HTTP API base URL today; future use:
    // listening port, DHT state). Small capacity because messages are rare.
    let (info_tx, mut info_rx) = mpsc::channel::<EngineInfo>(4);
    // Free-space probe results. Depth 1: only the newest reading matters, and
    // a full channel just means a probe is still in flight.
    let (disk_tx, mut disk_rx) = mpsc::channel::<Option<u64>>(1);
    // Search responses. Each dispatched query produces exactly one response,
    // so the depth only needs to absorb a few back-to-back searches.
    let (search_tx, mut search_rx) = mpsc::channel::<search::SearchResponse>(4);

    let engine_config = config.clone();
    // Keep the JoinHandle so we can both detect engine death mid-run and
    // await its persistence flush on shutdown. Wrapped in Option so we can
    // consume it from either path.
    let mut engine_handle: Option<tokio::task::JoinHandle<()>> = Some(tokio::spawn(async move {
        if let Err(e) =
            engine::torrent::run_engine(engine_config, cmd_rx, state_tx, msg_tx, info_tx).await
        {
            tracing::error!("Engine error: {}", e);
        }
    }));

    if let Some(ref source) = cli.torrent_source {
        // Same tilde story as --download-dir; magnet links never start with
        // `~` so expansion is a no-op for them.
        let source = config::expand_tilde(source);
        match validate_torrent_source(&source) {
            Ok(()) => {
                // The CLI add stays ungrouped — groups are picked in the TUI.
                // Push the placeholder too: run_app's loop is what receives
                // the engine's `AddFinished`, so the pairing holds here.
                app.push_pending_add(source.clone());
                send_cmd(
                    &cmd_tx,
                    EngineCommand::AddTorrent {
                        source,
                        group: None,
                    },
                    &mut app,
                )
                .await;
            }
            Err(e) => app.set_error(e),
        }
    }

    let download_dir = config.general.download_dir.clone();
    // Provider list is fixed at startup; each search spawn clones it.
    let search_providers = config.search.build_providers();
    let mut event_stream = EventStream::new();
    // Frame-rate cap for animations and timed-message aging; honors
    // `config.ui.refresh_rate_ms` (clamped to 16–1000 ms / ~60–1 FPS). Rendering
    // itself is change-driven via `needs_render`, so this only paces idle repaints.
    let mut frame_interval = tokio::time::interval(std::time::Duration::from_millis(
        config.ui.refresh_rate_ms.clamp(16, 1000),
    ));
    let mut needs_render = true;
    let mut engine_died = false;

    loop {
        // Detect engine death: previously a panic in the spawned task would
        // silently drop the JoinHandle and the UI would keep rendering stale
        // state with no indication. is_finished() is a cheap flag read.
        if engine_handle.as_ref().is_some_and(|h| h.is_finished()) {
            if let Some(h) = engine_handle.take() {
                match h.await {
                    Ok(()) => tracing::info!("Engine task ended"),
                    Err(e) => tracing::error!("Engine task panicked: {}", e),
                }
            }
            if !app.should_quit {
                app.set_error("Engine task ended unexpectedly; quitting".to_string());
                app.should_quit = true;
                // Without this the loop can quit without ever drawing the
                // message, so the TUI just vanishes. The Err below is what
                // puts it on stderr once the terminal is restored.
                needs_render = true;
                engine_died = true;
            }
        }

        while let Ok(torrents) = state_rx.try_recv() {
            // Only repaint when something on screen actually moved.
            needs_render |= app.handle_state_push(torrents);
        }
        while let Ok(msg) = msg_rx.try_recv() {
            app.set_info(msg);
            needs_render = true;
        }
        while let Ok(info) = info_rx.try_recv() {
            apply_engine_info(&mut app, info);
            needs_render = true;
        }
        while let Ok(free) = disk_rx.try_recv() {
            if app.free_disk_space != free {
                app.set_disk_space(free);
                needs_render = true;
            }
        }
        while let Ok(response) = search_rx.try_recv() {
            app.apply_search_response(response);
            needs_render = true;
        }

        app.clear_expired_messages();

        if needs_render {
            needs_render = false;
            terminal.draw(|f| {
                let chunks = ui::layout::get_layout(f.area());

                ui::layout::render_header(f, chunks[0], &app);

                // The split is on only when the pane is open *and* the main
                // area clears the size thresholds; below them the full-area
                // search modes render instead, exactly like before panes
                // existed. Key handlers gate focus routing on this flag, so
                // a hidden pane never steals j/k from the visible table.
                let split_active = app.search_pane_open
                    && ui::layout::use_split_layout(chunks[1], app.split_vertical);
                app.split_active = split_active;

                match app.mode {
                    AppMode::Detail => {
                        ui::detail::render_detail(f, chunks[1], &mut app);
                        app.table_area = None;
                    }
                    AppMode::SearchResults if !split_active => {
                        ui::search::render_search_results(f, chunks[1], &mut app);
                        app.table_area = None;
                    }
                    _ if split_active => {
                        let panes = ui::layout::split_main(chunks[1], app.split_vertical);
                        app.table_area = Some(panes[0]);
                        ui::table::render_table(f, panes[0], &mut app);
                        // The query strip is editable only while the user is
                        // actually typing in it.
                        let input_active = app.mode == AppMode::SearchInput;
                        ui::search::render_search_pane(
                            f,
                            panes[1],
                            &mut app,
                            &input_widget,
                            input_active,
                        );
                    }
                    _ => {
                        app.table_area = Some(chunks[1]);
                        ui::table::render_table(f, chunks[1], &mut app);
                    }
                }

                match app.mode {
                    AppMode::Input => {
                        ui::input::render_input(f, chunks[2], &input_widget, &app.theme);
                    }
                    // With the split on screen the query is edited in the
                    // pane's own strip, so the bottom bar stays a status bar.
                    AppMode::SearchInput if !split_active => {
                        ui::input::render_search_input(f, chunks[2], &input_widget, &app.theme);
                    }
                    AppMode::Filter => {
                        ui::layout::render_filter_bar(f, chunks[2], &app.filter_text, &app.theme);
                    }
                    AppMode::ThrottleInput => {
                        ui::layout::render_throttle_bar(
                            f,
                            chunks[2],
                            app.throttle_step,
                            &app.throttle_input_buf,
                            &app.theme,
                        );
                    }
                    _ => {
                        ui::layout::render_status_bar(f, chunks[2], &app);
                    }
                }

                if app.mode == AppMode::Help {
                    ui::help::render_help(f, f.area(), &app.theme);
                }
                if app.mode == AppMode::ConfirmDelete {
                    let label = delete_dialog_label(&app);
                    if !label.is_empty() {
                        ui::dialogs::render_delete_dialog(
                            f,
                            f.area(),
                            &label,
                            app.watch_dir_configured,
                            &app.theme,
                        );
                    }
                }
                if app.mode == AppMode::ConfirmQuit {
                    ui::dialogs::render_quit_dialog(f, f.area(), &app.theme);
                }
            })?;
        }

        if app.should_quit {
            send_cmd(&cmd_tx, EngineCommand::Shutdown, &mut app).await;
            // Give the engine task a chance to flush librqbit's persisted
            // state. The timeout caps how long we wait if it's stuck.
            if let Some(h) = engine_handle.take() {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), h).await;
            }
            return if engine_died {
                Err(anyhow::anyhow!("engine task ended unexpectedly"))
            } else {
                Ok(())
            };
        }

        tokio::select! {
            event = event_stream.next() => {
                match event {
                    Some(Ok(Event::Key(key))) => {
                        if key.kind != KeyEventKind::Press {
                            continue;
                        }
                        // Ctrl+C: first opens quit dialog, second force-quits
                        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                            let was_detail = app.mode == AppMode::Detail;
                            if app.mode == AppMode::ConfirmQuit {
                                app.should_quit = true;
                            } else {
                                app.mode = AppMode::ConfirmQuit;
                            }
                            if was_detail {
                                // Quit dialog overlays Detail — drop the heavy
                                // per-tick allocations until the user returns.
                                send_cmd(
                                    &cmd_tx,
                                    EngineCommand::SetDetailTorrent(None),
                                    &mut app,
                                )
                                .await;
                            }
                            needs_render = true;
                            continue;
                        }
                        match app.mode {
                            AppMode::Input => handle_input_mode(&mut app, &mut input_widget, key, &cmd_tx).await,
                            AppMode::Normal => handle_normal_mode(&mut app, &mut input_widget, key, &cmd_tx).await,
                            AppMode::Detail => handle_detail_mode(&mut app, key, &cmd_tx).await,
                            AppMode::Help => handle_help_mode(&mut app, key),
                            AppMode::ConfirmDelete => handle_delete_mode(&mut app, key, &cmd_tx).await,
                            AppMode::ConfirmQuit => handle_quit_mode(&mut app, key),
                            AppMode::Filter => handle_filter_mode(&mut app, key),
                            AppMode::ThrottleInput => handle_throttle_mode(&mut app, key, &cmd_tx).await,
                            AppMode::SearchInput => {
                                // The handler is pure state; the network spawn
                                // stays here so tests never touch it.
                                if let Some(query) =
                                    handle_search_input_mode(&mut app, &mut input_widget, key)
                                {
                                    let providers = search_providers.clone();
                                    let tx = search_tx.clone();
                                    tokio::spawn(async move {
                                        search::run_search(providers, query, tx).await;
                                    });
                                }
                            }
                            AppMode::SearchResults => {
                                handle_search_results_mode(&mut app, &mut input_widget, key, &cmd_tx).await
                            }
                        }
                        needs_render = true;
                    }
                    Some(Ok(Event::Paste(s))) if app.mode == AppMode::Input || app.mode == AppMode::SearchInput => {
                        // Bracketed-paste payload (e.g. a magnet link from the
                        // clipboard). push_str filters control chars per-char
                        // so escape sequences in the paste never reach the
                        // buffer or the engine.
                        input_widget.push_str(&s);
                        needs_render = true;
                    }
                    Some(Ok(Event::Paste(s))) if app.mode == AppMode::Filter => {
                        for c in s.chars() {
                            if !c.is_control() {
                                app.push_filter_char(c);
                            }
                        }
                        needs_render = true;
                    }
                    Some(Ok(Event::Mouse(mouse)))
                        if app.mode == AppMode::Normal
                            && matches!(
                                mouse.kind,
                                MouseEventKind::Down(crossterm::event::MouseButton::Left)
                            ) =>
                    {
                        if let Some(area) = app.table_area {
                            // Layout: row 0 top-border, row 1 header, rows 2..h-2
                            // data, row h-1 bottom-border. `< content_bottom`
                            // (= area.y + h - 1) correctly excludes the bottom
                            // border row.
                            let content_y = area.y + 2;
                            let content_bottom = area.y + area.height.saturating_sub(1);
                            if mouse.row >= content_y
                                && mouse.row < content_bottom
                                && mouse.column >= area.x
                                && mouse.column < area.x + area.width
                            {
                                // Account for table scroll: the visible top row
                                // is the table state's offset.
                                let visible_offset = (mouse.row - content_y) as usize;
                                let clicked_index = app.table_state.offset() + visible_offset;
                                let count = app.sorted_torrents().len();
                                if clicked_index < count {
                                    app.selected_index = clicked_index;
                                    app.update_selected_id();
                                    app.table_state.select(Some(clicked_index));
                                    needs_render = true;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some(torrents) = state_rx.recv() => {
                needs_render |= app.handle_state_push(torrents);
            }
            Some(msg) = msg_rx.recv() => {
                app.set_info(msg);
                needs_render = true;
            }
            Some(info) = info_rx.recv() => {
                apply_engine_info(&mut app, info);
                needs_render = true;
            }
            Some(response) = search_rx.recv() => {
                app.apply_search_response(response);
                needs_render = true;
            }
            _ = frame_interval.tick() => {
                // Only burn a render frame when something actually animates.
                // Disk-space refresh is internally throttled to ~5s.
                let prev_disk = app.free_disk_space;
                // Off the reactor: `available_space` is a blocking statvfs, and
                // on a hung NFS/SMB mount it blocks for the mount timeout —
                // which would freeze the whole TUI, not just this readout.
                if app.disk_space_due() {
                    let dir = download_dir.clone();
                    let tx = disk_tx.clone();
                    tokio::task::spawn_blocking(move || {
                        let _ = tx.try_send(fs4::available_space(&dir).ok());
                    });
                }
                let disk_changed = prev_disk != app.free_disk_space;

                if app.has_fetching_metadata() || app.search_in_flight || !app.pending_adds.is_empty() {
                    app.tick_spinner();
                    needs_render = true;
                } else if disk_changed
                    || app.error_message.is_some()
                    || app.info_message.is_some()
                {
                    // Re-render so timed messages can age out cleanly.
                    needs_render = true;
                }
            }
        }
    }
}

async fn handle_normal_mode(
    app: &mut App,
    input_widget: &mut InputWidget,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Char('q') => {
            if app.confirm_on_quit_required() {
                app.mode = AppMode::ConfirmQuit;
            } else {
                app.should_quit = true;
            }
        }
        KeyCode::Char('j') | KeyCode::Down => {
            // With the split on screen, navigation belongs to the focused
            // pane; without it, always to the downloads table.
            if app.search_has_focus() {
                app.search_next()
            } else {
                app.next()
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if app.search_has_focus() {
                app.search_previous()
            } else {
                app.previous()
            }
        }
        KeyCode::Char('a') => {
            app.mode = AppMode::Input;
            input_widget.clear();
        }
        KeyCode::Char('s') => {
            if app.search_pane_open {
                // Pane already open: refocus it and reopen the query for
                // editing, pre-filled — iterating on a search is editing,
                // not retyping.
                app.focused = Pane::Search;
                app.mode = AppMode::SearchInput;
                input_widget.clear();
                let query = app.search_query.clone();
                input_widget.push_str(&query);
            } else {
                app.open_search_pane();
                input_widget.clear();
            }
        }
        KeyCode::Char('w') if app.search_pane_open => {
            app.cycle_focus();
        }
        KeyCode::Char('|') if app.search_pane_open => {
            app.toggle_split();
        }
        KeyCode::Char('g') if app.search_has_focus() => {
            app.cycle_search_group();
        }
        KeyCode::Char('p') => {
            if app.has_marks() {
                let ids: Vec<usize> = app.marked_ids.iter().copied().collect();
                // "Any user-paused → resume all" is the intuitive model. A
                // throttle-paused torrent does not count: it reports Paused
                // mid duty cycle while the user never asked for it, and
                // treating it as paused flipped this whole batch to Resume
                // whenever a speed limit was set (#47). Queued torrents do
                // count — resuming them is how the user starts one early.
                let any_paused = ids.iter().any(|id| {
                    app.torrents.iter().any(|t| {
                        t.id == *id
                            && (matches!(t.status, types::TorrentStatus::Queued)
                                || (matches!(t.status, types::TorrentStatus::Paused)
                                    && !t.throttle_managed))
                    })
                });
                // Send the whole batch as one message — the 32-slot channel
                // would block on the 33rd send otherwise, and the engine's own
                // state_tx send (4-slot) could deadlock while the UI waits.
                let cmd = if any_paused {
                    EngineCommand::ResumeMany(ids)
                } else {
                    EngineCommand::PauseMany(ids)
                };
                send_cmd(cmd_tx, cmd, app).await;
                app.clear_marks();
            } else if let Some(torrent) = app.selected_torrent() {
                let id = torrent.id;
                // Resume only what the user paused. A torrent that is Paused
                // while throttle-managed is mid duty cycle, and `p` there means
                // "stop it properly", not "hand it back to the throttle".
                if matches!(torrent.status, types::TorrentStatus::Paused)
                    && torrent.throttle_managed
                {
                    send_cmd(cmd_tx, EngineCommand::Pause(id), app).await;
                } else {
                    match torrent.status {
                        // Seeding and Complete are pausable too — librqbit
                        // handles a finished torrent fine, and the marked and
                        // `P` paths already pause them, so leaving them out
                        // here made `p` a dropped keypress on a seeding row.
                        types::TorrentStatus::Downloading
                        | types::TorrentStatus::Complete
                        | types::TorrentStatus::Seeding => {
                            send_cmd(cmd_tx, EngineCommand::Pause(id), app).await;
                        }
                        types::TorrentStatus::Paused | types::TorrentStatus::Queued => {
                            // Queued resumes like a user pause: it leaves the
                            // queue and starts immediately, possibly over the
                            // configured cap — explicit intent wins.
                            send_cmd(cmd_tx, EngineCommand::Resume(id), app).await;
                        }
                        types::TorrentStatus::FetchingMetadata => {
                            app.set_info("Can't pause while fetching metadata".to_string());
                        }
                        _ => {}
                    }
                }
            }
        }
        KeyCode::Char('P') => {
            // Same test as the `p` handler: only a *user* pause counts as
            // paused. A throttle-managed torrent reports Paused mid duty
            // cycle, and treating that as paused made `P` send ResumeAll when
            // the user asked to pause everything. `.all()` on an empty
            // iterator is also true, so an idle session needs the emptiness
            // guard or `P` resumes nothing at all.
            let pausable: Vec<&types::TorrentInfo> = app
                .torrents
                .iter()
                .filter(|t| {
                    matches!(
                        t.status,
                        types::TorrentStatus::Downloading
                            | types::TorrentStatus::Paused
                            | types::TorrentStatus::Queued
                            | types::TorrentStatus::Complete
                            | types::TorrentStatus::Seeding
                    )
                })
                .collect();
            let all_paused = !pausable.is_empty()
                && pausable.iter().all(|t| {
                    // Queued counts as paused: it is engine-held, not running,
                    // so `P` on an all-held session resumes everything.
                    (matches!(t.status, types::TorrentStatus::Paused) && !t.throttle_managed)
                        || matches!(t.status, types::TorrentStatus::Queued)
                });

            let cmd = if all_paused {
                EngineCommand::ResumeAll
            } else {
                EngineCommand::PauseAll
            };
            send_cmd(cmd_tx, cmd, app).await;
        }
        // Guard on the *visible* list, like Enter does: with a filter that
        // matches nothing the dialog label is empty and the popup is never
        // drawn, leaving the user in a modal with nothing on screen.
        KeyCode::Char('d') if !app.sorted_torrents().is_empty() || app.has_marks() => {
            app.mode = AppMode::ConfirmDelete;
        }
        KeyCode::Enter if app.search_has_focus() => {
            // Enter on the focused search pane adds the selected hit and
            // stays put, so several hits can be queued from one search.
            add_selected_search_result(app, cmd_tx).await;
        }
        KeyCode::Enter if !app.sorted_torrents().is_empty() => {
            app.mode = AppMode::Detail;
            app.detail_tab = DetailTab::Stats;
            app.detail_file_index = 0;
            app.detail_peer_index = 0;
            app.detail_peer_scroll_offset = 0;
            // Tell the engine which torrent we're viewing so the next
            // snapshot includes files/peers/info for only this one.
            let detail_id = app.selected_torrent_id;
            send_cmd(cmd_tx, EngineCommand::SetDetailTorrent(detail_id), app).await;
        }
        KeyCode::Char('?') => {
            app.mode = AppMode::Help;
        }
        KeyCode::Tab => {
            let next = app.sort_column.next();
            app.change_sort_column(next);
        }
        KeyCode::Char('r') => {
            app.toggle_sort_reversed();
        }
        KeyCode::Char('/') => {
            app.mode = AppMode::Filter;
        }
        KeyCode::Char('t') => {
            app.mode = AppMode::ThrottleInput;
            app.throttle_step = 0;
            app.throttle_input_buf = if app.speed_limit_download_kbps > 0 {
                app.speed_limit_download_kbps.to_string()
            } else {
                String::new()
            };
        }
        // Session-only theme cycling; the `preset` key in config.toml is the
        // persistence path. The keypress path sets `needs_render` afterwards,
        // so the repaint comes for free.
        KeyCode::Char('T') => app.cycle_theme(),
        KeyCode::Char(' ') => {
            app.toggle_mark();
            app.next();
        }
        KeyCode::Char('v') => {
            app.mark_all();
        }
        KeyCode::Char('V') => {
            app.clear_marks();
        }
        KeyCode::Esc => {
            // First Esc with the split on screen and focus on Search closes
            // the pane; anything else keeps the long-standing "clear marks"
            // meaning. The split_active gate (inside search_has_focus) keeps
            // a hidden pane on a small terminal from swallowing the key.
            if app.search_has_focus() {
                app.close_search_pane();
            } else {
                app.clear_marks();
            }
        }
        _ => {}
    }
}

async fn handle_input_mode(
    app: &mut App,
    input_widget: &mut InputWidget,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Esc => {
            app.mode = AppMode::Normal;
        }
        KeyCode::Enter => {
            // No shell is involved in TUI input, so `~/x.torrent` only works
            // if we expand it ourselves.
            let value = config::expand_tilde(input_widget.value().trim());
            match validate_torrent_source(&value) {
                Ok(()) => {
                    // The `a` dialog stays ungrouped; groups are picked in the
                    // search results view.
                    app.push_pending_add(value.clone());
                    send_cmd(
                        cmd_tx,
                        EngineCommand::AddTorrent {
                            source: value,
                            group: None,
                        },
                        app,
                    )
                    .await;
                    app.mode = AppMode::Normal;
                }
                Err(e) => {
                    app.set_error(e);
                    app.mode = AppMode::Normal;
                }
            }
        }
        KeyCode::Backspace => {
            input_widget.pop();
        }
        KeyCode::Char(c) => {
            input_widget.push(c);
        }
        _ => {}
    }
}

/// Keys for the search prompt. Pure state transitions: Enter on a non-empty
/// query returns it for the caller to spawn `run_search` with (keeping the
/// network out of tests); Esc leaves the prompt.
fn handle_search_input_mode(
    app: &mut App,
    input_widget: &mut InputWidget,
    key: crossterm::event::KeyEvent,
) -> Option<String> {
    match key.code {
        KeyCode::Esc => {
            // With the pane open, Esc only stops editing: the pane stays
            // open with its results browsable (focus moves to Search). The
            // follow-up Esc — now in Normal mode — is the one that closes
            // the pane. With no pane this is the old plain cancel.
            app.mode = AppMode::Normal;
            if app.search_pane_open {
                app.focused = Pane::Search;
            }
            None
        }
        KeyCode::Enter => {
            let query = input_widget.value().trim().to_string();
            if query.is_empty() {
                // Nothing to search for — stay in the prompt rather than
                // firing an empty query at every provider.
                None
            } else {
                app.begin_search(query.clone());
                Some(query)
            }
        }
        KeyCode::Backspace => {
            input_widget.pop();
            None
        }
        KeyCode::Char(c) => {
            input_widget.push(c);
            None
        }
        _ => None,
    }
}

/// Send `AddTorrent` for the selected search hit, tagged with the sticky
/// `search_group`. Shared by the results-mode Enter and the split-pane
/// routing (Normal mode, focus on Search). Stays in the current view so
/// several hits can be queued from one search.
async fn add_selected_search_result(app: &mut App, cmd_tx: &mpsc::Sender<EngineCommand>) {
    // Clone out of the borrow before mutating `app` for the message.
    let selected = app
        .selected_search_result()
        .map(|r| (r.magnet.clone(), r.title.clone()));
    if let Some((magnet, title)) = selected {
        let group = app.search_group.clone();
        // Placeholder row for the table; popped when the engine's
        // `AddFinished` arrives, however the add resolves.
        app.push_pending_add(title.clone());
        send_cmd(
            cmd_tx,
            EngineCommand::AddTorrent {
                source: magnet,
                group: group.clone(),
            },
            app,
        )
        .await;
        match group {
            // The engine confirms completion ("Added:"/"Queued:"/"Failed") via
            // the message channel once the add actually resolves; claiming
            // "Added" here would lie for dead magnets.
            Some(g) => app.set_info(format!("Adding: {} \u{2192} {}\u{2026}", title, g)),
            None => app.set_info(format!("Adding: {}\u{2026}", title)),
        }
    }
}

/// Keys for the results view: navigate, add the selected hit, refine the
/// search, or leave. Enter deliberately stays in the view so several hits can
/// be queued from one search.
async fn handle_search_results_mode(
    app: &mut App,
    input_widget: &mut InputWidget,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Esc => {
            // Like the prompt's Esc: the pane (when open) survives with its
            // hits; closing it is the next Esc's job, from Normal mode.
            app.mode = AppMode::Normal;
            if app.search_pane_open {
                app.focused = Pane::Search;
            }
        }
        KeyCode::Char('j') | KeyCode::Down => app.search_next(),
        KeyCode::Char('k') | KeyCode::Up => app.search_previous(),
        KeyCode::Enter => add_selected_search_result(app, cmd_tx).await,
        KeyCode::Char('g') => app.cycle_search_group(),
        KeyCode::Char('s') => {
            // Refine: back to the prompt with the current query pre-filled,
            // so iterating on a search is editing, not retyping.
            app.mode = AppMode::SearchInput;
            input_widget.clear();
            input_widget.push_str(&app.search_query);
        }
        _ => {}
    }
}

async fn handle_detail_mode(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.mode = AppMode::Normal;
            // Detail mode left — stop the engine from materializing
            // files/peers for the (now-hidden) torrent.
            send_cmd(cmd_tx, EngineCommand::SetDetailTorrent(None), app).await;
        }
        KeyCode::Tab => {
            app.detail_tab = app.detail_tab.next();
            app.detail_file_index = 0;
            app.detail_peer_index = 0;
            app.detail_peer_scroll_offset = 0;
        }
        KeyCode::Char('j') | KeyCode::Down => match app.detail_tab {
            DetailTab::Files => {
                if let Some(torrent) = app.selected_torrent() {
                    let file_count = torrent.files.len();
                    if file_count > 0 {
                        app.detail_file_index = (app.detail_file_index + 1).min(file_count - 1);
                    }
                }
            }
            DetailTab::Peers => {
                if let Some(torrent) = app.selected_torrent() {
                    let peer_count = torrent.peers.len();
                    if peer_count > 0 {
                        app.detail_peer_index = (app.detail_peer_index + 1).min(peer_count - 1);
                    }
                }
            }
            _ => {}
        },
        KeyCode::Char('k') | KeyCode::Up => match app.detail_tab {
            DetailTab::Files => {
                app.detail_file_index = app.detail_file_index.saturating_sub(1);
            }
            DetailTab::Peers => {
                app.detail_peer_index = app.detail_peer_index.saturating_sub(1);
            }
            _ => {}
        },
        KeyCode::Char(' ') if app.detail_tab == DetailTab::Files => {
            if let Some(torrent) = app.selected_torrent() {
                let torrent_id = torrent.id;
                let file_count = torrent.files.len();
                if app.detail_file_index < file_count {
                    app.toggle_file_selection(torrent_id, app.detail_file_index);
                    let selected = app.selected_file_indices(torrent_id, file_count);
                    send_cmd(
                        cmd_tx,
                        EngineCommand::SetSelectedFiles {
                            id: torrent_id,
                            file_indices: selected,
                        },
                        app,
                    )
                    .await;
                }
            }
        }
        KeyCode::Char('s') if app.detail_tab == DetailTab::Files => {
            handle_stream_keypress(app);
        }
        KeyCode::Char('S') if app.detail_tab == DetailTab::Files => {
            if let Some(torrent) = app.selected_torrent() {
                let torrent_id = torrent.id;
                let total_files = torrent.files.len();
                // Don't send an empty file selection — librqbit may interpret
                // it as "deselect everything" and pause the torrent. This
                // happens when the torrent briefly returns to FetchingMetadata.
                if total_files == 0 {
                    app.set_info("No files to apply yet (still fetching metadata)".to_string());
                } else {
                    let selected = app.selected_file_indices(torrent_id, total_files);
                    send_cmd(
                        cmd_tx,
                        EngineCommand::SetSelectedFiles {
                            id: torrent_id,
                            file_indices: selected,
                        },
                        app,
                    )
                    .await;
                }
            }
        }
        _ => {}
    }
}

fn handle_help_mode(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
            app.mode = AppMode::Normal;
        }
        _ => {}
    }
}

fn handle_filter_mode(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Esc => {
            app.clear_filter();
            app.mode = AppMode::Normal;
        }
        KeyCode::Enter => {
            app.mode = AppMode::Normal;
            app.restore_selection();
        }
        KeyCode::Backspace => {
            app.pop_filter_char();
        }
        KeyCode::Char(c) => {
            app.push_filter_char(c);
        }
        _ => {}
    }
}

async fn handle_throttle_mode(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Esc => {
            app.mode = AppMode::Normal;
        }
        KeyCode::Backspace => {
            app.throttle_input_buf.pop();
        }
        KeyCode::Char(c)
            if c.is_ascii_digit() && app.throttle_input_buf.len() < MAX_THROTTLE_INPUT_DIGITS =>
        {
            app.throttle_input_buf.push(c);
        }
        KeyCode::Enter => {
            let value = app
                .throttle_input_buf
                .parse::<u64>()
                .unwrap_or(0)
                .min(MAX_SPEED_LIMIT_KBPS);
            if app.throttle_step == 0 {
                app.throttle_download_value = value;
                app.throttle_step = 1;
                app.throttle_input_buf = if app.speed_limit_upload_kbps > 0 {
                    app.speed_limit_upload_kbps.to_string()
                } else {
                    String::new()
                };
            } else {
                app.throttle_upload_value = value;
                app.speed_limit_download_kbps = app.throttle_download_value;
                app.speed_limit_upload_kbps = app.throttle_upload_value;
                send_cmd(
                    cmd_tx,
                    EngineCommand::SetSpeedLimits {
                        download_kbps: app.speed_limit_download_kbps,
                        upload_kbps: app.speed_limit_upload_kbps,
                    },
                    app,
                )
                .await;
                app.mode = AppMode::Normal;
            }
        }
        _ => {}
    }
}

/// Keys for the delete confirmation: `k` keeps the downloaded files, `d`
/// deletes them, `c` or Esc cancels. Both `k` and `d` remove the torrent from
/// the session — the choice is only about the data on disk. Operates on the
/// marked set when there is one, otherwise on the selected row, and marks are
/// cleared afterwards either way, including marks hidden by the active filter.
async fn handle_delete_mode(
    app: &mut App,
    key: crossterm::event::KeyEvent,
    cmd_tx: &mpsc::Sender<EngineCommand>,
) {
    match key.code {
        KeyCode::Char('k') => {
            if app.has_marks() {
                let ids: Vec<usize> = app.marked_ids.iter().copied().collect();
                send_cmd(
                    cmd_tx,
                    EngineCommand::DeleteMany {
                        ids,
                        delete_files: false,
                    },
                    app,
                )
                .await;
                app.clear_marks();
            } else if let Some(torrent) = app.selected_torrent() {
                let id = torrent.id;
                send_cmd(
                    cmd_tx,
                    EngineCommand::Delete {
                        id,
                        delete_files: false,
                    },
                    app,
                )
                .await;
            }
            app.mode = AppMode::Normal;
        }
        KeyCode::Char('d') => {
            if app.has_marks() {
                let ids: Vec<usize> = app.marked_ids.iter().copied().collect();
                send_cmd(
                    cmd_tx,
                    EngineCommand::DeleteMany {
                        ids,
                        delete_files: true,
                    },
                    app,
                )
                .await;
                app.clear_marks();
            } else if let Some(torrent) = app.selected_torrent() {
                let id = torrent.id;
                send_cmd(
                    cmd_tx,
                    EngineCommand::Delete {
                        id,
                        delete_files: true,
                    },
                    app,
                )
                .await;
            }
            app.mode = AppMode::Normal;
        }
        KeyCode::Char('c') | KeyCode::Esc => {
            app.mode = AppMode::Normal;
        }
        _ => {}
    }
}

fn handle_quit_mode(app: &mut App, key: crossterm::event::KeyEvent) {
    match key.code {
        KeyCode::Char('y') => {
            app.should_quit = true;
        }
        KeyCode::Char('n') | KeyCode::Esc => {
            app.mode = AppMode::Normal;
        }
        _ => {}
    }
}

/// The ConfirmDelete question line. A single unmarked selection names the
/// torrent; with marks the count is the honest summary — the old label
/// quoted a generated "N selected torrents" string, which read like a
/// filename wrapped in someone else's quotes. Empty when there is nothing
/// to point at (filter matched nothing and nothing marked); the caller
/// skips drawing the dialog then, matching the old empty-label behavior.
/// The object phrase only — `"name"` for a single row, a count for marks.
/// The dialog supplies the surrounding "Delete ... ?" wording; this used to
/// duplicate it, rendering as: Delete "Delete "foo"?". Empty when there is
/// nothing to point at (filter matched nothing and nothing marked); the
/// caller skips drawing the dialog then.
fn delete_dialog_label(app: &App) -> String {
    if app.has_marks() {
        let n = app.marked_count();
        format!("{} marked torrent{}", n, if n == 1 { "" } else { "s" })
    } else {
        app.selected_torrent()
            .map(|t| format!("\"{}\"", ui::util::truncate(&t.name, 40)))
            .unwrap_or_default()
    }
}

fn apply_engine_info(app: &mut App, info: EngineInfo) {
    match info {
        EngineInfo::HttpApiReady { base_url } => {
            app.http_api_base = Some(base_url);
        }
        // One completed add pairs with one dispatched-add placeholder; the
        // engine sends this on every add outcome (success, queued,
        // already-managed, failure).
        EngineInfo::AddFinished => app.pop_pending_add(),
    }
}

/// Handle the `s` keystroke in Detail mode's Files tab. Composes the
/// librqbit stream URL and hands it to the configured external player.
/// All failure modes degrade to a status-bar message instead of a crash.
fn handle_stream_keypress(app: &mut App) {
    let base = match app.http_api_base.clone() {
        Some(b) => b,
        None => {
            app.set_error("Streaming API not ready yet".to_string());
            return;
        }
    };

    // Pull the (torrent_id, file_idx, file_name) tuple inside a tight borrow
    // scope so the immutable borrow ends before we mutate `app` to set
    // status messages.
    let resolved = app.selected_torrent().and_then(|t| {
        t.files
            .get(app.detail_file_index)
            .map(|f| (t.id, app.detail_file_index, f.name.clone()))
    });

    let (torrent_id, file_idx, file_name) = match resolved {
        Some(v) => v,
        None => {
            app.set_error("No file selected (waiting for metadata)".to_string());
            return;
        }
    };

    let url = engine::torrent::stream_url(&base, torrent_id, file_idx);
    match player::spawn_player(&app.player_config, &url) {
        Ok(()) => app.set_info(format!("Streaming {}", file_name)),
        Err(e) => app.set_error(format!("Failed to open player: {}", e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn key(code: KeyCode) -> crossterm::event::KeyEvent {
        crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn read_log(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("torrenttui.log")).unwrap()
    }

    #[test]
    fn clamped_speed_limit_bounds_every_entry_point() {
        // Calls the production function rather than re-implementing `.min()`,
        // so deleting the clamp fails here. The old version of this test
        // asserted `u64::MAX.min(CAP) == CAP` and could not detect #49 at all.
        assert_eq!(clamped_speed_limit(u64::MAX), MAX_SPEED_LIMIT_KBPS);
        assert_eq!(clamped_speed_limit(0), 0);
        assert_eq!(clamped_speed_limit(1024), 1024);
        // The two conversions that used to overflow: the status bar's `* 1024`
        // and the engine's cast to i64 for the token bucket.
        let capped = clamped_speed_limit(u64::MAX);
        assert!(capped.checked_mul(1024).is_some());
        assert!(i64::try_from(capped.saturating_mul(1024)).is_ok());
    }

    #[tokio::test]
    async fn pause_all_pauses_when_a_throttled_torrent_looks_paused() {
        // A throttle-managed torrent reports Paused mid duty cycle. Counting
        // that as "paused" made `P` send ResumeAll when the user asked to
        // pause everything.
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.handle_state_push(vec![
            torrent(0, types::TorrentStatus::Downloading, true),
            torrent(1, types::TorrentStatus::Paused, true),
        ]);
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('P')), &tx).await;
        assert!(matches!(
            rx.try_recv().expect("a command"),
            EngineCommand::PauseAll
        ));
    }

    #[tokio::test]
    async fn pause_all_resumes_only_when_everything_is_user_paused() {
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.handle_state_push(vec![
            torrent(0, types::TorrentStatus::Paused, false),
            torrent(1, types::TorrentStatus::Paused, false),
        ]);
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('P')), &tx).await;
        assert!(matches!(
            rx.try_recv().expect("a command"),
            EngineCommand::ResumeAll
        ));
    }

    #[tokio::test]
    async fn pause_all_on_an_idle_session_does_not_resume() {
        // `.all()` on an empty iterator is true, which used to make `P` send
        // ResumeAll with nothing to resume.
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('P')), &tx).await;
        assert!(matches!(
            rx.try_recv().expect("a command"),
            EngineCommand::PauseAll
        ));
    }

    #[tokio::test]
    async fn p_pauses_a_seeding_torrent() {
        // Was a silent no-op: the match fell through to `_ => {}`, so a single
        // seeding torrent could not be stopped even though mark+p and P both
        // paused it.
        for status in [
            types::TorrentStatus::Seeding,
            types::TorrentStatus::Complete,
        ] {
            let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
            let mut app = App::new();
            let mut iw = InputWidget::new();
            app.handle_state_push(vec![torrent(0, status.clone(), false)]);
            handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('p')), &tx).await;
            assert!(
                matches!(rx.try_recv().expect("a command"), EngineCommand::Pause(0)),
                "{status:?} should pause"
            );
        }
    }

    #[test]
    fn log_is_appended_not_truncated() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut f = open_log_file(dir.path()).unwrap();
            writeln!(f, "first run").unwrap();
        }
        {
            let mut f = open_log_file(dir.path()).unwrap();
            writeln!(f, "second run").unwrap();
        }
        let contents = read_log(dir.path());
        // The whole point of #42: the earlier run survives the relaunch.
        assert!(contents.contains("first run"), "{contents:?}");
        assert!(contents.contains("second run"), "{contents:?}");
    }

    #[test]
    fn log_is_created_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let f = open_log_file(dir.path()).unwrap();
        drop(f);
        assert!(dir.path().join("torrenttui.log").exists());
        assert!(!dir.path().join("torrenttui.log.1").exists());
    }

    #[test]
    fn oversized_log_is_rotated_on_open() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("torrenttui.log"),
            vec![b'x'; (MAX_LOG_BYTES + 1) as usize],
        )
        .unwrap();

        let mut f = open_log_file(dir.path()).unwrap();
        writeln!(f, "fresh run").unwrap();
        drop(f);

        let rotated = dir.path().join("torrenttui.log.1");
        assert!(rotated.exists(), "previous generation should be kept");
        assert_eq!(
            std::fs::metadata(&rotated).unwrap().len(),
            MAX_LOG_BYTES + 1
        );
        // The live log restarts from the rotation, not from 5 MB of history.
        let contents = read_log(dir.path());
        assert_eq!(contents, "fresh run\n");
    }

    #[test]
    fn log_under_the_cap_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("torrenttui.log"), b"keep me\n").unwrap();

        let mut f = open_log_file(dir.path()).unwrap();
        writeln!(f, "and me").unwrap();
        drop(f);

        assert!(!dir.path().join("torrenttui.log.1").exists());
        assert_eq!(read_log(dir.path()), "keep me\nand me\n");
    }

    #[test]
    fn rotation_replaces_the_older_generation() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("torrenttui.log.1"), b"ancient\n").unwrap();
        std::fs::write(
            dir.path().join("torrenttui.log"),
            vec![b'y'; (MAX_LOG_BYTES + 1) as usize],
        )
        .unwrap();

        drop(open_log_file(dir.path()).unwrap());

        // Only one previous generation is kept, so `ancient` is gone.
        let rotated = std::fs::read(dir.path().join("torrenttui.log.1")).unwrap();
        assert_eq!(rotated.len(), (MAX_LOG_BYTES + 1) as usize);
    }

    #[test]
    fn help_mode_esc_returns_to_normal() {
        let mut app = App::new();
        app.mode = AppMode::Help;
        handle_help_mode(&mut app, key(KeyCode::Esc));
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn help_mode_ignores_other_keys() {
        let mut app = App::new();
        app.mode = AppMode::Help;
        handle_help_mode(&mut app, key(KeyCode::Char('x')));
        assert_eq!(app.mode, AppMode::Help);
    }

    #[test]
    fn quit_mode_y_sets_should_quit() {
        let mut app = App::new();
        app.mode = AppMode::ConfirmQuit;
        handle_quit_mode(&mut app, key(KeyCode::Char('y')));
        assert!(app.should_quit);
    }

    #[test]
    fn quit_mode_n_cancels_without_quitting() {
        let mut app = App::new();
        app.mode = AppMode::ConfirmQuit;
        handle_quit_mode(&mut app, key(KeyCode::Char('n')));
        assert!(!app.should_quit);
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn filter_mode_typing_backspace_and_esc() {
        let mut app = App::new();
        app.mode = AppMode::Filter;
        handle_filter_mode(&mut app, key(KeyCode::Char('a')));
        handle_filter_mode(&mut app, key(KeyCode::Char('b')));
        assert_eq!(app.filter_text, "ab");
        handle_filter_mode(&mut app, key(KeyCode::Backspace));
        assert_eq!(app.filter_text, "a");
        handle_filter_mode(&mut app, key(KeyCode::Esc));
        assert_eq!(app.mode, AppMode::Normal);
        assert_eq!(app.filter_text, ""); // Esc clears the filter
    }

    fn torrent(id: usize, status: types::TorrentStatus, managed: bool) -> types::TorrentInfo {
        types::TorrentInfo {
            id,
            name: format!("t{id}"),
            group: None,
            size_bytes: 100,
            downloaded_bytes: 10,
            uploaded_bytes: 0,
            download_speed: 0,
            upload_speed: 0,
            peers_connected: 0,
            peers_total: 0,
            status,
            eta_seconds: None,
            files: Vec::new(),
            peers: Vec::new(),
            info_hash: String::new(),
            trackers: Vec::new(),
            piece_length: None,
            throttle_managed: managed,
        }
    }

    #[tokio::test]
    async fn bulk_pause_under_a_speed_limit_pauses_rather_than_resumes() {
        // #47: throttle-managed torrents used to count as "paused", so `p` on a
        // set of actively downloading torrents sent ResumeMany and nothing
        // appeared to happen. Being managed must not imply paused.
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.handle_state_push(vec![
            torrent(0, types::TorrentStatus::Downloading, true),
            torrent(1, types::TorrentStatus::Downloading, true),
        ]);
        app.marked_ids.insert(0);
        app.marked_ids.insert(1);
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('p')), &tx).await;
        match rx.try_recv().expect("a command should have been sent") {
            EngineCommand::PauseMany(ids) => assert_eq!(ids.len(), 2),
            other => panic!("expected PauseMany, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bulk_pause_resumes_when_something_is_genuinely_user_paused() {
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.handle_state_push(vec![
            torrent(0, types::TorrentStatus::Downloading, true),
            // User-paused: a real pause clears throttle management.
            torrent(1, types::TorrentStatus::Paused, false),
        ]);
        app.marked_ids.insert(0);
        app.marked_ids.insert(1);
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('p')), &tx).await;
        match rx.try_recv().expect("a command should have been sent") {
            EngineCommand::ResumeMany(ids) => assert_eq!(ids.len(), 2),
            other => panic!("expected ResumeMany, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn single_pause_resumes_only_a_user_paused_torrent() {
        // Paused + managed is mid duty cycle: `p` there means "stop it
        // properly", not "hand it back to the throttle".
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.handle_state_push(vec![torrent(0, types::TorrentStatus::Paused, true)]);
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('p')), &tx).await;
        assert!(matches!(
            rx.try_recv().expect("a command"),
            EngineCommand::Pause(0)
        ));

        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        app.handle_state_push(vec![torrent(0, types::TorrentStatus::Paused, false)]);
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('p')), &tx).await;
        assert!(matches!(
            rx.try_recv().expect("a command"),
            EngineCommand::Resume(0)
        ));
    }

    #[tokio::test]
    async fn normal_mode_a_opens_input() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('a')), &tx).await;
        assert_eq!(app.mode, AppMode::Input);
    }

    #[tokio::test]
    async fn normal_mode_switch_keys_change_mode() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut iw = InputWidget::new();

        let mut app = App::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('?')), &tx).await;
        assert_eq!(app.mode, AppMode::Help);

        let mut app = App::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('/')), &tx).await;
        assert_eq!(app.mode, AppMode::Filter);

        let mut app = App::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('t')), &tx).await;
        assert_eq!(app.mode, AppMode::ThrottleInput);
    }

    #[tokio::test]
    async fn normal_mode_t_cycles_themes_and_wraps() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();

        // Startup state: the "config" slot holds the resolved config theme.
        assert_eq!(app.theme_name, "config");
        assert_eq!(app.theme, app.config_theme);

        // First press moves to the first preset and announces it.
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('T')), &tx).await;
        assert_eq!(app.theme_name, "default");
        assert_eq!(app.theme, crate::ui::theme::Theme::default());
        assert_eq!(app.info_message.as_deref(), Some("Theme: default"));

        // Walk the rest of the presets.
        for name in &crate::ui::theme::Theme::PRESET_NAMES[1..] {
            handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('T')), &tx).await;
            assert_eq!(&app.theme_name, name);
            assert_eq!(app.theme, crate::ui::theme::Theme::preset(name).unwrap());
        }

        // Past the last preset the rotation wraps back to the config slot,
        // restoring any per-key overrides the session started with.
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('T')), &tx).await;
        assert_eq!(app.theme_name, "config");
        assert_eq!(app.theme, app.config_theme);
    }

    #[tokio::test]
    async fn normal_mode_t_restores_config_overrides() {
        // A config theme with a per-key override must survive a full cycle —
        // that is the whole point of the "config" slot.
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.config_theme = crate::ui::theme::Theme::preset("dracula").unwrap();
        app.config_theme.accent = ratatui::style::Color::Red;
        app.theme = app.config_theme.clone();
        let overridden = app.config_theme.clone();

        let slots = crate::ui::theme::Theme::PRESET_NAMES.len() + 1;
        for _ in 0..slots {
            handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('T')), &tx).await;
        }
        assert_eq!(app.theme_name, "config");
        assert_eq!(app.theme, overridden);
        assert_eq!(app.theme.accent, ratatui::style::Color::Red);
    }

    #[test]
    fn stream_keypress_without_api_sets_error() {
        let mut app = App::new();
        assert!(app.http_api_base.is_none());
        handle_stream_keypress(&mut app);
        assert_eq!(
            app.error_message.as_deref(),
            Some("Streaming API not ready yet")
        );
    }

    fn search_result(title: &str, magnet: &str) -> search::SearchResult {
        search::SearchResult {
            title: title.to_string(),
            magnet: magnet.to_string(),
            size_bytes: 1024,
            seeders: 10,
            leechers: 2,
            provider: "TPB".to_string(),
        }
    }

    #[tokio::test]
    async fn normal_mode_s_opens_search_input() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('s')), &tx).await;
        assert_eq!(app.mode, AppMode::SearchInput);
        assert_eq!(iw.value(), "");
    }

    #[test]
    fn search_input_enter_with_text_starts_search() {
        // The handler returns the query for run_app to spawn `run_search`
        // with — the spawn itself is the thin, untested shell.
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.mode = AppMode::SearchInput;
        iw.push_str("ubuntu");
        let query = handle_search_input_mode(&mut app, &mut iw, key(KeyCode::Enter));
        assert_eq!(query.as_deref(), Some("ubuntu"));
        assert!(app.search_in_flight);
        assert_eq!(app.search_query, "ubuntu");
        // The results view opens immediately so the spinner has somewhere to
        // render while the providers answer.
        assert_eq!(app.mode, AppMode::SearchResults);
        assert!(app.search_results.is_empty());
    }

    #[test]
    fn search_input_enter_on_empty_query_stays_put() {
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.mode = AppMode::SearchInput;
        iw.push_str("   ");
        let query = handle_search_input_mode(&mut app, &mut iw, key(KeyCode::Enter));
        assert_eq!(query, None);
        assert_eq!(app.mode, AppMode::SearchInput);
        assert!(!app.search_in_flight);
    }

    #[test]
    fn search_input_typing_and_esc() {
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.mode = AppMode::SearchInput;
        handle_search_input_mode(&mut app, &mut iw, key(KeyCode::Char('a')));
        handle_search_input_mode(&mut app, &mut iw, key(KeyCode::Char('b')));
        handle_search_input_mode(&mut app, &mut iw, key(KeyCode::Backspace));
        assert_eq!(iw.value(), "a");
        handle_search_input_mode(&mut app, &mut iw, key(KeyCode::Esc));
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn search_response_populates_results_and_leaves_in_flight() {
        let mut app = App::new();
        app.begin_search("x".to_string());
        app.apply_search_response(search::SearchResponse {
            query: "x".to_string(),
            results: vec![search_result("A", "magnet:?xt=urn:btih:aaa")],
            errors: Vec::new(),
        });
        assert!(!app.search_in_flight);
        assert_eq!(app.mode, AppMode::SearchResults);
        assert_eq!(app.search_results.len(), 1);
        assert_eq!(
            app.selected_search_result().unwrap().magnet,
            "magnet:?xt=urn:btih:aaa"
        );
    }

    #[test]
    fn search_response_with_no_hits_shows_a_message() {
        let mut app = App::new();
        app.begin_search("nothing".to_string());
        app.apply_search_response(search::SearchResponse {
            query: "nothing".to_string(),
            results: Vec::new(),
            errors: Vec::new(),
        });
        assert_eq!(
            app.info_message.as_deref(),
            Some("No results for 'nothing'")
        );
    }

    #[test]
    fn search_response_provider_errors_are_summarized() {
        let mut app = App::new();
        app.begin_search("x".to_string());
        app.apply_search_response(search::SearchResponse {
            query: "x".to_string(),
            results: vec![search_result("A", "magnet:?xt=urn:btih:aaa")],
            errors: vec![("TPB".to_string(), "connection timed out".to_string())],
        });
        let err = app.error_message.as_deref().expect("an error message");
        assert!(err.contains("TPB"), "{err:?}");
        assert!(err.contains("connection timed out"), "{err:?}");
        // Partial failure still keeps the hits that did arrive.
        assert_eq!(app.search_results.len(), 1);
    }

    #[test]
    fn search_response_after_esc_does_not_yank_the_view_back() {
        // User bailed to Normal mid-search: the hits still land, but the UI
        // stays where they put it.
        let mut app = App::new();
        app.begin_search("x".to_string());
        app.mode = AppMode::Normal;
        app.apply_search_response(search::SearchResponse {
            query: "x".to_string(),
            results: vec![search_result("A", "magnet:?xt=urn:btih:aaa")],
            errors: Vec::new(),
        });
        assert_eq!(app.mode, AppMode::Normal);
        assert!(!app.search_in_flight);
        assert_eq!(app.search_results.len(), 1);
    }

    #[tokio::test]
    async fn search_results_enter_adds_the_selected_magnet() {
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.begin_search("x".to_string());
        app.apply_search_response(search::SearchResponse {
            query: "x".to_string(),
            results: vec![
                search_result("First", "magnet:?xt=urn:btih:first"),
                search_result("Second", "magnet:?xt=urn:btih:second"),
            ],
            errors: Vec::new(),
        });
        app.search_next();
        handle_search_results_mode(&mut app, &mut iw, key(KeyCode::Enter), &tx).await;
        match rx.try_recv().expect("a command should have been sent") {
            EngineCommand::AddTorrent { source, group } => {
                assert_eq!(source, "magnet:?xt=urn:btih:second");
                assert_eq!(group, None);
            }
            other => panic!("expected AddTorrent, got {other:?}"),
        }
        assert_eq!(app.info_message.as_deref(), Some("Adding: Second\u{2026}"));
        // Stays in the results view so several hits can be queued.
        assert_eq!(app.mode, AppMode::SearchResults);
    }

    #[tokio::test]
    async fn search_results_enter_with_a_group_tags_the_add() {
        let (tx, mut rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.group_names = vec!["linux".to_string()];
        app.begin_search("x".to_string());
        app.apply_search_response(search::SearchResponse {
            query: "x".to_string(),
            results: vec![search_result("Ubuntu", "magnet:?xt=urn:btih:ubuntu")],
            errors: Vec::new(),
        });
        handle_search_results_mode(&mut app, &mut iw, key(KeyCode::Char('g')), &tx).await;
        assert_eq!(app.search_group.as_deref(), Some("linux"));
        handle_search_results_mode(&mut app, &mut iw, key(KeyCode::Enter), &tx).await;
        match rx.try_recv().expect("a command should have been sent") {
            EngineCommand::AddTorrent { source, group } => {
                assert_eq!(source, "magnet:?xt=urn:btih:ubuntu");
                assert_eq!(group.as_deref(), Some("linux"));
            }
            other => panic!("expected AddTorrent, got {other:?}"),
        }
        assert_eq!(
            app.info_message.as_deref(),
            Some("Adding: Ubuntu \u{2192} linux\u{2026}")
        );
    }

    #[tokio::test]
    async fn search_results_esc_returns_to_normal() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.mode = AppMode::SearchResults;
        handle_search_results_mode(&mut app, &mut iw, key(KeyCode::Esc), &tx).await;
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[tokio::test]
    async fn search_results_s_reopens_the_prompt_with_the_query_prefilled() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.begin_search("ubuntu".to_string());
        handle_search_results_mode(&mut app, &mut iw, key(KeyCode::Char('s')), &tx).await;
        assert_eq!(app.mode, AppMode::SearchInput);
        assert_eq!(iw.value(), "ubuntu");
    }

    #[tokio::test]
    async fn search_results_navigation_clamps_at_both_ends() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.begin_search("x".to_string());
        app.apply_search_response(search::SearchResponse {
            query: "x".to_string(),
            results: vec![
                search_result("First", "magnet:?xt=urn:btih:first"),
                search_result("Second", "magnet:?xt=urn:btih:second"),
            ],
            errors: Vec::new(),
        });
        handle_search_results_mode(&mut app, &mut iw, key(KeyCode::Char('k')), &tx).await;
        assert_eq!(app.search_index, 0);
        handle_search_results_mode(&mut app, &mut iw, key(KeyCode::Char('j')), &tx).await;
        handle_search_results_mode(&mut app, &mut iw, key(KeyCode::Char('j')), &tx).await;
        assert_eq!(app.search_index, 1);
        assert_eq!(app.search_state.selected(), Some(1));
    }

    /// An app mid-split: pane open, split on screen, two downloads and two
    /// search hits to navigate. Handlers gate focus routing on
    /// `split_active`, which is normally set by the render dispatch — tests
    /// set it by hand.
    fn app_in_split() -> (
        mpsc::Sender<EngineCommand>,
        mpsc::Receiver<EngineCommand>,
        App,
        InputWidget,
    ) {
        let (tx, rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        app.handle_state_push(vec![
            torrent(0, types::TorrentStatus::Downloading, false),
            torrent(1, types::TorrentStatus::Downloading, false),
        ]);
        app.open_search_pane();
        app.split_active = true;
        app.begin_search("x".to_string());
        app.apply_search_response(search::SearchResponse {
            query: "x".to_string(),
            results: vec![
                search_result("First", "magnet:?xt=urn:btih:first"),
                search_result("Second", "magnet:?xt=urn:btih:second"),
            ],
            errors: Vec::new(),
        });
        app.mode = AppMode::Normal;
        (tx, rx, app, InputWidget::new())
    }

    #[tokio::test]
    async fn normal_mode_s_opens_the_search_pane() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('s')), &tx).await;
        assert!(app.search_pane_open);
        assert_eq!(app.focused, Pane::Search);
        assert_eq!(app.mode, AppMode::SearchInput);
        assert_eq!(iw.value(), "");
    }

    #[tokio::test]
    async fn normal_mode_s_with_pane_open_refocuses_and_prefills() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let (.., mut app, mut iw) = app_in_split();
        app.focused = Pane::Downloads;
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('s')), &tx).await;
        assert!(app.search_pane_open);
        assert_eq!(app.focused, Pane::Search);
        assert_eq!(app.mode, AppMode::SearchInput);
        assert_eq!(iw.value(), "x");
    }

    #[tokio::test]
    async fn w_cycles_focus_only_with_the_pane_open() {
        let (tx, _rx) = mpsc::channel::<EngineCommand>(8);
        let mut app = App::new();
        let mut iw = InputWidget::new();
        // Pane closed: `w` is inert.
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('w')), &tx).await;
        assert_eq!(app.focused, Pane::Downloads);

        let (tx, _rx, mut app, mut iw) = app_in_split();
        app.focused = Pane::Downloads;
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('w')), &tx).await;
        assert_eq!(app.focused, Pane::Search);
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('w')), &tx).await;
        assert_eq!(app.focused, Pane::Downloads);
        // Focus switching never changes the mode.
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[tokio::test]
    async fn pipe_toggles_the_split_orientation() {
        let (tx, _rx, mut app, mut iw) = app_in_split();
        assert!(app.split_vertical);
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('|')), &tx).await;
        assert!(!app.split_vertical);
    }

    #[tokio::test]
    async fn focused_search_pane_owns_jk_and_enter() {
        let (tx, mut rx, mut app, mut iw) = app_in_split();
        // `j` moves the results cursor, not the downloads selection.
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('j')), &tx).await;
        assert_eq!(app.search_index, 1);
        assert_eq!(app.selected_index, 0);
        // Enter adds the selected hit, tagged with the search group.
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Enter), &tx).await;
        match rx.try_recv().expect("a command should have been sent") {
            EngineCommand::AddTorrent { source, .. } => {
                assert_eq!(source, "magnet:?xt=urn:btih:second")
            }
            other => panic!("expected AddTorrent, got {other:?}"),
        }
        // `g` cycles the add-target group on the focused pane.
        app.group_names = vec!["linux".to_string()];
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('g')), &tx).await;
        assert_eq!(app.search_group.as_deref(), Some("linux"));
    }

    #[tokio::test]
    async fn focused_downloads_pane_keeps_the_usual_keys() {
        let (tx, _rx, mut app, mut iw) = app_in_split();
        app.focused = Pane::Downloads;
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('j')), &tx).await;
        assert_eq!(app.selected_index, 1);
        assert_eq!(app.search_index, 0);
        // Enter on downloads opens Detail, as always.
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Enter), &tx).await;
        assert_eq!(app.mode, AppMode::Detail);
    }

    #[tokio::test]
    async fn hidden_pane_does_not_steal_navigation() {
        // Pane open but the split didn't fit (small terminal): j/k must stay
        // on the downloads table the user can actually see.
        let (tx, _rx, mut app, mut iw) = app_in_split();
        app.split_active = false;
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Char('j')), &tx).await;
        assert_eq!(app.selected_index, 1);
        assert_eq!(app.search_index, 0);
    }

    #[test]
    fn search_input_esc_keeps_the_pane_open() {
        let mut app = App::new();
        let mut iw = InputWidget::new();
        app.open_search_pane();
        handle_search_input_mode(&mut app, &mut iw, key(KeyCode::Esc));
        assert_eq!(app.mode, AppMode::Normal);
        assert!(app.search_pane_open);
        assert_eq!(app.focused, Pane::Search);
    }

    #[tokio::test]
    async fn esc_on_focused_search_pane_closes_it() {
        // The second Esc of the sequence: Normal mode, focus Search, split
        // on screen — this one closes the pane.
        let (tx, _rx, mut app, mut iw) = app_in_split();
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Esc), &tx).await;
        assert!(!app.search_pane_open);
        assert_eq!(app.focused, Pane::Downloads);
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[tokio::test]
    async fn esc_on_focused_downloads_still_clears_marks() {
        let (tx, _rx, mut app, mut iw) = app_in_split();
        app.focused = Pane::Downloads;
        app.marked_ids.insert(0);
        handle_normal_mode(&mut app, &mut iw, key(KeyCode::Esc), &tx).await;
        assert!(!app.has_marks());
        assert!(app.search_pane_open);
    }
}
