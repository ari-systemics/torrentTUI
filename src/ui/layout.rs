//! Frame skeleton, the status/filter/throttle bars, and the human-readable
//! formatters (`format_size`, `format_speed`, `format_eta`) shared by every
//! widget.
//!
//! The formatters live here rather than in `util` because the bars are their
//! main consumer and their rounding choices are display decisions rather than
//! general-purpose utilities.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Sparkline},
    Frame,
};

use crate::app::App;
use crate::types::AppMode;
use crate::ui::theme::Theme;

/// Split the frame into the three regions every mode shares: a 4-row header
/// (gradient title + session subtitle, with speed sparklines on the right), a
/// flexible main area (table or detail view), and a 3-row bottom bar (status,
/// filter, throttle prompt or add-torrent input, depending on mode).
///
/// Always three elements, in that order — main.rs indexes them positionally, so
/// returning a different count panics rather than misrendering. The main area
/// is `Min(5)`, so on a very short terminal ratatui shrinks the fixed bars
/// instead and downstream renderers must tolerate a zero-height inner area.
pub fn get_layout(area: Rect) -> Vec<Rect> {
    Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4), // Header
            Constraint::Min(5),    // Main area
            Constraint::Length(3), // Status bar
        ])
        .split(area)
        .to_vec()
}

/// Minimum main-area width for the vertical (side-by-side) split: the
/// downloads table alone wants ~90 columns to stay readable, so at 100 the
/// 60/40 cut leaves it ~58 and the search pane ~38 — tight but usable, and
/// anything less is not.
pub const SPLIT_VERTICAL_MIN_WIDTH: u16 = 100;
/// Minimum main-area height for the horizontal (stacked) split: 12 rows of
/// downloads over 8 of search. Below that one of the two panes collapses to
/// its borders and the full-area search modes are the better trade.
pub const SPLIT_HORIZONTAL_MIN_HEIGHT: u16 = 20;

/// Whether the terminal can fit the split-pane layout. `area` is the main
/// chunk (between header and status bar), `vertical` the orientation from
/// `App::split_vertical`. When this returns false the search pane falls back
/// to the full-area SearchInput/SearchResults modes, exactly as if the pane
/// had never been opened.
pub fn use_split_layout(area: Rect, vertical: bool) -> bool {
    if vertical {
        area.width >= SPLIT_VERTICAL_MIN_WIDTH
    } else {
        area.height >= SPLIT_HORIZONTAL_MIN_HEIGHT
    }
}

/// Cut the main area into `[downloads, search]` panes, 60/40, downloads
/// first. `vertical` puts them side by side (search on the right), otherwise
/// stacked (search on the bottom). Only call when `use_split_layout` passed —
/// below its thresholds one pane is all border and no content.
pub fn split_main(area: Rect, vertical: bool) -> Vec<Rect> {
    let direction = if vertical {
        Direction::Horizontal
    } else {
        Direction::Vertical
    };
    Layout::default()
        .direction(direction)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area)
        .to_vec()
}

/// Width reserved for the two header sparklines (label column included).
const SPARKLINE_SECTION_WIDTH: u16 = 28;
/// Below this inner width the sparkline section is dropped entirely and the
/// title takes the full header.
const SPARKLINE_MIN_WIDTH: u16 = 60;

