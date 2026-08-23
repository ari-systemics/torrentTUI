//! The Detail view: four tabs (Stats, Info, Files, Peers) over one torrent.
//!
//! Everything here depends on the engine having been told which torrent is
//! being viewed. `TorrentInfo::files`, `peers`, `trackers`, `info_hash` and
//! `piece_length` are populated only for the Detail target; entering and
//! leaving this view sends `SetDetailTorrent`, and without that every tab but
//! Stats renders empty.
//!
//! All four renderers return early and draw nothing when there is no selection,
//! which is safe only because the caller has already cleared the frame.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Tabs},
    Frame,
};

use crate::app::App;
use crate::types::{DetailTab, PeerInfo};
use crate::ui::layout::{format_eta, format_size, format_speed};
use crate::ui::progress::render_progress_bar_styled;
use crate::ui::util::{is_streamable_media, truncate};

pub fn render_detail(f: &mut Frame, area: Rect, app: &mut App) {
    let (torrent_name, tab_index) = match app.selected_torrent() {
        Some(t) => (t.name.clone(), app.detail_tab.index()),
        None => return,
    };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header with name
            Constraint::Length(3), // Tabs
            Constraint::Min(5),    // Tab content
        ])
        .split(area);

    // Header
    let header = Paragraph::new(Line::from(vec![Span::styled(
        torrent_name,
        Style::default()
            .fg(app.theme.accent)
            .add_modifier(Modifier::BOLD),
    )]))
    .block(
        Block::default()
            .title(" Torrent Detail ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(app.theme.accent)),
    );
    f.render_widget(header, chunks[0]);

    // Tabs
    let tab_titles = vec!["Stats", "Info", "Files", "Peers"];
    let tabs = Tabs::new(tab_titles)
        .select(tab_index)
        .style(Style::default().fg(app.theme.text_dim))
        .highlight_style(
            Style::default()
                .fg(app.theme.accent)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(app.theme.accent)),
        );
    f.render_widget(tabs, chunks[1]);

    // Tab content
    match app.detail_tab {
        DetailTab::Stats => render_stats_tab(f, chunks[2], app),
        DetailTab::Info => render_info_tab(f, chunks[2], app),
        DetailTab::Files => render_files_tab(f, chunks[2], app),
        DetailTab::Peers => render_peers_tab(f, chunks[2], app),
    }
}

