//! UI-side application state and the operations key handlers perform on it.
//!
//! `App` deliberately holds no engine handle: everything it knows about
//! torrents arrives as a snapshot over the state channel, and everything it
//! wants done leaves as an `EngineCommand`. That makes it trivially
//! constructible in tests and keeps any render path from blocking on the
//! session.
//!
//! The recurring pattern here is the sort cache. `sorted_torrents()` is called
//! several times per frame, and lowercasing names for the filter plus sorting
//! dominated CPU at hundreds of torrents. Anything that changes the filter, the
//! sort, or the torrent list must therefore invalidate the cache, rebuild it,
//! and then restore the selection — which is why `change_sort_column`,
//! `push_filter_char` and friends exist as wrappers instead of callers poking
//! the fields directly.
//!
//! `torrents` is replaced wholesale on every push, so state keyed by torrent id
//! (`marked_ids`, `deselected_files`) can outlive the torrent it refers to.
//! `prune_stale_state` is what stops that accumulating, and `handle_state_push`
//! is the one entry point that runs it.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::config::PlayerConfig;
use crate::search::{SearchResponse, SearchResult};
use crate::types::{AppMode, DetailTab, Pane, SortColumn, TorrentInfo, TorrentStatus};
use crate::ui::theme::Theme;
use ratatui::widgets::TableState;

/// Depth of the header-sparkline speed histories. At the engine's 100 ms
/// push cadence this is ~24 s of download/upload history.
pub const SPEED_HISTORY_CAPACITY: usize = 240;

/// Cap on tracked in-flight adds. Eight concurrent "Adding…" rows already
/// overflow a small terminal; beyond that the oldest placeholder is dropped
/// (the engine's msg_tx confirmation still reports every outcome).
pub const PENDING_ADDS_CAPACITY: usize = 8;

/// Append a speed sample to a history, dropping the oldest sample when full.
/// A free function (not a method) so the capping logic is testable without
/// constructing an `App`.
pub(crate) fn push_speed_sample(history: &mut VecDeque<u64>, sample: u64) {
    if history.len() >= SPEED_HISTORY_CAPACITY {
        history.pop_front();
    }
    history.push_back(sample);
}

/// All UI-side state. Constructed once in `run_app`; every field is either
/// derived from an engine snapshot or owned by a key handler.
pub struct App {
    pub torrents: Vec<TorrentInfo>,
    pub table_state: TableState,
    pub selected_index: usize,
    pub selected_torrent_id: Option<usize>,
    pub mode: AppMode,
    pub detail_tab: DetailTab,
    pub sort_column: SortColumn,
    pub sort_reversed: bool,
    pub error_message: Option<String>,
    pub error_timer: Option<std::time::Instant>,
    pub info_message: Option<String>,
    pub info_timer: Option<std::time::Instant>,
    pub spinner_tick: usize,
    pub should_quit: bool,
    pub filter_text: String,
    pub free_disk_space: Option<u64>,
    pub disk_space_timer: Option<std::time::Instant>,
    pub throttle_step: u8, // 0 = download, 1 = upload
    pub throttle_input_buf: String,
    pub throttle_download_value: u64,
    pub throttle_upload_value: u64,
    pub speed_limit_download_kbps: u64,
    pub speed_limit_upload_kbps: u64,
    /// Where the torrent table was drawn last frame, so mouse clicks can be
    /// mapped back to rows. Set by the renderer and cleared to `None` in
    /// Detail mode, which is what makes clicks inert there.
    pub table_area: Option<ratatui::layout::Rect>,
    pub detail_file_index: usize,
    /// Per torrent id, the file indices the user has *deselected*. Stored
    /// negatively so a torrent whose metadata has not arrived yet — and which
    /// therefore has no known file count — still defaults to "download
    /// everything". Pruned by `prune_stale_state`.
    pub deselected_files: HashMap<usize, HashSet<usize>>,
    pub marked_ids: HashSet<usize>,
    pub detail_peer_index: usize,
    /// Top peer-row index currently visible in the Peers tab. The renderer
    /// recomputes it each frame to keep `detail_peer_index` in view and to
    /// clamp it when the peer list shrinks; the key handlers reset it to 0
    /// whenever the Peers list is re-entered (opening Detail, switching tab).
    pub detail_peer_scroll_offset: usize,
    /// Cached order of indices into `self.torrents` after applying the
    /// current filter and sort. Rebuilt when `sort_dirty` is true. The cache
    /// exists because `sorted_torrents()` is called multiple times per frame
    /// (table, detail, key handlers) and the lowercase-for-filter + sort
    /// work dominated CPU at hundreds of torrents.
    sort_cache: Vec<usize>,
    sort_dirty: bool,
    /// Wired from `config.general.confirm_on_quit`; when false, `q` quits
    /// immediately instead of opening the confirmation dialog.
    pub confirm_on_quit: bool,
    /// Base URL of the engine's embedded HTTP API (e.g.
    /// `http://127.0.0.1:34567`).
    /// `None` until the engine reports `EngineInfo::HttpApiReady`; if the
    /// bind failed at startup, stays `None` forever and the `s` stream
    /// keybinding shows an error.
    pub http_api_base: Option<String>,
    /// External player command + args for the `s` stream keybinding. Loaded
    /// once from `config.player` at startup.
    pub player_config: PlayerConfig,
    /// Name last seen behind each torrent id, so `prune_stale_state` can tell
    /// "same torrent" from "recycled id". librqbit hands out `max(ids) + 1`, so
    /// a freed id can be reused by a concurrent add.
    torrent_identities: HashMap<usize, String>,
    /// Wired from `config.general.watch_dir.is_some()`. Only affects the
    /// delete dialog, which warns that deleting also removes the `.torrent`
    /// from the watch folder.
    pub watch_dir_configured: bool,
    /// The palette every renderer draws with. Wired from `config.theme` at
    /// startup; `Theme::default()` here is just the placeholder. `T` swaps it
    /// for the next entry in the theme rotation.
    pub theme: Theme,
    /// The resolved config theme (preset + any per-key overrides), kept so
    /// `T` can cycle back to it — cycling to a preset and back would
    /// otherwise lose the user's overrides for the rest of the session.
    pub config_theme: Theme,
    /// Name of the active theme slot for the status message: `"config"` for
    /// `config_theme`, otherwise one of `Theme::PRESET_NAMES`. Session-only —
    /// the config file decides what launches next time.
    pub theme_name: String,
    /// The query behind the current search flow — shown in the results-view
    /// title and pre-filled when `s` reopens the search prompt.
    pub search_query: String,
    /// Hits from the last completed search, in the order `run_search` merged
    /// them (seeders desc, deduped). Cleared when a new search starts.
    pub search_results: Vec<SearchResult>,
    /// Cursor into `search_results`, kept in lockstep with `search_index`.
    pub search_state: TableState,
    pub search_index: usize,
    /// True from the moment a query is dispatched until its `SearchResponse`
    /// arrives; the results view shows the spinner while this is set.
    pub search_in_flight: bool,
    /// Names of the configured `[[groups]]`, poked in from the config at
    /// startup. The results view's `g` key cycles `search_group` through
    /// `[None] + group_names`.
    pub group_names: Vec<String>,
    /// Group that adds from the search results view are routed into; `None`
    /// means "plain add into the download dir". Sticky across searches within
    /// a session — dropping several hits for one topic into its folder is the
    /// headline flow.
    pub search_group: Option<String>,
    /// Aggregate download/upload speed samples (bytes/sec, summed across all
    /// torrents), newest last, capped at `SPEED_HISTORY_CAPACITY`. Fed by
    /// `handle_state_push`; drawn as the two header sparklines.
    pub down_history: VecDeque<u64>,
    pub up_history: VecDeque<u64>,
    /// Whether the search pane shares the main area with the downloads table.
    /// Set by `open_search_pane` / `close_search_pane`; on a terminal too
    /// small for the split this can be true while nothing pane-like is drawn
    /// (see `split_active`).
    pub search_pane_open: bool,
    /// Which pane owns the routed keys (`j`/`k`/`Enter`/`g`). Defaults to
    /// Downloads; `open_search_pane` moves it to Search.
    pub focused: Pane,
    /// Split orientation: true docks the search pane on the right (downloads
    /// left), false stacks it below. Seeded from `config.ui.search_split`,
    /// flipped at runtime by `|`. Session-only — never written back.
    pub split_vertical: bool,
    /// True only while the split layout is actually on screen: the pane is
    /// open *and* the terminal cleared `use_split_layout`'s thresholds on the
    /// last rendered frame. Set by the render dispatch in main.rs (same
    /// pattern as `table_area`), so key handlers can gate focus routing on it
    /// — a hidden pane on a small terminal must not steal `j`/`k` from the
    /// downloads table. Tests that exercise pane routing set it by hand.
    pub split_active: bool,
    /// Adds dispatched to the engine that have not reported back yet, oldest
    /// first (titles/sources exactly as given at dispatch). The metadata
    /// fetch behind a magnet can take up to the engine's add timeout, during
    /// which the "Adding: …" toast has long expired — these entries are what
    /// the downloads table renders as spinner rows so an in-flight add stays
    /// visible. Pushed by the dispatch sites, popped one per
    /// `EngineInfo::AddFinished`; capped at `PENDING_ADDS_CAPACITY`.
    pub pending_adds: VecDeque<String>,
}