pub fn render_header(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);
    // Zero-width/height inner area (tiny terminal): nothing to lay out.
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // The sparkline strip only earns its 28 columns when there is room for
    // it; on narrow terminals the title and totals take priority.
    let show_sparklines = inner.width >= SPARKLINE_MIN_WIDTH && inner.height >= 2;
    let (left, right) = if show_sparklines {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(SPARKLINE_SECTION_WIDTH),
            ])
            .split(inner);
        (cols[0], Some(cols[1]))
    } else {
        (inner, None)
    };

    // Title: one span per character, walking `header_gradient` across the
    // word. An empty gradient falls back to the flat accent color.
    const TITLE: &str = "TorrentTUI";
    let denom = TITLE.len().saturating_sub(1).max(1) as f32;
    let mut title_spans: Vec<Span> = TITLE
        .chars()
        .enumerate()
        .map(|(i, c)| {
            let color = if theme.header_gradient.is_empty() {
                theme.accent
            } else {
                crate::ui::theme::lerp_color(&theme.header_gradient, i as f32 / denom)
            };
            Span::styled(
                c.to_string(),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )
        })
        .collect();
    title_spans.push(Span::styled(
        concat!(" v", env!("CARGO_PKG_VERSION")),
        Style::default().fg(theme.text_dim),
    ));

    // Subtitle: session totals and the active/total count, dim.
    let subtitle = Line::from(vec![
        Span::styled(
            format!("\u{2193}{}", format_size(app.total_downloaded_bytes())),
            Style::default().fg(theme.download),
        ),
        Span::raw("  "),
        Span::styled(
            format!("\u{2191}{}", format_size(app.total_uploaded_bytes())),
            Style::default().fg(theme.upload),
        ),
        Span::styled(
            format!(
                "  {} active / {} total",
                app.active_count(),
                app.torrents.len()
            ),
            Style::default().fg(theme.text_dim),
        ),
    ]);

    f.render_widget(
        Paragraph::new(vec![Line::from(title_spans), subtitle]),
        left,
    );

    if let Some(right) = right {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(right);
        for (row, history, color, label) in [
            (rows[0], &app.down_history, theme.download, "\u{2193}"),
            (rows[1], &app.up_history, theme.upload, "\u{2191}"),
        ] {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(2), Constraint::Min(0)])
                .split(row);
            f.render_widget(
                Paragraph::new(Span::styled(label, Style::default().fg(color))),
                cols[0],
            );
            // `Sparkline::data` takes any IntoIterator of &u64; an empty
            // history renders as a blank strip, which is the right look
            // before the first engine push arrives.
            let sparkline = Sparkline::default()
                .data(history.iter())
                .style(Style::default().fg(color));
            f.render_widget(sparkline, cols[1]);
        }
    }
}

/// Fewer than this many whole keys in the footer isn't worth keeping next
/// to the status facts; the least critical fact gets traded away first,
/// and below the threshold the hints hide entirely ('?' lists them all).
const MIN_HINT_KEYS: usize = 2;

