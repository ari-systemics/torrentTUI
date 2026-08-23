use crate::ui::theme::Theme;
use crate::ui::util::centered_rect;
use ratatui::{
    layout::{Constraint, Rect},
    style::{Modifier, Style},
    widgets::{Block, BorderType, Borders, Cell, Clear, Row, Table},
    Frame,
};

/// Draw the keybinding overlay. The table is hand-maintained and has no link
/// to the handlers in main.rs or the hint strings in `render_status_bar`, so a
/// new keybinding needs editing in all three places — nothing here fails if
/// they drift.
pub fn render_help(f: &mut Frame, area: Rect, theme: &Theme) {
    let popup = centered_rect(60, 80, area);
    f.render_widget(Clear, popup);

    let keybindings = vec![
        ("a", "Add magnet link or .torrent file"),
        ("s", "Open search pane / search online"),
        ("w", "Cycle pane focus (split search)"),
        ("|", "Toggle split orientation"),
        ("p", "(Un)pause selected torrent"),
        ("P", "(Un)pause all torrents"),
        ("d", "Delete selected torrent"),
        ("Enter", "Open detail view"),
        ("j / \u{2193}", "Move selection down"),
        ("k / \u{2191}", "Move selection up"),
        ("Tab", "Cycle sort column / detail tab"),
        ("r", "Reverse sort order"),
        ("/", "Search/filter torrents"),
        ("t", "Set speed limits"),
        ("T", "Cycle color theme (this session;"),
        ("", "set preset in config to keep)"),
        ("Space", "Mark/unmark torrent"),
        ("v", "Mark all visible"),
        ("V", "Clear all marks"),
        ("?", "Toggle this help"),
        ("q / Esc", "Quit / back"),
        ("", ""),
        ("", "Queue: cap parallel downloads via"),
        ("", "max_concurrent_downloads in config.toml;"),
        ("", "'p' on a Queued torrent starts it now"),
        ("", ""),
        ("", "Groups: [[groups]] name + dir tables in"),
        ("", "config.toml route adds into sub-folders"),
        ("", "of download_dir (set at add time only)"),
        ("", ""),
        ("", "-- Detail View --"),
        ("j / k", "Navigate files / peers"),
        ("Space", "Toggle file selection"),
        ("S", "Apply file selection"),
        ("s", "Stream selected file in default player"),
        ("", ""),
        ("", "-- Search Results --"),
        ("j / k", "Navigate results"),
        ("Enter", "Add selected result as download"),
        ("g", "Cycle target download group"),
        ("s", "New search (pre-fills the query)"),
        ("Esc", "Close the search pane"),
    ];

    let rows: Vec<Row> = keybindings
        .iter()
        .map(|(key, action)| {
            Row::new(vec![
                Cell::from(*key).style(
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(*action),
            ])
        })
        .collect();

    let table = Table::new(rows, [Constraint::Length(15), Constraint::Min(30)])
        .block(
            Block::default()
                .title(" Help \u{2014} Keybindings ")
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme.accent)),
        )
        .style(Style::default().fg(theme.text));

    f.render_widget(table, popup);
}
