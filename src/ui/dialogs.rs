use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame,
};

use crate::ui::theme::Theme;
use crate::ui::util::centered_rect;

/// Render the delete-confirmation popup. `watch_dir_configured` adds a line
/// making clear that both answers remove the `.torrent` from the watch folder —
/// otherwise `[K]eep files` reads as a promise the delete path no longer keeps.
///
/// That flag is wired from `config.general.watch_dir.is_some()` alone, so the
/// note also shows in the cases where the engine disables cleanup at startup
/// (watch folder equal to the download dir, unavailable, or a home/root path).
/// Erring toward the warning is deliberate; it is not a guarantee.
pub fn render_delete_dialog(
    f: &mut Frame,
    area: Rect,
    torrent_name: &str,
    watch_dir_configured: bool,
    theme: &Theme,
) {
    let mut popup = centered_rect(50, 25, area);
    // The percentage height is only 6 rows on an 80x24 terminal, which is
    // exactly the borders plus the four base lines. Grow it so the watch-folder
    // note is not clipped off the bottom.
    let needed = if watch_dir_configured { 8 } else { 6 };
    popup.height = popup.height.max(needed).min(area.height);
    f.render_widget(Clear, popup);

    let mut text = vec![
        Line::from(""),
        Line::from(Span::styled(
            // `torrent_name` is the object phrase from `delete_dialog_label`
            // (a quoted name or a mark count) — never prefix it here again.
            format!("  Delete {}?", torrent_name),
            Style::default().fg(theme.text),
        )),
    ];

    if watch_dir_configured {
        text.push(Line::from(Span::styled(
            "  Also removes the .torrent from your watch folder",
            Style::default().fg(theme.text_dim),
        )));
    }

    text.extend([
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  [K]",
                Style::default()
                    .fg(theme.download)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("eep files   "),
            Span::styled(
                "[D]",
                Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("elete files   "),
            Span::styled(
                "[C]",
                Style::default()
                    .fg(theme.warning)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("ancel"),
        ]),
    ]);

    let dialog = Paragraph::new(text).block(
        Block::default()
            .title(" Confirm Delete ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.error)),
    );
    f.render_widget(dialog, popup);
}

pub fn render_quit_dialog(f: &mut Frame, area: Rect, theme: &Theme) {
    let popup = centered_rect(40, 20, area);
    f.render_widget(Clear, popup);

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Active downloads in progress.",
            Style::default().fg(theme.text),
        )),
        Line::from(Span::styled(
            "  Really quit?",
            Style::default().fg(theme.text),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  [Y]",
                Style::default()
                    .fg(theme.error)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("es   "),
            Span::styled(
                "[N]",
                Style::default()
                    .fg(theme.download)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("o"),
        ]),
    ];

    let dialog = Paragraph::new(text).block(
        Block::default()
            .title(" Confirm Quit ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.warning)),
    );
    f.render_widget(dialog, popup);
}