pub fn render_status_bar(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    if let Some(ref err) = app.error_message {
        let error = Paragraph::new(Line::from(vec![Span::styled(
            err.clone(),
            Style::default().fg(theme.error),
        )]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.error)),
        );
        f.render_widget(error, area);
        return;
    }

    if let Some(ref info) = app.info_message {
        let info_widget = Paragraph::new(Line::from(vec![Span::styled(
            info.clone(),
            Style::default().fg(theme.info),
        )]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.info)),
        );
        f.render_widget(info_widget, area);
        return;
    }

    let down_speed = format_speed(app.total_download_speed());
    let up_speed = format_speed(app.total_upload_speed());
    let active = app.active_count();
    let total = app.torrents.len();

    let hints = match app.mode {
        AppMode::Normal if app.search_has_focus() => {
            // Focus lives in the search pane: j/k move its cursor and Enter
            // adds a hit, so showing download hints here ("d:delete") would
            // invite deleting a download while aiming at a search result.
            "j/k:navigate hits  Enter:add  g:group  s:search  w:focus table".to_string()
        }
        AppMode::Normal => {
            let pane_suffix = if app.search_pane_open {
                "  w:panes  |:split"
            } else {
                ""
            };
            if app.torrents.is_empty() {
                format!("a:add  s:search  /:filter  ?:help  q:quit{pane_suffix}")
            } else {
                format!("a:add  s:search  Space:mark  p:(un)pause  d:delete  Enter:detail  /:filter  t:throttle  ?:help  q:quit{pane_suffix}")
            }
        }
        AppMode::Input => "Enter:submit  Esc:cancel".to_string(),
        AppMode::Detail => {
            "Tab:switch tab  j/k:navigate  Space:toggle  S:apply  Esc:back".to_string()
        }
        AppMode::Help => "Esc/?:close".to_string(),
        AppMode::ConfirmDelete => "k:keep files  d:delete files  c:cancel".to_string(),
        AppMode::ConfirmQuit => "y:quit  n:cancel".to_string(),
        AppMode::Filter => "Enter:apply  Esc:clear & close".to_string(),
        AppMode::ThrottleInput => "Enter:confirm  Esc:cancel".to_string(),
        AppMode::SearchInput => {
            if app.search_pane_open {
                // Esc only leaves the prompt — the pane and its hits stay.
                "Enter:search  Esc:back to results".to_string()
            } else {
                "Enter:search  Esc:cancel".to_string()
            }
        }
        AppMode::SearchResults => {
            if app.search_pane_open {
                "Enter:add  j/k:navigate  g:group  s:new search  Esc:close pane".to_string()
            } else {
                "Enter:add  j/k:navigate  s:new search  Esc:back".to_string()
            }
        }
    };

    // Build right-aligned speed section
    let mut right_spans = vec![Span::styled(
        format!("\u{2193} {}", down_speed),
        Style::default().fg(theme.download),
    )];
    if app.speed_limit_download_kbps > 0 {
        right_spans.push(Span::styled(
            format!(
                " [{}]",
                format_speed(app.speed_limit_download_kbps.saturating_mul(1024))
            ),
            Style::default().fg(theme.text_dim),
        ));
    }
    right_spans.push(Span::raw("  "));

    let total_up = app.total_uploaded_bytes();
    let total_down = app.total_downloaded_bytes();
    if total_up > 0 || total_down > 0 {
        let ratio = if total_down > 0 {
            total_up as f64 / total_down as f64
        } else {
            0.0
        };
        right_spans.push(Span::styled(
            format!("R:{:.2}  ", ratio),
            Style::default().fg(theme.text_dim),
        ));
    }

    right_spans.push(Span::styled(
        format!("\u{2191} {} ", up_speed),
        Style::default().fg(theme.upload),
    ));
    if app.speed_limit_upload_kbps > 0 {
        right_spans.push(Span::styled(
            format!(
                "[{}] ",
                format_speed(app.speed_limit_upload_kbps.saturating_mul(1024))
            ),
            Style::default().fg(theme.text_dim),
        ));
    }

    // Calculate width of speed section for the right column
    // Use character count, not byte length: the ↓/↑ arrows are 3 UTF-8 bytes
    // but occupy a single terminal column, so `.len()` over-reserves the right
    // column by ~4 columns. All glyphs here are single-width, so chars().count()
    // is exact (use unicode_width if wide CJK ever appears in this section).
    let right_text_width: u16 = right_spans
        .iter()
        .map(|s| s.content.chars().count() as u16)
        .sum();

    // Build left section: hints, counts, then optional facts. Everything is
    // measured against the width left over after the speed column so a narrow
    // split degrades gracefully — shorten the hints first, then drop the
    // least critical fact — instead of clipping mid-word ("110 GB fre").
    const SEP: &str = "  \u{2502}  ";
    let text_w = |s: &str| unicode_width::UnicodeWidthStr::width(s) as u16;

    let counts = format!("{} active / {} total", active, total);
    let mandatory_w = text_w(SEP) + text_w(&counts);

    // Optional status facts with a drop rank: queue backlog goes first,
    // the disk-free readout (which can turn into a red low-space warning)
    // survives longest.
    let mut optionals: Vec<(u8, String, Style)> = Vec::new();
    let queued = app.queued_count();
    if queued > 0 {
        // A permanent `Q:0` would be noise for the unlimited default config.
        optionals.push((0, format!("Q:{queued}"), theme.status_queued.into()));
    }
    if app.has_marks() {
        optionals.push((
            1,
            format!("{} marked", app.marked_count()),
            theme.accent.into(),
        ));
    }
    if !app.filter_text.is_empty() && app.mode != AppMode::Filter {
        optionals.push((
            2,
            format!("filter: \"{}\"", app.filter_text),
            theme.info.into(),
        ));
    }
    if let Some(space) = app.free_disk_space {
        let style = if space < 1_073_741_824 {
            theme.error
        } else {
            theme.text_dim
        };
        optionals.push((3, format!("{} free", format_size(space)), style.into()));
    }

    // Bordered inner width; matches the `block.inner(area)` computed below.
    let budget = area
        .width
        .saturating_sub(2)
        .saturating_sub(right_text_width);
    let stats_w = |kept: &[(u8, String, Style)]| -> u16 {
        mandatory_w
            + kept
                .iter()
                .map(|(_, s, _)| text_w(SEP) + text_w(s))
                .sum::<u16>()
    };

    // Whole `key:action` tokens only — "Enter:add  j…" is noise, fewer
    // complete keys read fine ('?' still lists everything). Returns None
    // when not even one key fits.
    let fit_hints = |room: u16| -> Option<String> {
        let mut cur = String::new();
        for token in hints.split("  ") {
            if token.is_empty() {
                continue;
            }
            let extra = text_w(token) + if cur.is_empty() { 0 } else { 2 };
            if text_w(&cur) + extra > room {
                break;
            }
            if !cur.is_empty() {
                cur.push_str("  ");
            }
            cur.push_str(token);
        }
        (!cur.is_empty()).then_some(cur)
    };

    let mut kept: &[(u8, String, Style)] = &optionals;
    // Every loop exit assigns this before `break`.
    let shown_hints;
    loop {
        let room = budget.saturating_sub(stats_w(kept)).saturating_sub(1);
        if let Some(fitted) = fit_hints(room) {
            // One lonely key next to four facts isn't worth the column;
            // trade the least critical fact away first.
            let enough = fitted.split("  ").count() >= MIN_HINT_KEYS || kept.is_empty();
            if enough {
                shown_hints = fitted;
                break;
            }
        }
        if kept.is_empty() {
            shown_hints = fit_hints(budget).unwrap_or_default();
            break;
        }
        kept = &kept[1..];
    }

    let mut left_spans = vec![
        Span::styled(
            if shown_hints.is_empty() {
                String::new()
            } else {
                format!(" {shown_hints}")
            },
            Style::default().fg(theme.text_dim),
        ),
        Span::raw(SEP),
        Span::raw(counts),
    ];
    for (_, text, style) in kept {
        left_spans.push(Span::raw(SEP));
        left_spans.push(Span::styled(text.clone(), *style));
    }

    // Split into two columns: left fills, right is fixed-width for speeds
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.border));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // On a very narrow terminal the bordered inner area collapses to zero
    // width/height. ratatui renders that safely, but there's nothing to lay
    // out, so skip the column split and paragraph rendering entirely.
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(right_text_width)])
        .split(inner);

    let left_widget = Paragraph::new(Line::from(left_spans));
    let right_widget =
        Paragraph::new(Line::from(right_spans)).alignment(ratatui::layout::Alignment::Right);

    f.render_widget(left_widget, columns[0]);
    f.render_widget(right_widget, columns[1]);
}