impl App {
    /// A blank app with row 0 selected and the sort cache marked dirty.
    ///
    /// The config-derived fields (speed limits, `confirm_on_quit`,
    /// `player_config`, `watch_dir_configured`, `theme`, `config_theme`,
    /// `theme_name`) get placeholder values here and are overwritten by
    /// `run_app` immediately afterwards — do not read the real defaults off
    /// this function.
    pub fn new() -> Self {
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        Self {
            torrents: Vec::new(),
            table_state,
            selected_index: 0,
            selected_torrent_id: None,
            mode: AppMode::Normal,
            detail_tab: DetailTab::Stats,
            sort_column: SortColumn::Index,
            sort_reversed: false,
            error_message: None,
            error_timer: None,
            info_message: None,
            info_timer: None,
            spinner_tick: 0,
            should_quit: false,
            filter_text: String::new(),
            free_disk_space: None,
            disk_space_timer: None,
            throttle_step: 0,
            throttle_input_buf: String::new(),
            throttle_download_value: 0,
            throttle_upload_value: 0,
            speed_limit_download_kbps: 0,
            speed_limit_upload_kbps: 0,
            table_area: None,
            detail_file_index: 0,
            deselected_files: HashMap::new(),
            marked_ids: HashSet::new(),
            detail_peer_index: 0,
            detail_peer_scroll_offset: 0,
            sort_cache: Vec::new(),
            sort_dirty: true,
            confirm_on_quit: true,
            http_api_base: None,
            player_config: PlayerConfig::default(),
            torrent_identities: HashMap::new(),
            watch_dir_configured: false,
            theme: Theme::default(),
            config_theme: Theme::default(),
            theme_name: "config".to_string(),
            search_query: String::new(),
            search_results: Vec::new(),
            search_state: {
                let mut state = TableState::default();
                state.select(Some(0));
                state
            },
            search_index: 0,
            search_in_flight: false,
            group_names: Vec::new(),
            search_group: None,
            down_history: VecDeque::new(),
            up_history: VecDeque::new(),
            search_pane_open: false,
            focused: Pane::Downloads,
            split_vertical: true,
            split_active: false,
            pending_adds: VecDeque::new(),
        }
    }

    /// Whether `q` should open the confirmation dialog instead of quitting
    /// outright. True for anything mid-work: downloading, resolving metadata,
    /// or actively seeding. Seeding counts because quitting still cuts peers
    /// abruptly, even though nothing is being downloaded.
    pub fn confirm_on_quit_required(&self) -> bool {
        self.confirm_on_quit
            && self.torrents.iter().any(|t| {
                matches!(
                    t.status,
                    TorrentStatus::Downloading
                        | TorrentStatus::FetchingMetadata
                        | TorrentStatus::Seeding
                )
            })
    }

    /// Mark the cached sort order as dirty. Call from anywhere that mutates
    /// `torrents`, `sort_column`, `sort_reversed`, or `filter_text`.
    pub fn invalidate_sort(&mut self) {
        self.sort_dirty = true;
    }

    /// Replace the torrent list and refresh derived state (sort cache,
    /// selection bookkeeping, pruned marks). One canonical entry point so
    /// callers can't forget to invalidate.
    /// Returns whether anything the UI draws actually changed. The engine
    /// pushes a snapshot every 100 ms whether or not it differs, so repainting
    /// unconditionally meant a session of idle torrents redrew the terminal ten
    /// times a second forever — and rebuilt the sort cache each time.
    pub fn handle_state_push(&mut self, torrents: Vec<TorrentInfo>) -> bool {
        // Record the speed sample whether or not the visible rows changed —
        // the sparkline history is a time series, not a diff — but keep it
        // out of the change verdict below so an idle session still skips the
        // repaint.
        let down: u64 = torrents.iter().map(|t| t.download_speed).sum();
        let up: u64 = torrents.iter().map(|t| t.upload_speed).sum();
        push_speed_sample(&mut self.down_history, down);
        push_speed_sample(&mut self.up_history, up);

        let changed = Self::render_state_differs(&self.torrents, &torrents);
        self.torrents = torrents;
        if !changed {
            // Same rows, same values: the cached order is still correct and
            // nothing on screen would move.
            return false;
        }
        self.invalidate_sort();
        self.prune_stale_state();
        self.ensure_sort_cache();
        self.restore_selection();
        true
    }

    /// Compare only the fields that reach the screen. Deliberately ignores
    /// `files`/`peers`/`trackers`, which are populated for the Detail torrent
    /// only and are compared by the Detail view's own redraw trigger.
    fn render_state_differs(old: &[TorrentInfo], new: &[TorrentInfo]) -> bool {
        if old.len() != new.len() {
            return true;
        }
        old.iter().zip(new.iter()).any(|(a, b)| {
            a.id != b.id
                || a.name != b.name
                || a.group != b.group
                || a.status != b.status
                || a.size_bytes != b.size_bytes
                || a.downloaded_bytes != b.downloaded_bytes
                || a.uploaded_bytes != b.uploaded_bytes
                || a.download_speed != b.download_speed
                || a.upload_speed != b.upload_speed
                || a.peers_connected != b.peers_connected
                || a.peers_total != b.peers_total
                || a.eta_seconds != b.eta_seconds
                || a.throttle_managed != b.throttle_managed
                || a.files.len() != b.files.len()
                || a.peers.len() != b.peers.len()
        })
    }