fn render_stats_tab(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let torrent = match app.selected_torrent() {
        Some(t) => t,
        None => return,
    };

    let percent = torrent.progress_percent();
    let progress = render_progress_bar_styled(percent, 30, theme);

    let stats_text = vec![
        Line::from(vec![
            Span::styled("  Status:    ", Style::default().fg(theme.text_dim)),
            Span::raw(torrent.status.to_string()),
        ]),
        Line::from(vec![
            Span::styled("  Size:      ", Style::default().fg(theme.text_dim)),
            Span::raw(format!(
                "{} / {}",
                format_size(torrent.downloaded_bytes),
                format_size(torrent.size_bytes)
            )),
        ]),
        Line::from(
            std::iter::once(Span::styled(
                "  Progress:  ",
                Style::default().fg(theme.text_dim),
            ))
            .chain(progress.spans)
            .collect::<Vec<_>>(),
        ),
        Line::from(vec![
            Span::styled("  Uploaded:  ", Style::default().fg(theme.text_dim)),
            Span::raw(format_size(torrent.uploaded_bytes)),
        ]),
        Line::from(vec![
            Span::styled("  Ratio:     ", Style::default().fg(theme.text_dim)),
            Span::raw(if torrent.downloaded_bytes > 0 {
                format!(
                    "{:.2}",
                    torrent.uploaded_bytes as f64 / torrent.downloaded_bytes as f64
                )
            } else {
                "\u{2014}".to_string()
            }),
        ]),
        Line::from(vec![
            Span::styled("  Down:      ", Style::default().fg(theme.text_dim)),
            Span::styled(
                format_speed(torrent.download_speed),
                Style::default().fg(theme.download),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Up:        ", Style::default().fg(theme.text_dim)),
            Span::styled(
                format_speed(torrent.upload_speed),
                Style::default().fg(theme.upload),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Peers:     ", Style::default().fg(theme.text_dim)),
            Span::raw(format!(
                "{} connected / {} total",
                torrent.peers_connected, torrent.peers_total
            )),
        ]),
        Line::from(vec![
            Span::styled("  ETA:       ", Style::default().fg(theme.text_dim)),
            Span::raw(format_eta(torrent.eta_seconds)),
        ]),
    ];

    let stats = Paragraph::new(stats_text).block(
        Block::default()
            .title(" Stats ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border)),
    );
    f.render_widget(stats, area);
}

fn render_info_tab(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let torrent = match app.selected_torrent() {
        Some(t) => t,
        None => return,
    };

    let ratio = if torrent.downloaded_bytes > 0 {
        format!(
            "{:.2}",
            torrent.uploaded_bytes as f64 / torrent.downloaded_bytes as f64
        )
    } else {
        "\u{2014}".to_string()
    };

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  Info Hash:    ", Style::default().fg(theme.text_dim)),
            Span::styled(&torrent.info_hash, Style::default().fg(theme.accent)),
        ]),
        Line::from(vec![
            Span::styled("  Uploaded:     ", Style::default().fg(theme.text_dim)),
            Span::raw(format!(
                "{}  (ratio: {})",
                format_size(torrent.uploaded_bytes),
                ratio
            )),
        ]),
    ];

    if let Some(pl) = torrent.piece_length {
        lines.push(Line::from(vec![
            Span::styled("  Piece Size:   ", Style::default().fg(theme.text_dim)),
            Span::raw(format_size(pl as u64)),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Trackers:",
        Style::default().fg(theme.text_dim),
    )));
    if torrent.trackers.is_empty() {
        lines.push(Line::from(Span::styled(
            "    (DHT only)",
            Style::default().fg(theme.text_dim),
        )));
    } else {
        for tracker in &torrent.trackers {
            lines.push(Line::from(format!("    {}", tracker)));
        }
    }

    let info_widget = Paragraph::new(lines).block(
        Block::default()
            .title(" Info ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border)),
    );
    f.render_widget(info_widget, area);
}

fn render_files_tab(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let torrent = match app.selected_torrent() {
        Some(t) => t,
        None => return,
    };

    if torrent.files.is_empty() {
        let placeholder =
            Paragraph::new("  No file information available yet (waiting for metadata).")
                .style(Style::default().fg(theme.text_dim))
                .block(
                    Block::default()
                        .title(" Files ")
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(theme.border)),
                );
        f.render_widget(placeholder, area);
        return;
    }

    let torrent_id = torrent.id;
    let mut lines: Vec<Line> = Vec::new();
    for (idx, file) in torrent.files.iter().enumerate() {
        let percent = if file.size_bytes > 0 {
            (file.progress_bytes as f64 / file.size_bytes as f64 * 100.0).clamp(0.0, 100.0)
        } else {
            0.0
        };
        let bar = render_progress_bar_styled(percent, 10, theme);

        let selected = app.is_file_selected(torrent_id, idx);
        let checkbox = if selected { "[\u{2713}]" } else { "[ ]" };

        let is_highlighted = idx == app.detail_file_index;

        let highlight_style = if is_highlighted {
            Style::default()
                .bg(theme.highlight)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let file_style = if !selected {
            Style::default().fg(theme.text_dim)
        } else {
            highlight_style
        };

        let checkbox_style = if selected {
            Style::default().fg(theme.download)
        } else {
            Style::default().fg(theme.text_dim)
        };

        let is_media = is_streamable_media(&file.name);
        // ▶ on streamable media so users know `s` will work; two spaces
        // otherwise to keep the column alignment identical.
        let media_glyph = if is_media { "\u{25B6} " } else { "  " };
        let media_glyph_style = if is_media {
            Style::default().fg(theme.accent)
        } else {
            Style::default()
        };

        lines.push(Line::from(
            vec![
                Span::styled(if is_highlighted { "> " } else { "  " }, highlight_style),
                Span::styled(format!("{} ", checkbox), checkbox_style),
                Span::styled(media_glyph, media_glyph_style),
                Span::styled(crate::ui::util::pad_to_width(&file.name, 45), file_style),
                Span::styled(format!("{:>10}", format_size(file.size_bytes)), file_style),
                Span::raw("  "),
            ]
            .into_iter()
            .chain(bar.spans)
            .collect::<Vec<_>>(),
        ));
    }

    let files_widget = Paragraph::new(lines).block(
        Block::default()
            .title(format!(
                " Files ({}) - Space:toggle  S:apply  s:stream \u{25B6} ",
                torrent.files.len()
            ))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border)),
    );
    f.render_widget(files_widget, area);
}