pub fn render_filter_bar(f: &mut Frame, area: Rect, filter_text: &str, theme: &Theme) {
    let line = Line::from(vec![
        Span::styled(" Filter: ", Style::default().fg(theme.accent)),
        Span::raw(filter_text),
        Span::styled("\u{2588}", Style::default().fg(theme.text)), // cursor
    ]);

    let bar = Paragraph::new(line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.accent)),
    );
    f.render_widget(bar, area);
}

pub fn render_throttle_bar(f: &mut Frame, area: Rect, step: u8, input_buf: &str, theme: &Theme) {
    let prompt = if step == 0 {
        " Download limit (KB/s, 0=unlimited): "
    } else {
        " Upload limit (KB/s, 0=unlimited): "
    };

    let line = Line::from(vec![
        Span::styled(prompt, Style::default().fg(theme.accent)),
        Span::raw(input_buf),
        Span::styled("\u{2588}", Style::default().fg(theme.text)),
    ]);

    let bar = Paragraph::new(line).block(
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.accent)),
    );
    f.render_widget(bar, area);
}

/// Format a byte-per-second rate for display. Divides by 1024 at each step but
/// labels the result KB/MB/GB, matching what other torrent clients show rather
/// than being strictly correct about KiB. Callers holding a KB/s value from the
/// throttle config must multiply by 1024 first.
pub fn format_speed(bytes_per_sec: u64) -> String {
    if bytes_per_sec == 0 {
        return "0 B/s".to_string();
    }
    let kb = bytes_per_sec as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{:.1} KB/s", kb);
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{:.1} MB/s", mb);
    }
    let gb = mb / 1024.0;
    format!("{:.2} GB/s", gb)
}

/// Format a byte count for display, with the same 1024-per-step, KB-labelled
/// convention as `format_speed`. Anything under 1 MB is rendered as whole KB,
/// so sub-kilobyte files show as `0 KB` rather than in bytes — fine for torrent
/// payloads, wrong if this is ever reused for small values.
pub fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{:.0} KB", kb);
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format!("{:.1} MB", mb);
    }
    let gb = mb / 1024.0;
    format!("{:.2} GB", gb)
}

/// Compact size for tight table columns: never wider than five columns, so a
/// squeezed pane loses padding — not the unit ("595." instead of "595.3 MB").
pub fn format_size_short(bytes: u64) -> String {
    if bytes < 1024 {
        // Sub-kilobyte sizes print exactly; rounding 512B up to "1K" would
        // misstate free space by 100%.
        return format!("{bytes}B");
    }
    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        return format!("{kb:.0}K");
    }
    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return if mb < 100.0 {
            format!("{mb:.1}M")
        } else {
            format!("{mb:.0}M")
        };
    }
    let gb = mb / 1024.0;
    if gb < 1024.0 {
        return if gb < 100.0 {
            format!("{gb:.1}G")
        } else {
            format!("{gb:.0}G")
        };
    }
    format!("{:.0}T", gb / 1024.0)
}