    /// Set a new sort column and refresh the cache. Prefer these five wrappers
    /// over assigning `sort_column` / `sort_reversed` / `filter_text` directly:
    /// each has to pair `invalidate_sort` with `ensure_sort_cache` *and*
    /// `restore_selection`, and skipping the last one leaves the user's cursor
    /// on whatever torrent happens to land at the old index.
    pub fn change_sort_column(&mut self, next: SortColumn) {
        self.sort_column = next;
        self.invalidate_sort();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Toggle the sort direction and refresh — see `change_sort_column`.
    pub fn toggle_sort_reversed(&mut self) {
        self.sort_reversed = !self.sort_reversed;
        self.invalidate_sort();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Append a character to the filter and refresh — see `change_sort_column`.
    pub fn push_filter_char(&mut self, c: char) {
        self.filter_text.push(c);
        self.invalidate_sort();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Drop the trailing filter character and refresh — see
    /// `change_sort_column`.
    pub fn pop_filter_char(&mut self) {
        self.filter_text.pop();
        self.invalidate_sort();
        self.ensure_sort_cache();
        self.restore_selection();
    }

    /// Empty the filter and refresh — see `change_sort_column`.
    pub fn clear_filter(&mut self) {
        if !self.filter_text.is_empty() {
            self.filter_text.clear();
            self.invalidate_sort();
            self.ensure_sort_cache();
        }
        self.restore_selection();
    }

    /// Whether a torrent passes the (already lowercased) filter text:
    /// case-insensitive substring on the name *or* the group name, so
    /// `/linux` also finds every torrent grouped under "linux-isos".
    fn matches_filter(t: &TorrentInfo, filter_lower: &str) -> bool {
        filter_lower.is_empty()
            || t.name.to_lowercase().contains(filter_lower)
            || t.group
                .as_deref()
                .is_some_and(|g| g.to_lowercase().contains(filter_lower))
    }

    fn rebuild_sort_cache(&mut self) {
        let filter_lower = self.filter_text.to_lowercase();
        // Collect indices into `self.torrents` after applying the filter.
        let mut indices: Vec<usize> = self
            .torrents
            .iter()
            .enumerate()
            .filter(|(_, t)| Self::matches_filter(t, &filter_lower))
            .map(|(i, _)| i)
            .collect();

        if self.sort_column == SortColumn::Name {
            // Lowercase keys precomputed so each name is lowered once, not
            // O(N log N) times during sort comparisons.
            let lc: Vec<String> = indices
                .iter()
                .map(|&i| self.torrents[i].name.to_lowercase())
                .collect();
            let mut pos: Vec<usize> = (0..indices.len()).collect();
            pos.sort_by(|&a, &b| {
                let cmp = lc[a].cmp(&lc[b]);
                if self.sort_reversed {
                    cmp.reverse()
                } else {
                    cmp
                }
            });
            indices = pos.into_iter().map(|i| indices[i]).collect();
        } else {
            let sort_column = self.sort_column;
            let sort_reversed = self.sort_reversed;
            let torrents = &self.torrents;
            indices.sort_by(|&a, &b| {
                let ta = &torrents[a];
                let tb = &torrents[b];
                let cmp = match sort_column {
                    SortColumn::Index => ta.id.cmp(&tb.id),
                    SortColumn::Name => {
                        // Handled by the early-return above; degrade gracefully.
                        debug_assert!(false, "SortColumn::Name should hit the early return");
                        std::cmp::Ordering::Equal
                    }
                    // Ungrouped sorts first: "" precedes every name byte-wise,
                    // like the 0 values Speed/Peers give empty torrents.
                    SortColumn::Group => ta
                        .group
                        .as_deref()
                        .unwrap_or("")
                        .cmp(tb.group.as_deref().unwrap_or("")),
                    SortColumn::Size => ta.size_bytes.cmp(&tb.size_bytes),
                    SortColumn::Progress => ta
                        .progress_percent()
                        .partial_cmp(&tb.progress_percent())
                        .unwrap_or(std::cmp::Ordering::Equal),
                    SortColumn::Speed => ta.download_speed.cmp(&tb.download_speed),
                    SortColumn::Peers => ta.peers_connected.cmp(&tb.peers_connected),
                    SortColumn::Eta => match (ta.eta_seconds, tb.eta_seconds) {
                        (Some(a_eta), Some(b_eta)) => a_eta.cmp(&b_eta),
                        (Some(_), None) => std::cmp::Ordering::Less,
                        (None, Some(_)) => std::cmp::Ordering::Greater,
                        (None, None) => std::cmp::Ordering::Equal,
                    },
                    SortColumn::Status => ta.status.sort_key().cmp(&tb.status.sort_key()),
                };
                if sort_reversed {
                    cmp.reverse()
                } else {
                    cmp
                }
            });
        }

        self.sort_cache = indices;
        self.sort_dirty = false;
    }

    /// The current filtered and sorted view, borrowed from the cache. Takes
    /// `&self` so render paths can call it, which means it will never rebuild a
    /// cache it finds stale: if `sort_dirty` is set it falls back to a full
    /// live sort, correct but O(N log N) on every call, and this runs several
    /// times per frame.
    ///
    /// If the cache is *not* dirty but `torrents` was mutated behind its back,
    /// indices that no longer resolve are dropped and you get a quietly
    /// shortened list with no error. Mutate through `handle_state_push` (or
    /// pair `invalidate_sort` with `ensure_sort_cache`) and neither happens.
    pub fn sorted_torrents(&self) -> Vec<&TorrentInfo> {
        // Read path: never rebuild. Callers that need a fresh cache must
        // ensure invalidate_sort + ensure_sort_cache ran first; for the
        // hot path this is wrapped by the convenience methods below.
        if self.sort_dirty {
            // Fall back to the live sort when called before the cache has
            // been (re)built. Avoids needing `&mut self` in render paths.
            // This branch is also what tests rely on — they construct an
            // App and immediately call sorted_torrents without a state push.
            return self.sorted_torrents_live();
        }
        self.sort_cache
            .iter()
            .filter_map(|&i| self.torrents.get(i))
            .collect()
    }

    /// Refresh the sort cache if dirty. Call before render paths that need
    /// fresh data. The hot path (run_app) keeps this cheap by invalidating
    /// only when the underlying inputs change.
    pub fn ensure_sort_cache(&mut self) {
        if self.sort_dirty {
            self.rebuild_sort_cache();
        }
    }

    /// Live (uncached) sort. Used as a fallback in `sorted_torrents()` when
    /// the cache is dirty and the caller only has `&self`. Keeps the original
    /// O(N log N) cost but skips the lowercase keys allocation when not Name.
    fn sorted_torrents_live(&self) -> Vec<&TorrentInfo> {
        let filter_lower = self.filter_text.to_lowercase();
        let mut torrents: Vec<&TorrentInfo> = self
            .torrents
            .iter()
            .filter(|t| Self::matches_filter(t, &filter_lower))
            .collect();

        if self.sort_column == SortColumn::Name {
            let lc: Vec<String> = torrents.iter().map(|t| t.name.to_lowercase()).collect();
            let mut indices: Vec<usize> = (0..torrents.len()).collect();
            indices.sort_by(|&a, &b| {
                let cmp = lc[a].cmp(&lc[b]);
                if self.sort_reversed {
                    cmp.reverse()
                } else {
                    cmp
                }
            });
            return indices.into_iter().map(|i| torrents[i]).collect();
        }

        torrents.sort_by(|a, b| {
            let cmp = match self.sort_column {
                SortColumn::Index => a.id.cmp(&b.id),
                SortColumn::Name => {
                    debug_assert!(false, "SortColumn::Name should hit the early return");
                    std::cmp::Ordering::Equal
                }
                // Ungrouped sorts first — see `rebuild_sort_cache`.
                SortColumn::Group => a
                    .group
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.group.as_deref().unwrap_or("")),
                SortColumn::Size => a.size_bytes.cmp(&b.size_bytes),
                SortColumn::Progress => a
                    .progress_percent()
                    .partial_cmp(&b.progress_percent())
                    .unwrap_or(std::cmp::Ordering::Equal),
                SortColumn::Speed => a.download_speed.cmp(&b.download_speed),
                SortColumn::Peers => a.peers_connected.cmp(&b.peers_connected),
                SortColumn::Eta => match (a.eta_seconds, b.eta_seconds) {
                    (Some(a_eta), Some(b_eta)) => a_eta.cmp(&b_eta),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => std::cmp::Ordering::Equal,
                },
                SortColumn::Status => a.status.sort_key().cmp(&b.status.sort_key()),
            };
            if self.sort_reversed {
                cmp.reverse()
            } else {
                cmp
            }
        });

        torrents
    }

    pub fn next(&mut self) {
        let count = self.sorted_torrents().len();
        if count == 0 {
            return;
        }
        self.selected_index = (self.selected_index + 1).min(count - 1);
        self.update_selected_id();
        self.table_state.select(Some(self.selected_index));
    }

    pub fn previous(&mut self) {
        if self.sorted_torrents().is_empty() {
            return;
        }
        self.selected_index = self.selected_index.saturating_sub(1);
        self.update_selected_id();
        self.table_state.select(Some(self.selected_index));
    }

    pub fn update_selected_id(&mut self) {
        let sorted = self.sorted_torrents();
        self.selected_torrent_id = sorted.get(self.selected_index).map(|t| t.id);
    }

    pub fn selected_torrent(&self) -> Option<&TorrentInfo> {
        // Index the sort cache directly instead of materializing the whole
        // `Vec<&TorrentInfo>` just to read one element — this is called several
        // times per frame. Falls back to the live sort when the cache is dirty.
        if self.sort_dirty {
            return self
                .sorted_torrents_live()
                .get(self.selected_index)
                .copied();
        }
        self.sort_cache
            .get(self.selected_index)
            .and_then(|&i| self.torrents.get(i))
    }

    /// Re-anchor the selection after the visible list changed. Follows the
    /// selected torrent by id where it still exists, so re-sorting or filtering
    /// keeps the cursor on the same *torrent* rather than the same row number;
    /// when it is gone, the old index is clamped into the new bounds.
    ///
    /// Reads the sort cache, so run `ensure_sort_cache()` first or you anchor
    /// against the previous ordering. Also refreshes `selected_torrent_id`,
    /// syncs `table_state`, and clamps the detail-view cursors.
    pub fn restore_selection(&mut self) {
        let (new_index, new_id) = {
            let sorted = self.sorted_torrents();
            let len = sorted.len();
            let new_index = if let Some(id) = self.selected_torrent_id {
                if let Some(pos) = sorted.iter().position(|t| t.id == id) {
                    pos
                } else if len > 0 {
                    self.selected_index.min(len - 1)
                } else {
                    0
                }
            } else if len > 0 {
                self.selected_index.min(len - 1)
            } else {
                0
            };
            // Refresh the cached id from the resolved index so callers don't
            // see a stale value between user actions.
            let new_id = sorted.get(new_index).map(|t| t.id);
            (new_index, new_id)
        };
        self.selected_index = new_index;
        self.selected_torrent_id = new_id;
        self.table_state.select(Some(self.selected_index));
        self.clamp_detail_indices();
    }

    /// Clamp detail-view indices against the currently selected torrent's
    /// file/peer counts. Called after every state push so a torrent that
    /// briefly drops to FetchingMetadata (files emptied) or that gains/loses
    /// peers can't leave the cursor pointing past the end of the list.
    pub fn clamp_detail_indices(&mut self) {
        // Extract bounds in a scope so the immutable borrow of `self` is
        // released before we mutate the index fields.
        let bounds = self
            .selected_torrent()
            .map(|t| (t.files.len(), t.peers.len()));
        match bounds {
            Some((file_count, peer_count)) => {
                let file_max = file_count.saturating_sub(1);
                if self.detail_file_index > file_max {
                    self.detail_file_index = file_max;
                }
                let peer_max = peer_count.saturating_sub(1);
                if self.detail_peer_index > peer_max {
                    self.detail_peer_index = peer_max;
                }
            }
            None => {
                self.detail_file_index = 0;
                self.detail_peer_index = 0;
            }
        }
    }

    /// Drop UI-side bookkeeping for torrents that are no longer in the list.
    /// The engine prunes its own maps; this mirrors the same pattern on the UI
    /// side (called from the state-update arm of the main loop).
    pub fn prune_stale_state(&mut self) {
        let current: HashMap<usize, &str> = self
            .torrents
            .iter()
            .map(|t| (t.id, t.name.as_str()))
            .collect();
        // Drop state for ids that are gone, and — because librqbit recycles
        // ids as `max + 1` — also for ids whose torrent has been replaced by a
        // different one. Without the identity check, deleting the highest-id
        // torrent and letting the watch folder add a new one into the freed id
        // silently carried the old file deselections onto the new torrent.
        let identities = &self.torrent_identities;
        self.marked_ids.retain(|id| {
            current
                .get(id)
                .is_some_and(|name| identities.get(id).is_none_or(|prev| prev == name))
        });
        self.deselected_files.retain(|id, _| {
            current
                .get(id)
                .is_some_and(|name| identities.get(id).is_none_or(|prev| prev == name))
        });
        self.torrent_identities = current
            .into_iter()
            .map(|(id, name)| (id, name.to_string()))
            .collect();
    }

    pub fn total_download_speed(&self) -> u64 {
        self.torrents.iter().map(|t| t.download_speed).sum()
    }

    pub fn total_upload_speed(&self) -> u64 {
        self.torrents.iter().map(|t| t.upload_speed).sum()
    }

    pub fn total_uploaded_bytes(&self) -> u64 {
        self.torrents.iter().map(|t| t.uploaded_bytes).sum()
    }

    pub fn total_downloaded_bytes(&self) -> u64 {
        self.torrents.iter().map(|t| t.downloaded_bytes).sum()
    }

    pub fn active_count(&self) -> usize {
        self.torrents
            .iter()
            .filter(|t| matches!(t.status, TorrentStatus::Downloading))
            .count()
    }

    /// Torrents currently held by the download queue (engine-paused because
    /// `max_concurrent_downloads` was reached). Rendered as `Q:<n>` in the
    /// status bar when non-zero.
    pub fn queued_count(&self) -> usize {
        self.torrents
            .iter()
            .filter(|t| matches!(t.status, TorrentStatus::Queued))
            .count()
    }

    pub fn has_fetching_metadata(&self) -> bool {
        self.torrents
            .iter()
            .any(|t| matches!(t.status, TorrentStatus::FetchingMetadata))
    }

    /// Remember a dispatched add until the engine reports
    /// `EngineInfo::AddFinished`. Past `PENDING_ADDS_CAPACITY` the oldest
    /// entry is dropped — the cap only bounds a pathological burst; the
    /// engine still confirms every add's outcome over the message channel.
    pub fn push_pending_add(&mut self, title: String) {
        if self.pending_adds.len() >= PENDING_ADDS_CAPACITY {
            self.pending_adds.pop_front();
        }
        self.pending_adds.push_back(title);
    }

    /// The engine finished one add. Every completion path (success, queued,
    /// already-managed, error) sends exactly one `AddFinished`, so each pop
    /// pairs with one push.
    pub fn pop_pending_add(&mut self) {
        self.pending_adds.pop_front();
    }

    /// Show an error in the status bar for 3 seconds. Replaces any error
    /// already showing — unlike `set_info`, which concatenates — because the
    /// newest failure is the one the user needs.
    pub fn set_error(&mut self, msg: String) {
        self.error_message = Some(msg);
        self.error_timer = Some(std::time::Instant::now());
    }

    /// Show an informational message for 5 seconds. If one is still on screen
    /// the new text is appended after a `|` rather than replacing it, so a
    /// burst does not silently swallow all but the last. The combined line is
    /// rendered unwrapped, which is why the engine batches its own bursts into
    /// at most two messages.
    pub fn set_info(&mut self, msg: String) {
        // If a previous info message is still on screen, append the new one so
        // burst notifications (e.g. multiple completions in one tick) don't
        // overwrite each other silently.
        if let Some(existing) = self.info_message.take() {
            self.info_message = Some(format!("{} | {}", existing, msg));
        } else {
            self.info_message = Some(msg);
        }
        self.info_timer = Some(std::time::Instant::now());
    }

    /// Age out status-bar messages: errors after 3 seconds, info after 5.
    /// There is no timer task behind this — the main loop calls it every pass,
    /// so a message set outside that loop stays on screen until the next
    /// iteration, and one set during shutdown never clears at all.
    pub fn clear_expired_messages(&mut self) {
        if let Some(timer) = self.error_timer {
            if timer.elapsed() > std::time::Duration::from_secs(3) {
                self.error_message = None;
                self.error_timer = None;
            }
        }
        if let Some(timer) = self.info_timer {
            if timer.elapsed() > std::time::Duration::from_secs(5) {
                self.info_message = None;
                self.info_timer = None;
            }
        }
    }

    pub fn tick_spinner(&mut self) {
        self.spinner_tick = (self.spinner_tick + 1) % 10;
    }

    /// Advance to the next theme in the rotation — the config theme first,
    /// then every preset in `PRESET_NAMES` order, wrapping — and announce it
    /// in the status bar. The "config" slot (index 0) restores the resolved
    /// config theme so per-key overrides survive a full cycle; preset slots
    /// replace the palette wholesale. Runtime-only: nothing is written back
    /// to `config.toml`.
    pub fn cycle_theme(&mut self) {
        let slot = Theme::PRESET_NAMES
            .iter()
            .position(|n| *n == self.theme_name)
            .map_or(0, |i| i + 1);
        let next = (slot + 1) % (Theme::PRESET_NAMES.len() + 1);
        let (name, theme) = if next == 0 {
            ("config", self.config_theme.clone())
        } else {
            let name = Theme::PRESET_NAMES[next - 1];
            // Every PRESET_NAMES entry resolves by construction.
            (name, Theme::preset(name).unwrap_or_default())
        };
        self.theme = theme;
        self.theme_name = name.to_string();
        self.set_info(format!("Theme: {name}"));
    }

    /// Open the results view for a freshly dispatched query: previous hits are
    /// dropped and the in-flight flag drives the "Searching…" spinner until
    /// `apply_search_response` lands. The caller owns the actual
    /// `run_search` spawn — this is the pure state transition, so tests can
    /// exercise it without the network.
    ///
    /// With the split on screen the "results view" is the pane itself, so the
    /// mode goes back to Normal with focus on Search and the pane-aware key
    /// routing takes over — that keeps `w`/`|` live while hits stream in.
    /// Without the split (small terminal or pane closed) the full-area
    /// SearchResults mode is still where the spinner and hits render.
    pub fn begin_search(&mut self, query: String) {
        self.search_query = query;
        self.search_results.clear();
        self.search_index = 0;
        self.search_state.select(Some(0));
        self.search_in_flight = true;
        if self.split_active {
            self.mode = AppMode::Normal;
            self.focused = Pane::Search;
        } else {
            self.mode = AppMode::SearchResults;
        }
    }

    /// Merge a completed `run_search` into the UI: store the hits, clear the
    /// in-flight flag and clamp the cursor into the new bounds. Provider
    /// failures are summarized into one status-bar error; an all-provider
    /// strikeout still reads as "no results" rather than a wall of errors.
    ///
    /// The mode is only forced to SearchResults when the user is still in the
    /// search flow — if they Esc'd back to Normal mid-search, the hits land
    /// silently behind them instead of yanking the view back.
    pub fn apply_search_response(&mut self, response: SearchResponse) {
        let SearchResponse {
            query,
            results,
            errors,
        } = response;
        self.search_in_flight = false;
        self.search_results = results;
        // Results only ever change wholesale here, so plain index clamping is
        // enough — there is no per-row identity worth following.
        if self.search_results.is_empty() {
            self.search_index = 0;
        } else if self.search_index >= self.search_results.len() {
            self.search_index = self.search_results.len() - 1;
        }
        self.search_state.select(Some(self.search_index));

        if !errors.is_empty() {
            let summary = errors
                .iter()
                .map(|(provider, err)| format!("{provider}: {err}"))
                .collect::<Vec<_>>()
                .join(" | ");
            self.set_error(summary);
        } else if self.search_results.is_empty() {
            self.set_info(format!("No results for '{query}'"));
        }

        if matches!(self.mode, AppMode::SearchInput | AppMode::SearchResults) {
            self.mode = AppMode::SearchResults;
        }
    }

    pub fn search_next(&mut self) {
        if self.search_results.is_empty() {
            return;
        }
        self.search_index = (self.search_index + 1).min(self.search_results.len() - 1);
        self.search_state.select(Some(self.search_index));
    }

    pub fn search_previous(&mut self) {
        if self.search_results.is_empty() {
            return;
        }
        self.search_index = self.search_index.saturating_sub(1);
        self.search_state.select(Some(self.search_index));
    }

    pub fn selected_search_result(&self) -> Option<&SearchResult> {
        self.search_results.get(self.search_index)
    }

    /// Cycle the target group for adds from the results view through
    /// `[ungrouped] + group_names`, wrapping. A no-op when no groups are
    /// configured, so `g` stays inert rather than clearing a group it can
    /// never bring back.
    pub fn cycle_search_group(&mut self) {
        if self.group_names.is_empty() {
            return;
        }
        self.search_group = match self.search_group.as_deref() {
            None => Some(self.group_names[0].clone()),
            Some(cur) => match self.group_names.iter().position(|n| n == cur) {
                Some(i) if i + 1 < self.group_names.len() => Some(self.group_names[i + 1].clone()),
                // Past the last configured group (or a group that vanished
                // from the config) wraps back to ungrouped.
                _ => None,
            },
        };
    }

    /// Open the search pane and hand it focus, entering the query prompt.
    /// Pure state transition — whether the split is actually drawn depends on
    /// the terminal size (`use_split_layout`); on a small terminal this is
    /// indistinguishable from today's full-area search flow.
    pub fn open_search_pane(&mut self) {
        self.search_pane_open = true;
        self.focused = Pane::Search;
        self.mode = AppMode::SearchInput;
    }

    /// Close the search pane and return focus and mode to the downloads
    /// table. The query and results are kept, so reopening picks up where
    /// the user left off.
    pub fn close_search_pane(&mut self) {
        self.search_pane_open = false;
        self.focused = Pane::Downloads;
        self.mode = AppMode::Normal;
    }

    /// Swap which pane owns the routed keys. A no-op with the pane closed —
    /// there is nothing to switch to, and focus must never point at a pane
    /// that does not exist.
    pub fn cycle_focus(&mut self) {
        if !self.search_pane_open {
            return;
        }
        self.focused = match self.focused {
            Pane::Downloads => Pane::Search,
            Pane::Search => Pane::Downloads,
        };
    }

    /// Flip the split orientation (side-by-side <-> stacked). Runtime-only;
    /// the config file's `search_split` decides the next launch.
    pub fn toggle_split(&mut self) {
        self.split_vertical = !self.split_vertical;
    }

    /// Whether the routed keys (`j`/`k`/`Enter`/`g`) currently belong to the
    /// search pane. Requires `split_active` — not just an open pane — so the
    /// small-terminal fallback (pane open but hidden) keeps downloads
    /// navigation on the table the user actually sees.
    pub fn search_has_focus(&self) -> bool {
        self.split_active && self.focused == Pane::Search
    }

    /// Whether a file is included in the download. Returns `true` for torrents
    /// and indices this has never seen, because selection is tracked as the set
    /// of *exclusions* — so an out-of-range `file_index` also reports selected.
    /// Bound the index against the torrent's real file count before trusting
    /// it.
    pub fn is_file_selected(&self, torrent_id: usize, file_index: usize) -> bool {
        !self
            .deselected_files
            .get(&torrent_id)
            .is_some_and(|s| s.contains(&file_index))
    }

    pub fn toggle_file_selection(&mut self, torrent_id: usize, file_index: usize) {
        let set = self.deselected_files.entry(torrent_id).or_default();
        if set.contains(&file_index) {
            set.remove(&file_index);
        } else {
            set.insert(file_index);
        }
    }

    pub fn selected_file_indices(&self, torrent_id: usize, total_files: usize) -> Vec<usize> {
        (0..total_files)
            .filter(|i| self.is_file_selected(torrent_id, *i))
            .collect()
    }

    /// Whether the free-space figure is due for a refresh (at most once every
    /// 5 seconds). Safe and intended to call every frame — the rate limit lives
    /// here rather than at the call site. Marks the attempt immediately, so a
    /// slow probe can't queue up behind itself.
    pub fn disk_space_due(&mut self) -> bool {
        let due = match self.disk_space_timer {
            None => true,
            Some(t) => t.elapsed() > std::time::Duration::from_secs(5),
        };
        if due {
            self.disk_space_timer = Some(std::time::Instant::now());
        }
        due
    }

    /// Record a probe result. `None` (directory removed, network share
    /// unmounted) clears the cached value rather than keeping a stale reading,
    /// so the indicator disappears instead of lying.
    pub fn set_disk_space(&mut self, free: Option<u64>) {
        self.free_disk_space = free;
    }

    pub fn toggle_mark(&mut self) {
        if let Some(torrent) = self.selected_torrent() {
            let id = torrent.id;
            if self.marked_ids.contains(&id) {
                self.marked_ids.remove(&id);
            } else {
                self.marked_ids.insert(id);
            }
        }
    }

    pub fn clear_marks(&mut self) {
        self.marked_ids.clear();
    }

    /// Mark every torrent currently *visible* — the active filter applies —
    /// and add to the existing marks rather than replacing them, so narrowing
    /// the filter and pressing `v` repeatedly accumulates a selection across
    /// searches. Marks for torrents hidden by the filter survive untouched,
    /// including through a subsequent bulk delete.
    pub fn mark_all(&mut self) {
        let ids: Vec<usize> = self.sorted_torrents().iter().map(|t| t.id).collect();
        self.marked_ids.extend(ids);
    }

    pub fn has_marks(&self) -> bool {
        !self.marked_ids.is_empty()
    }

    pub fn marked_count(&self) -> usize {
        self.marked_ids.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TorrentStatus;

    fn make_torrent(id: usize, name: &str, size: u64, status: TorrentStatus) -> TorrentInfo {
        TorrentInfo {
            id,
            name: name.to_string(),
            group: None,
            size_bytes: size,
            downloaded_bytes: 0,
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
            throttle_managed: false,
        }
    }

    /// Build an App the way the running app does — through `handle_state_push`,
    /// so the sort cache is built and every test below exercises
    /// `rebuild_sort_cache` rather than the dirty-cache fallback.
    fn app_with_torrents(torrents: Vec<TorrentInfo>) -> App {
        let mut app = App::new();
        app.handle_state_push(torrents);
        app
    }

    /// The fallback path: torrents assigned directly, cache left dirty. Only
    /// for the two tests that deliberately cover `sorted_torrents_live`.
    fn app_with_dirty_cache(torrents: Vec<TorrentInfo>) -> App {
        let mut app = App::new();
        app.torrents = torrents;
        app
    }

    #[test]
    fn sorted_torrents_no_filter() {
        let app = app_with_torrents(vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Beta", 200, TorrentStatus::Paused),
        ]);
        assert_eq!(app.sorted_torrents().len(), 2);
    }

    #[test]
    fn queued_count_counts_only_queued() {
        let app = app_with_torrents(vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Beta", 200, TorrentStatus::Queued),
            make_torrent(2, "Gamma", 300, TorrentStatus::Queued),
            make_torrent(3, "Delta", 400, TorrentStatus::Paused),
        ]);
        assert_eq!(app.queued_count(), 2);
        assert_eq!(app.active_count(), 1);
    }

    #[test]
    fn sorted_torrents_filter_case_insensitive() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Beta", 200, TorrentStatus::Paused),
        ]);
        for c in "alpha".chars() {
            app.push_filter_char(c);
        }
        let sorted = app.sorted_torrents();
        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].name, "Alpha");
    }

    #[test]
    fn sorted_torrents_filter_no_matches() {
        let mut app = app_with_torrents(vec![make_torrent(
            0,
            "Alpha",
            100,
            TorrentStatus::Downloading,
        )]);
        for c in "zzz".chars() {
            app.push_filter_char(c);
        }
        assert!(app.sorted_torrents().is_empty());
    }

    #[test]
    fn filter_matches_group_name_case_insensitively() {
        let mut grouped = make_torrent(0, "ubuntu.iso", 100, TorrentStatus::Downloading);
        grouped.group = Some("Linux-ISOs".to_string());
        let plain = make_torrent(1, "debian.iso", 200, TorrentStatus::Downloading);
        let mut app = app_with_torrents(vec![grouped, plain]);
        for c in "linux".chars() {
            app.push_filter_char(c);
        }
        let sorted = app.sorted_torrents();
        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].name, "ubuntu.iso");
    }

    #[test]
    fn sorted_by_group_ungrouped_first() {
        let mut a = make_torrent(0, "A", 100, TorrentStatus::Downloading);
        a.group = Some("movies".to_string());
        let b = make_torrent(1, "B", 100, TorrentStatus::Downloading);
        let mut c = make_torrent(2, "C", 100, TorrentStatus::Downloading);
        c.group = Some("linux".to_string());
        let mut app = app_with_torrents(vec![a, b, c]);
        app.change_sort_column(SortColumn::Group);
        let names: Vec<&str> = app
            .sorted_torrents()
            .iter()
            .map(|t| t.name.as_str())
            .collect();
        assert_eq!(names, vec!["B", "C", "A"]);
    }

    #[test]
    fn group_change_triggers_a_repaint() {
        let rows = vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)];
        let mut app = App::new();
        app.handle_state_push(rows.clone());
        let mut grouped = rows;
        grouped[0].group = Some("movies".to_string());
        assert!(app.handle_state_push(grouped), "group set is a change");
    }

    #[test]
    fn cycle_search_group_walks_none_then_groups_then_wraps() {
        let mut app = App::new();
        app.group_names = vec!["linux".to_string(), "movies".to_string()];
        assert_eq!(app.search_group, None);
        app.cycle_search_group();
        assert_eq!(app.search_group.as_deref(), Some("linux"));
        app.cycle_search_group();
        assert_eq!(app.search_group.as_deref(), Some("movies"));
        app.cycle_search_group();
        assert_eq!(app.search_group, None);
    }

    #[test]
    fn cycle_search_group_is_inert_without_groups() {
        let mut app = App::new();
        app.cycle_search_group();
        assert_eq!(app.search_group, None);
    }

    #[test]
    fn recycled_torrent_id_drops_stale_marks_and_deselections() {
        // librqbit reuses `max(ids) + 1`, so deleting the highest-id torrent
        // frees its id for a concurrent watch-folder add. Without the identity
        // check the old deselections silently attached to the new torrent.
        let mut app = App::new();
        app.handle_state_push(vec![make_torrent(
            7,
            "Old",
            100,
            TorrentStatus::Downloading,
        )]);
        app.marked_ids.insert(7);
        app.deselected_files.insert(7, HashSet::from([2, 5]));

        // Same id, different torrent.
        app.handle_state_push(vec![make_torrent(
            7,
            "New",
            100,
            TorrentStatus::Downloading,
        )]);
        assert!(!app.marked_ids.contains(&7), "mark must not survive");
        assert!(
            !app.deselected_files.contains_key(&7),
            "deselection must not survive"
        );
        assert!(
            app.is_file_selected(7, 2),
            "new torrent downloads everything"
        );
    }

    #[test]
    fn same_torrent_keeps_its_marks_across_pushes() {
        let mut app = App::new();
        let rows = vec![make_torrent(7, "Same", 100, TorrentStatus::Downloading)];
        app.handle_state_push(rows.clone());
        app.marked_ids.insert(7);
        app.deselected_files.insert(7, HashSet::from([1]));

        let mut moved = rows.clone();
        moved[0].downloaded_bytes += 10;
        app.handle_state_push(moved);
        assert!(app.marked_ids.contains(&7));
        assert!(!app.is_file_selected(7, 1));
    }

    #[test]
    fn identical_state_push_reports_no_change() {
        // The engine pushes every 100 ms regardless. Repainting on each one
        // meant idle torrents redrew the terminal ten times a second.
        let rows = vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Beta", 200, TorrentStatus::Paused),
        ];
        let mut app = App::new();
        assert!(
            app.handle_state_push(rows.clone()),
            "first push is a change"
        );
        assert!(
            !app.handle_state_push(rows.clone()),
            "identical push is not"
        );

        // ...but anything the table draws must still trigger a repaint.
        let mut moved = rows.clone();
        moved[0].downloaded_bytes += 1;
        assert!(app.handle_state_push(moved), "progress moved");

        let mut renamed = rows.clone();
        renamed[1].status = TorrentStatus::Seeding;
        assert!(app.handle_state_push(renamed), "status changed");

        assert!(
            app.handle_state_push(vec![rows[0].clone()]),
            "a torrent disappearing is a change"
        );
    }

    #[test]
    fn sorted_torrents_falls_back_to_a_live_sort_when_the_cache_is_dirty() {
        // The `&self` read path can't rebuild, so it sorts live instead of
        // returning a stale order. Mutating the fields directly (rather than
        // through `change_sort_column`) is what leaves the cache dirty.
        let mut app = app_with_dirty_cache(vec![
            make_torrent(0, "Zeta", 100, TorrentStatus::Downloading),
            make_torrent(1, "Alpha", 200, TorrentStatus::Downloading),
        ]);
        app.sort_column = SortColumn::Name;
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "Alpha");
        assert_eq!(sorted[1].name, "Zeta");
    }

    #[test]
    fn cached_and_live_comparators_agree() {
        // `rebuild_sort_cache` and `sorted_torrents_live` carry separate copies
        // of the comparator. Pin them against each other so adding a sort
        // column to one and not the other fails here.
        for column in [
            SortColumn::Index,
            SortColumn::Name,
            SortColumn::Group,
            SortColumn::Size,
            SortColumn::Progress,
            SortColumn::Status,
            SortColumn::Speed,
            SortColumn::Eta,
        ] {
            for reversed in [false, true] {
                let torrents = vec![
                    make_torrent(0, "Zeta", 100, TorrentStatus::Downloading),
                    make_torrent(1, "Alpha", 300, TorrentStatus::Paused),
                    make_torrent(2, "Mid", 200, TorrentStatus::Seeding),
                ];
                let mut cached = app_with_torrents(torrents.clone());
                cached.change_sort_column(column);
                if reversed {
                    cached.toggle_sort_reversed();
                }
                let mut live = app_with_dirty_cache(torrents);
                live.sort_column = column;
                live.sort_reversed = reversed;

                let a: Vec<usize> = cached.sorted_torrents().iter().map(|t| t.id).collect();
                let b: Vec<usize> = live.sorted_torrents().iter().map(|t| t.id).collect();
                assert_eq!(a, b, "column {column:?} reversed={reversed}");
            }
        }
    }

    #[test]
    fn sorted_by_name() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Zeta", 100, TorrentStatus::Downloading),
            make_torrent(1, "Alpha", 200, TorrentStatus::Downloading),
        ]);
        app.change_sort_column(SortColumn::Name);
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "Alpha");
        assert_eq!(sorted[1].name, "Zeta");
    }

    #[test]
    fn sorted_by_name_reversed() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Alpha", 100, TorrentStatus::Downloading),
            make_torrent(1, "Zeta", 200, TorrentStatus::Downloading),
        ]);
        app.change_sort_column(SortColumn::Name);
        app.toggle_sort_reversed();
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "Zeta");
    }

    #[test]
    fn sorted_by_size() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "Big", 1000, TorrentStatus::Downloading),
            make_torrent(1, "Small", 100, TorrentStatus::Downloading),
        ]);
        app.change_sort_column(SortColumn::Size);
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "Small");
        assert_eq!(sorted[1].name, "Big");
    }

    #[test]
    fn sorted_eta_none_last() {
        let mut t1 = make_torrent(0, "A", 100, TorrentStatus::Downloading);
        t1.eta_seconds = Some(60);
        let mut t2 = make_torrent(1, "B", 100, TorrentStatus::Downloading);
        t2.eta_seconds = None;
        let mut app = app_with_torrents(vec![t2, t1]);
        app.sort_column = SortColumn::Eta;
        let sorted = app.sorted_torrents();
        assert_eq!(sorted[0].name, "A"); // Some(60) first
        assert_eq!(sorted[1].name, "B"); // None last
    }

    #[test]
    fn next_empty_list() {
        let mut app = App::new();
        app.next(); // should not panic
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn next_single_item() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        app.next();
        assert_eq!(app.selected_index, 0); // stays at 0
    }

    #[test]
    fn next_advances() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 100, TorrentStatus::Downloading),
            make_torrent(2, "C", 100, TorrentStatus::Downloading),
        ]);
        app.next();
        assert_eq!(app.selected_index, 1);
        app.next();
        assert_eq!(app.selected_index, 2);
        app.next();
        assert_eq!(app.selected_index, 2); // clamped at end
    }

    #[test]
    fn previous_at_zero() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        app.previous();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn previous_moves_up() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 100, TorrentStatus::Downloading),
        ]);
        app.selected_index = 1;
        app.previous();
        assert_eq!(app.selected_index, 0);
    }

    #[test]
    fn selected_torrent_returns_correct() {
        let app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 200, TorrentStatus::Downloading),
        ]);
        let t = app.selected_torrent().unwrap();
        assert_eq!(t.name, "A");
    }

    #[test]
    fn toggle_mark() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        assert!(!app.has_marks());
        app.toggle_mark();
        assert!(app.has_marks());
        assert_eq!(app.marked_count(), 1);
        app.toggle_mark();
        assert!(!app.has_marks());
    }

    #[test]
    fn mark_all_and_clear() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 100, TorrentStatus::Downloading),
            make_torrent(2, "C", 100, TorrentStatus::Downloading),
        ]);
        app.mark_all();
        assert_eq!(app.marked_count(), 3);
        app.clear_marks();
        assert!(!app.has_marks());
    }

    #[test]
    fn marks_tracked_by_id() {
        let mut app = app_with_torrents(vec![
            make_torrent(5, "A", 100, TorrentStatus::Downloading),
            make_torrent(10, "B", 100, TorrentStatus::Downloading),
        ]);
        app.toggle_mark(); // marks id=5 (first in sorted)
        assert!(app.marked_ids.contains(&5));
        assert!(!app.marked_ids.contains(&10));
    }

    #[test]
    fn restore_selection_refreshes_cached_id() {
        let mut app = app_with_torrents(vec![
            make_torrent(5, "A", 100, TorrentStatus::Downloading),
            make_torrent(10, "B", 100, TorrentStatus::Downloading),
        ]);
        app.selected_index = 1;
        app.update_selected_id();
        assert_eq!(app.selected_torrent_id, Some(10));

        // Remove the torrent currently selected and call restore_selection.
        app.torrents.retain(|t| t.id != 10);
        app.restore_selection();
        assert_eq!(app.selected_index, 0);
        // The cached id must follow the new selection, not stay stale on 10.
        assert_eq!(app.selected_torrent_id, Some(5));
    }

    #[test]
    fn prune_stale_state_drops_marks_and_deselections() {
        let mut app = app_with_torrents(vec![
            make_torrent(1, "A", 100, TorrentStatus::Downloading),
            make_torrent(2, "B", 100, TorrentStatus::Downloading),
        ]);
        app.marked_ids.insert(1);
        app.marked_ids.insert(2);
        app.marked_ids.insert(99); // stale, no torrent with id 99
        app.deselected_files.insert(2, HashSet::new());
        app.deselected_files.insert(42, HashSet::new()); // stale

        app.prune_stale_state();

        assert!(app.marked_ids.contains(&1));
        assert!(app.marked_ids.contains(&2));
        assert!(!app.marked_ids.contains(&99));
        assert!(app.deselected_files.contains_key(&2));
        assert!(!app.deselected_files.contains_key(&42));
    }

    #[test]
    fn set_info_concatenates_when_unread() {
        let mut app = App::new();
        app.set_info("first".to_string());
        app.set_info("second".to_string());
        assert_eq!(app.info_message.as_deref(), Some("first | second"));
    }

    fn file(name: &str) -> crate::types::FileInfo {
        crate::types::FileInfo {
            name: name.to_string(),
            size_bytes: 100,
            progress_bytes: 0,
        }
    }

    fn peer(addr: &str) -> crate::types::PeerInfo {
        crate::types::PeerInfo {
            address: addr.to_string(),
            state: "Live".to_string(),
            downloaded_bytes: 0,
            pieces: 0,
            errors: 0,
        }
    }

    #[test]
    fn tick_spinner_wraps_at_ten() {
        let mut app = App::new();
        assert_eq!(app.spinner_tick, 0);
        app.tick_spinner();
        assert_eq!(app.spinner_tick, 1);
        for _ in 0..9 {
            app.tick_spinner();
        }
        // 10 ticks from 0 wraps back to 0.
        assert_eq!(app.spinner_tick, 0);
        // The `% 10` in tick_spinner must match SPINNER_FRAMES.len(), or
        // table.rs panics when it indexes SPINNER_FRAMES[spinner_tick].
        assert_eq!(crate::ui::progress::SPINNER_FRAMES.len(), 10);
    }

    #[test]
    fn pending_adds_push_and_pop_are_fifo() {
        let mut app = App::new();
        assert!(app.pending_adds.is_empty());
        app.push_pending_add("first".to_string());
        app.push_pending_add("second".to_string());
        app.pop_pending_add();
        assert_eq!(app.pending_adds.front().map(String::as_str), Some("second"));
        app.pop_pending_add();
        assert!(app.pending_adds.is_empty());
        // Popping an empty queue is a no-op, not a panic: a dropped
        // `AddFinished` can leave push/pop counts mismatched.
        app.pop_pending_add();
    }

    #[test]
    fn pending_adds_cap_drops_the_oldest() {
        let mut app = App::new();
        for i in 0..PENDING_ADDS_CAPACITY + 2 {
            app.push_pending_add(format!("add_{i}"));
        }
        assert_eq!(app.pending_adds.len(), PENDING_ADDS_CAPACITY);
        assert_eq!(
            app.pending_adds.front().map(String::as_str),
            Some("add_2"),
            "the two oldest entries must have been dropped"
        );
    }

    #[test]
    fn clamp_detail_indices_clamps_to_last_when_lists_shrink() {
        let mut t = make_torrent(0, "A", 100, TorrentStatus::Downloading);
        t.files = vec![file("a"), file("b")];
        t.peers = vec![peer("1.1.1.1:1"), peer("2.2.2.2:2"), peer("3.3.3.3:3")];
        let mut app = app_with_torrents(vec![t]);
        app.update_selected_id();
        app.detail_file_index = 9;
        app.detail_peer_index = 9;
        app.clamp_detail_indices();
        assert_eq!(app.detail_file_index, 1); // 2 files -> max index 1
        assert_eq!(app.detail_peer_index, 2); // 3 peers -> max index 2
    }

    #[test]
    fn clamp_detail_indices_resets_when_no_selection() {
        let mut app = App::new(); // no torrents -> selected_torrent() is None
        app.detail_file_index = 5;
        app.detail_peer_index = 7;
        app.clamp_detail_indices();
        assert_eq!(app.detail_file_index, 0);
        assert_eq!(app.detail_peer_index, 0);
    }

    #[test]
    fn confirm_on_quit_required_true_when_active() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        app.confirm_on_quit = true;
        assert!(app.confirm_on_quit_required());
    }

    #[test]
    fn confirm_on_quit_required_false_when_idle() {
        let mut app = app_with_torrents(vec![
            make_torrent(0, "A", 100, TorrentStatus::Complete),
            make_torrent(1, "B", 100, TorrentStatus::Paused),
        ]);
        app.confirm_on_quit = true;
        assert!(!app.confirm_on_quit_required());
    }

    #[test]
    fn confirm_on_quit_required_false_when_disabled() {
        let mut app =
            app_with_torrents(vec![make_torrent(0, "A", 100, TorrentStatus::Downloading)]);
        app.confirm_on_quit = false;
        assert!(!app.confirm_on_quit_required());
    }

    #[test]
    fn file_selection_toggle_and_query() {
        let mut app = App::new();
        // All files selected by default.
        assert!(app.is_file_selected(0, 0));
        assert_eq!(app.selected_file_indices(0, 3), vec![0, 1, 2]);
        // Deselect file 1.
        app.toggle_file_selection(0, 1);
        assert!(!app.is_file_selected(0, 1));
        assert_eq!(app.selected_file_indices(0, 3), vec![0, 2]);
        // Toggle it back.
        app.toggle_file_selection(0, 1);
        assert!(app.is_file_selected(0, 1));
        assert_eq!(app.selected_file_indices(0, 3), vec![0, 1, 2]);
    }

    #[test]
    fn messages_expire_after_their_window() {
        use std::time::{Duration, Instant};
        let mut app = App::new();
        app.set_error("boom".to_string());
        app.set_info("note".to_string());
        // Fresh messages are not cleared.
        app.clear_expired_messages();
        assert_eq!(app.error_message.as_deref(), Some("boom"));
        assert_eq!(app.info_message.as_deref(), Some("note"));
        // Age the timers past their windows (3s errors, 5s info).
        app.error_timer = Some(
            Instant::now()
                .checked_sub(Duration::from_secs(4))
                .expect("system uptime should exceed a few seconds"),
        );
        app.info_timer = Some(
            Instant::now()
                .checked_sub(Duration::from_secs(6))
                .expect("system uptime should exceed a few seconds"),
        );
        app.clear_expired_messages();
        assert!(app.error_message.is_none());
        assert!(app.info_message.is_none());
    }

    #[test]
    fn speed_history_caps_at_capacity_and_drops_oldest() {
        let mut history = VecDeque::new();
        for i in 0..SPEED_HISTORY_CAPACITY as u64 + 5 {
            push_speed_sample(&mut history, i);
        }
        assert_eq!(history.len(), SPEED_HISTORY_CAPACITY);
        // FIFO: the first five samples are gone, the newest is last.
        assert_eq!(history.front(), Some(&5));
        assert_eq!(history.back(), Some(&(SPEED_HISTORY_CAPACITY as u64 + 4)));
    }

    #[test]
    fn state_push_records_aggregate_speed_samples() {
        let mut app = App::new();
        let mut rows = vec![
            make_torrent(0, "A", 100, TorrentStatus::Downloading),
            make_torrent(1, "B", 200, TorrentStatus::Downloading),
        ];
        rows[0].download_speed = 500;
        rows[1].download_speed = 700;
        rows[0].upload_speed = 60;
        app.handle_state_push(rows.clone());
        assert_eq!(app.down_history.back(), Some(&1200));
        assert_eq!(app.up_history.back(), Some(&60));

        // An unchanged push still appends a sample — the sparkline is a time
        // series — but must not count as a render-worthy change.
        assert!(!app.handle_state_push(rows));
        assert_eq!(app.down_history.len(), 2);
        assert_eq!(app.down_history.back(), Some(&1200));
    }

    #[test]
    fn open_search_pane_focuses_search_and_enters_the_prompt() {
        let mut app = App::new();
        app.open_search_pane();
        assert!(app.search_pane_open);
        assert_eq!(app.focused, Pane::Search);
        assert_eq!(app.mode, AppMode::SearchInput);
    }

    #[test]
    fn close_search_pane_returns_focus_and_mode_to_downloads() {
        let mut app = App::new();
        app.open_search_pane();
        app.close_search_pane();
        assert!(!app.search_pane_open);
        assert_eq!(app.focused, Pane::Downloads);
        assert_eq!(app.mode, AppMode::Normal);
    }

    #[test]
    fn cycle_focus_swaps_panes_only_while_open() {
        let mut app = App::new();
        // Closed pane: nothing to switch to, focus must not move.
        app.cycle_focus();
        assert_eq!(app.focused, Pane::Downloads);

        app.open_search_pane();
        app.mode = AppMode::Normal;
        app.cycle_focus();
        assert_eq!(app.focused, Pane::Downloads);
        app.cycle_focus();
        assert_eq!(app.focused, Pane::Search);
    }

    #[test]
    fn toggle_split_flips_the_orientation() {
        let mut app = App::new();
        assert!(app.split_vertical);
        app.toggle_split();
        assert!(!app.split_vertical);
        app.toggle_split();
        assert!(app.split_vertical);
    }

    #[test]
    fn search_focus_requires_the_split_on_screen() {
        // An open-but-hidden pane (small terminal) must not claim the routed
        // keys — the user can only see the downloads table.
        let mut app = App::new();
        app.open_search_pane();
        assert!(!app.search_has_focus());
        app.split_active = true;
        assert!(app.search_has_focus());
        app.cycle_focus();
        assert!(!app.search_has_focus());
    }

    #[test]
    fn begin_search_with_the_split_goes_to_normal_not_results_mode() {
        // The pane is already the results view, so there is no separate
        // full-area mode to switch into — focus moves to Search instead.
        let mut app = App::new();
        app.open_search_pane();
        app.split_active = true;
        app.begin_search("x".to_string());
        assert_eq!(app.mode, AppMode::Normal);
        assert_eq!(app.focused, Pane::Search);
        assert!(app.search_in_flight);

        // The small-terminal path keeps the full-area SearchResults mode.
        let mut app = App::new();
        app.open_search_pane();
        app.begin_search("x".to_string());
        assert_eq!(app.mode, AppMode::SearchResults);
    }

    #[test]
    fn search_response_with_pane_open_keeps_pane_state() {
        let mut app = App::new();
        app.open_search_pane();
        app.split_active = true;
        app.begin_search("x".to_string());
        app.apply_search_response(SearchResponse {
            query: "x".to_string(),
            results: vec![SearchResult {
                title: "A".to_string(),
                magnet: "magnet:?xt=urn:btih:aaa".to_string(),
                size_bytes: 1,
                seeders: 1,
                leechers: 0,
                provider: "TPB".to_string(),
            }],
            errors: Vec::new(),
        });
        assert!(app.search_pane_open);
        assert_eq!(app.focused, Pane::Search);
        // Mode was Normal (split path), so nothing forces it to
        // SearchResults — that switch only happens inside the search modes.
        assert_eq!(app.mode, AppMode::Normal);
        assert_eq!(app.search_results.len(), 1);
        assert!(!app.search_in_flight);
    }
}