/// Render the Peers tab. Takes `&mut App` because scrolling is resolved here:
/// the row list is what defines the visible window, so the scroll offset is
/// recomputed each frame to keep `detail_peer_index` on screen (the key
/// handlers only move the index).
///
/// Rows are sorted by bytes downloaded, descending — so `detail_peer_index` is
/// a position in *this* ordering, not in `TorrentInfo::peers`. It stays valid
/// only because both have the same length; do not use it to index the
/// underlying vec.
fn render_peers_tab(f: &mut Frame, area: Rect, app: &mut App) {
    // Owned copy up front: this function mutates `app` (scroll state) before
    // building lines, so a borrowed `&app.theme` would not survive.
    let theme = app.theme.clone();
    // Pull just what we need so the immutable borrow ends before we touch
    // `app` mutably to update scroll state.
    let (peer_count, peers_connected, peers_total) = match app.selected_torrent() {
        Some(t) => (t.peers.len(), t.peers_connected, t.peers_total),
        None => return,
    };

    if peer_count == 0 {
        let text = vec![
            Line::from(""),
            Line::from(vec![
                Span::styled("  Connected:  ", Style::default().fg(theme.text_dim)),
                Span::raw(format!("{}", peers_connected)),
            ]),
            Line::from(vec![
                Span::styled("  Total seen: ", Style::default().fg(theme.text_dim)),
                Span::raw(format!("{}", peers_total)),
            ]),
            Line::from(""),
            Line::from(Span::styled(
                "  No peers connected",
                Style::default().fg(theme.text_dim),
            )),
        ];

        let peers_widget = Paragraph::new(text).block(
            Block::default()
                .title(" Peers ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.border)),
        );
        f.render_widget(peers_widget, area);
        return;
    }

    // 2 border rows + 3 header lines = 5, plus one row of slack so the last
    // peer never lands flush against the bottom border.
    let visible_height = area.height.saturating_sub(6) as usize;
    let peer_index = app.detail_peer_index.min(peer_count.saturating_sub(1));
    app.detail_peer_index = peer_index;

    // Keep the cursor visible. Track scroll_offset separately on App so
    // pressing `j` past the bottom of the visible window actually scrolls;
    // the previous cap-by-min logic froze the cursor at row 0 until the
    // very end of the list.
    if peer_index < app.detail_peer_scroll_offset {
        app.detail_peer_scroll_offset = peer_index;
    }
    if visible_height > 0 && peer_index >= app.detail_peer_scroll_offset + visible_height {
        app.detail_peer_scroll_offset = peer_index + 1 - visible_height;
    }
    // Bound to [0, peer_count - visible_height] when the list shrinks.
    let max_offset = peer_count.saturating_sub(visible_height.max(1));
    if app.detail_peer_scroll_offset > max_offset {
        app.detail_peer_scroll_offset = max_offset;
    }
    let scroll_offset = app.detail_peer_scroll_offset;

    // Re-borrow immutably (no more `app` mutations below) and sort peer
    // *references* — avoids deep-cloning every PeerInfo (two owned Strings each)
    // on every render of this tab.
    let Some(t) = app.selected_torrent() else {
        return;
    };
    let mut sorted: Vec<&PeerInfo> = t.peers.iter().collect();
    sorted.sort_by_key(|p| std::cmp::Reverse(p.downloaded_bytes));

    let mut lines = vec![
        Line::from(vec![
            Span::styled("  Connected: ", Style::default().fg(theme.text_dim)),
            Span::raw(format!("{}", peers_connected)),
            Span::styled("  /  Total seen: ", Style::default().fg(theme.text_dim)),
            Span::raw(format!("{}", peers_total)),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            format!(
                "  {:<22} {:<12} {:>12} {:>8} {:>6}",
                "Address", "State", "Downloaded", "Pieces", "Errs"
            ),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )]),
    ];

    for (i, peer) in sorted
        .iter()
        .enumerate()
        .skip(scroll_offset)
        .take(visible_height)
    {
        let is_selected = i == peer_index;
        let prefix = if is_selected { "> " } else { "  " };
        let style = if is_selected {
            Style::default()
                .bg(theme.highlight)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        lines.push(Line::from(Span::styled(
            format!(
                "{}{:<22} {:<12} {:>12} {:>8} {:>6}",
                prefix,
                truncate(&peer.address, 22),
                truncate(&peer.state, 12),
                format_size(peer.downloaded_bytes),
                peer.pieces,
                peer.errors
            ),
            style,
        )));
    }

    let peers_widget = Paragraph::new(lines).block(
        Block::default()
            .title(format!(" Peers ({}) - j/k:scroll ", peer_count))
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.border)),
    );
    f.render_widget(peers_widget, area);
}