pub fn format_eta(seconds: Option<u64>) -> String {
    match seconds {
        None => "\u{2014}".to_string(),
        Some(0) => "\u{2014}".to_string(),
        Some(s) => {
            let hours = s / 3600;
            let mins = (s % 3600) / 60;
            let secs = s % 60;
            if hours > 0 {
                format!("{}h {}m", hours, mins)
            } else if mins > 0 {
                format!("{}m {}s", mins, secs)
            } else {
                format!("{}s", secs)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_layout_thresholds_vertical() {
        // Side-by-side needs the width; height is irrelevant to the verdict.
        let wide = Rect::new(0, 0, SPLIT_VERTICAL_MIN_WIDTH, 5);
        let narrow = Rect::new(0, 0, SPLIT_VERTICAL_MIN_WIDTH - 1, 50);
        assert!(use_split_layout(wide, true));
        assert!(!use_split_layout(narrow, true));
    }

    #[test]
    fn split_layout_thresholds_horizontal() {
        // Stacked needs the height; width is irrelevant to the verdict.
        let tall = Rect::new(0, 0, 40, SPLIT_HORIZONTAL_MIN_HEIGHT);
        let short = Rect::new(0, 0, 200, SPLIT_HORIZONTAL_MIN_HEIGHT - 1);
        assert!(use_split_layout(tall, false));
        assert!(!use_split_layout(short, false));
    }

    #[test]
    fn split_main_returns_downloads_then_search() {
        let area = Rect::new(0, 0, 100, 30);
        let panes = split_main(area, true);
        assert_eq!(panes.len(), 2);
        assert_eq!(panes[0].width, 60);
        assert_eq!(panes[1].x, 60);
        let stacked = split_main(area, false);
        assert_eq!(stacked[0].height, 18);
        assert_eq!(stacked[1].y, 18);
    }

    #[test]
    fn short_size_stays_within_five_columns() {
        // The whole point of the short format: a squeezed column must clip
        // padding, never the unit or digits.
        assert_eq!(format_size_short(0), "0B");
        assert_eq!(format_size_short(512), "512B"); // exact below 1 KB
        assert_eq!(format_size_short(1024 * 1024), "1.0M");
        assert_eq!(format_size_short(600 * 1024 * 1024), "600M");
        assert_eq!(
            format_size_short((2.85f64 * 1024.0 * 1024.0 * 1024.0) as u64),
            "2.8G"
        );
        assert_eq!(format_size_short(595 * 1024 * 1024), "595M");
        for bytes in [
            1u64,
            999,
            1023 * 1024,
            1024 * 1024,
            999 * 1024 * 1024,
            1024u64 * 1024 * 1024,
            100 * 1024 * 1024 * 1024,
            5 * 1024 * 1024 * 1024 * 1024,
        ] {
            assert!(format_size_short(bytes).chars().count() <= 5, "{}", bytes);
        }
    }

    #[test]
    fn speed_zero() {
        assert_eq!(format_speed(0), "0 B/s");
    }

    #[test]
    fn speed_kilobytes() {
        assert_eq!(format_speed(1536), "1.5 KB/s");
        assert_eq!(format_speed(1024), "1.0 KB/s");
    }

    #[test]
    fn speed_megabytes() {
        assert_eq!(format_speed(1024 * 1024), "1.0 MB/s");
    }

    #[test]
    fn speed_gigabytes() {
        assert_eq!(format_speed(1024 * 1024 * 1024), "1.00 GB/s");
    }

    #[test]
    fn size_zero() {
        assert_eq!(format_size(0), "0 B");
    }

    #[test]
    fn size_kilobytes() {
        assert_eq!(format_size(2048), "2 KB");
    }

    #[test]
    fn size_megabytes() {
        assert_eq!(format_size(1024 * 1024), "1.0 MB");
    }

    #[test]
    fn size_gigabytes() {
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn eta_none() {
        assert_eq!(format_eta(None), "\u{2014}");
    }

    #[test]
    fn eta_zero() {
        assert_eq!(format_eta(Some(0)), "\u{2014}");
    }

    #[test]
    fn eta_seconds_only() {
        assert_eq!(format_eta(Some(45)), "45s");
    }

    #[test]
    fn eta_minutes_seconds() {
        assert_eq!(format_eta(Some(125)), "2m 5s");
    }

    #[test]
    fn eta_hours_minutes() {
        assert_eq!(format_eta(Some(3661)), "1h 1m");
    }
}
