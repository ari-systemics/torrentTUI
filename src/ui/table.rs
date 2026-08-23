use ratatui::{
    layout::Constraint,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Row, Table},
    Frame,
};

use crate::app::App;
use crate::types::{Pane, TorrentStatus};
use crate::ui::layout::{format_eta, format_size, format_speed};
use crate::ui::progress::{render_progress_bar_styled, SPINNER_FRAMES};
use crate::ui::theme::Theme;

const HEADER_LABELS: [&str; 9] = [
    "#",
    "Name",
    "Group",
    "Size",
    "Progress",
    "\u{2193} Speed",
    "Peers",
    "ETA",
    "Status",
];

/// Deterministic per-group color: hash the name into the theme's progress
/// gradient stops, so a group keeps its color across sessions and two groups
/// usually differ. `DefaultHasher::new()` (unlike `RandomState`) has fixed
/// keys, which is what makes the pick stable run to run. An empty gradient
/// (a theme can disable it) falls back to the accent.
pub(crate) fn group_color(name: &str, theme: &Theme) -> Color {
    use std::hash::Hasher;
    let stops = &theme.progress_gradient;
    if stops.is_empty() {
        return theme.accent;
    }
    let mut dh = std::collections::hash_map::DefaultHasher::new();
    dh.write(name.as_bytes());
    stops[(dh.finish() as usize) % stops.len()]
}

/// Which column preset fits the pane, chosen from the *inner* width so the
/// borders never eat into the budget. Ratatui squeezes every column when the
/// constraints overflow the area, which used to clip cells mid-value in the
/// split view ("Progr", "Statu", sizes without units) — a preset that fits
/// exactly reads better than nine columns that all truncate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TableTier {
    /// All nine columns — full-width window.
    Full,
    /// Group and ETA dropped; fits a 60% split pane down to ~100-col terminals.
    Medium,
    /// # / Name / Size / Progress / Status only; smallest split panes.
    Compact,
}

pub(crate) fn table_tier(inner_width: u16) -> TableTier {
    if inner_width >= 104 {
        TableTier::Full
    } else if inner_width >= 74 {
        TableTier::Medium
    } else {
        TableTier::Compact
    }
}

/// `(real HEADER_LABELS index, constraint)` per tier, plus the progress-bar
/// width. Real indexes keep the sort arrow on the correct heading in every
/// preset. The Name column is `Min` and absorbs whatever is left.
fn columns_for(tier: TableTier) -> (Vec<(usize, Constraint)>, u16) {
    use Constraint::{Length, Min};
    match tier {
        TableTier::Full => (
            vec![
                (0, Length(5)),  // #
                (1, Min(20)),    // Name
                (2, Length(10)), // Group
                (3, Length(10)), // Size
                (4, Length(24)), // Progress
                (5, Length(12)), // ↓ Speed
                (6, Length(8)),  // Peers
                (7, Length(10)), // ETA
                (8, Length(18)), // Status
            ],
            15,
        ),
        TableTier::Medium => (
            vec![
                (0, Length(5)),  // #
                (1, Min(14)),    // Name
                (3, Length(10)), // Size
                (4, Length(16)), // Progress
                (5, Length(11)), // ↓ Speed
                (6, Length(8)),  // Peers
                (8, Length(13)), // Status
            ],
            14,
        ),
        TableTier::Compact => (
            vec![
                (0, Length(4)),  // #
                (1, Min(8)),     // Name
                (3, Length(9)),  // Size
                (4, Length(12)), // Progress
                (8, Length(11)), // Status
            ],
            10,
        ),
    }
}

pub fn render_table(f: &mut Frame, area: ratatui::layout::Rect, app: &mut App) {
    let theme = app.theme.clone();
    let sorted = app.sorted_torrents();

    // Tier comes from the bordered inner width; see `table_tier`.
    let inner_width = area.width.saturating_sub(2);
    let (columns, progress_width) = columns_for(table_tier(inner_width));

    // The accent border is the focus seam: with the search pane open and
    // focused, this pane drops to the plain border color. Pane closed (or
    // downloads focused) keeps the accent, which is also the pre-split look.
    let border_color = if app.search_pane_open && app.focused == Pane::Search {
        theme.border
    } else {
        theme.accent
    };

    if sorted.is_empty() && app.pending_adds.is_empty() {
        let msg = if app.filter_text.is_empty() {
            "No torrents. Press 'a' to add a magnet link or .torrent file."
        } else {
            "No torrents match the current filter."
        };
        let empty_msg = ratatui::widgets::Paragraph::new(Line::from(vec![Span::styled(
            msg,
            Style::default().fg(theme.text_dim),
        )]))
        .block(
            Block::default()
                .title(" Downloads ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border_color)),
        )
        .centered();
        f.render_widget(empty_msg, area);
        return;
    }

    let sort_col = app.sort_column;
    let sort_rev = app.sort_reversed;
    let spinner_tick = app.spinner_tick;
    let marked_ids = &app.marked_ids;

    // Build header with sort indicator. Cells come from the tier's real
    // column indexes so a dropped column can never shift the arrow.
    let header_cells = columns.iter().map(|(i, _)| {
        let h = HEADER_LABELS[*i];
        let label = if sort_col.column_index() == *i {
            let arrow = if sort_rev { "\u{25bc}" } else { "\u{25b2}" };
            format!("{h} {arrow}")
        } else {
            h.to_string()
        };
        Cell::from(label).style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    });
    let header = Row::new(header_cells).height(1);

    // Build rows from sorted view (all owned data)
    let rows: Vec<Row> = sorted
        .iter()
        .map(|torrent| {
            let is_marked = marked_ids.contains(&torrent.id);
            let percent = torrent.progress_percent();
            let progress_cell = match torrent.status {
                TorrentStatus::FetchingMetadata => Cell::from(Line::from(Span::styled(
                    format!("{} Fetching...", SPINNER_FRAMES[spinner_tick]),
                    Style::default().fg(theme.status_fetching),
                ))),
                _ => Cell::from(render_progress_bar_styled(
                    percent,
                    progress_width as usize,
                    &theme,
                )),
            };

            let (status_text, status_style) =
                status_cell_style(&torrent.status, torrent.throttle_managed, &theme);

            let id_text = if is_marked {
                format!("\u{25cf} {}", torrent.id)
            } else {
                format!("  {}", torrent.id)
            };

            let group_cell = match torrent.group.as_deref() {
                Some(g) => {
                    Cell::from(g.to_string()).style(Style::default().fg(group_color(g, &theme)))
                }
                None => Cell::from("\u{00b7}").style(Style::default().fg(theme.text_dim)),
            };

            // Every column is always built; the tier's index list picks the
            // visible subset, so a preset change can never reorder or
            // misalign a cell.
            let all_cells = [
                Cell::from(id_text),
                Cell::from(torrent.name.clone()),
                group_cell,
                Cell::from(format_size(torrent.size_bytes)),
                progress_cell,
                Cell::from(format_speed(torrent.download_speed)),
                Cell::from(format!(
                    "{}/{}",
                    torrent.peers_connected, torrent.peers_total
                )),
                Cell::from(format_eta(torrent.eta_seconds)),
                Cell::from(status_text).style(status_style),
            ];
            let row = Row::new(
                columns
                    .iter()
                    .map(|(i, _)| all_cells[*i].clone())
                    .collect::<Vec<_>>(),
            );

            if is_marked {
                row.style(Style::default().bg(theme.marked_bg))
            } else {
                row
            }
        })
        .collect();

    // Drop sorted to release the immutable borrow on app
    drop(sorted);

    let table = Table::new(rows, columns.into_iter().map(|(_, c)| c))
        .header(header)
        .block(
            // Focus seam — see the top of this function.
            Block::default()
                .title(" Downloads ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(border_color)),
        )
        .row_highlight_style(
            Style::default()
                .bg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("\u{25b6} ");

    f.render_stateful_widget(table, area, &mut app.table_state);
}

/// Map a torrent's status (and the throttle-managed override) to the cell text
/// and style. Being under a speed limit takes precedence over the underlying
/// engine state, so a torrent mid duty cycle reads as "Throttled" rather than
/// blinking between Downloading and Paused every tick.
pub fn status_cell_style(
    status: &TorrentStatus,
    throttle_managed: bool,
    theme: &Theme,
) -> (String, Style) {
    if throttle_managed {
        return ("Throttled".to_string(), Style::default().fg(theme.accent));
    }
    let style = match status {
        TorrentStatus::Downloading => Style::default().fg(theme.status_downloading),
        TorrentStatus::Queued => Style::default().fg(theme.status_queued),
        TorrentStatus::Complete => Style::default().fg(theme.status_complete),
        TorrentStatus::Seeding => Style::default().fg(theme.status_seeding),
        TorrentStatus::Paused => Style::default().fg(theme.status_paused),
        TorrentStatus::FetchingMetadata => Style::default().fg(theme.status_fetching),
        TorrentStatus::Error(_) => Style::default().fg(theme.status_error),
    };
    (status.to_string(), style)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_fit_their_minimum_panes() {
        // Each preset's fixed columns + highlight symbol + inter-column
        // spacing must leave the Name `Min` room inside the smallest pane
        // that selects it. Split panes bottom out near 58 inner columns
        // (60% of a 100-col terminal) and top out under 104 (full window).
        for (inner_width, expected) in [
            (120u16, TableTier::Full),
            (90, TableTier::Medium),
            (58, TableTier::Compact),
        ] {
            let tier = table_tier(inner_width);
            assert_eq!(tier, expected, "width {inner_width}");
            let (cols, _) = columns_for(tier);
            // Only `Length` columns are fixed; Name absorbs the remainder.
            let fixed: u16 = cols
                .iter()
                .filter_map(|(_, c)| match c {
                    Constraint::Length(w) => Some(*w),
                    _ => None,
                })
                .sum();
            let chrome = 2 /* highlight symbol */ + (cols.len() as u16 - 1) /* spacing */;
            assert!(
                fixed + chrome + 8 <= inner_width,
                "tier {expected:?} needs {} columns but pane is only {inner_width}",
                fixed + chrome + 8
            );
        }
    }

    #[test]
    fn every_preset_starts_at_the_id_column_and_keeps_name() {
        for tier in [TableTier::Full, TableTier::Medium, TableTier::Compact] {
            let (cols, _) = columns_for(tier);
            assert_eq!(cols[0].0, 0, "{tier:?}");
            assert_eq!(cols[1].0, 1, "{tier:?}");
        }
    }

    #[test]
    fn throttle_managed_overrides_status() {
        let (text, style) = status_cell_style(&TorrentStatus::Downloading, true, &Theme::default());
        assert_eq!(text, "Throttled");
        assert_eq!(style, Style::default().fg(Theme::default().accent));
    }

    #[test]
    fn downloading_uses_the_theme_status_color() {
        let theme = Theme::default();
        let (text, style) = status_cell_style(&TorrentStatus::Downloading, false, &theme);
        assert_eq!(text, "Downloading");
        assert_eq!(style, Style::default().fg(theme.status_downloading));
    }

    #[test]
    fn queued_uses_the_theme_status_color() {
        let theme = Theme::default();
        let (text, style) = status_cell_style(&TorrentStatus::Queued, false, &theme);
        assert_eq!(text, "Queued");
        assert_eq!(style, Style::default().fg(theme.status_queued));
    }

    #[test]
    fn complete_and_seeding_have_distinct_theme_colors() {
        // Complete and Seeding used to share green; the theme splits them so an
        // actively uploading torrent reads differently from an idle finished one.
        let theme = Theme::default();
        let (_, c) = status_cell_style(&TorrentStatus::Complete, false, &theme);
        let (_, s) = status_cell_style(&TorrentStatus::Seeding, false, &theme);
        assert_eq!(c, Style::default().fg(theme.status_complete));
        assert_eq!(s, Style::default().fg(theme.status_seeding));
    }

    #[test]
    fn error_text_includes_message() {
        let (text, _) = status_cell_style(
            &TorrentStatus::Error("disk full".to_string()),
            false,
            &Theme::default(),
        );
        assert!(text.contains("disk full"));
    }

    #[test]
    fn header_labels_match_sort_column_positions() {
        // The sort arrow is drawn by `SortColumn::column_index()`, so a drift
        // between this table and the enum puts the arrow on the wrong heading.
        assert_eq!(
            HEADER_LABELS[crate::types::SortColumn::Group.column_index()],
            "Group"
        );
        assert_eq!(
            HEADER_LABELS[crate::types::SortColumn::Status.column_index()],
            "Status"
        );
        assert_eq!(HEADER_LABELS.len(), 9);
    }

    #[test]
    fn group_color_is_deterministic_and_in_the_gradient() {
        let theme = Theme::default();
        assert!(!theme.progress_gradient.is_empty());
        let c1 = group_color("linux", &theme);
        let c2 = group_color("linux", &theme);
        assert_eq!(c1, c2, "same name must pick the same stop");
        assert!(theme.progress_gradient.contains(&c1));
    }

    #[test]
    fn group_color_falls_back_to_accent_without_a_gradient() {
        let theme = Theme {
            progress_gradient: Vec::new(),
            ..Theme::default()
        };
        assert_eq!(group_color("linux", &theme), theme.accent);
    }
}
