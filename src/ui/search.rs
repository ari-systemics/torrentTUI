//! The search surface, in two shapes that share one body: the full-area
//! results view (pane closed or small terminal) and the split-pane variant
//! (`render_search_pane`) that adds a one-line query strip on top and keys
//! its border off the focused pane. Both delegate to `render_results`, so
//! the table of hits — provider-ordered (seeders desc, deduped), cursor a
//! plain index, no sort or filter — can never drift between them.

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Cell, Paragraph, Row, Table},
    Frame,
};

use crate::app::App;
use crate::types::Pane;
use crate::ui::input::InputWidget;
use crate::ui::layout::{format_size, format_size_short};
use crate::ui::progress::SPINNER_FRAMES;
use crate::ui::theme::Theme;
use crate::ui::util::{sanitize_display, truncate};

const HEADER_LABELS: [&str; 6] = ["#", "Name", "Size", "Seeds", "Leechs", "Source"];

// Widths of every column except Name, plus the borders and inter-column
// spacing, so the Name budget below comes out in terminal columns.
const FIXED_COLUMNS_WIDTH: usize = 4 + 10 + 7 + 7 + 12;
const CHROME_WIDTH: usize = FIXED_COLUMNS_WIDTH + 2 /* borders */ + 5 /* spacing */;

/// The bordered block both search surfaces share. The title carries the add
/// target: the group `g` last cycled to, or the plain download dir when
/// ungrouped.
fn search_block(app: &App, border_color: Color) -> Block<'static> {
    let target = app.search_group.as_deref().unwrap_or("downloads");
    let title = format!(" Search: {} \u{2192} {} ", app.search_query, target);
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
}

/// The full-area results view, used when the search pane is closed or the
/// terminal is too small for the split (`use_split_layout`).
pub fn render_search_results(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme.clone();
    let block = search_block(app, theme.border);
    render_results(f, area, app, &theme, block);
}

/// The search half of the split layout: a one-line query strip on top
/// (editable only in SearchInput mode, where `input_active` is set) over the
/// same results table the full-area view draws. The block border is the
/// focus seam — accent when the pane holds focus, plain otherwise.
pub fn render_search_pane(
    f: &mut Frame,
    area: Rect,
    app: &mut App,
    input: &InputWidget,
    input_active: bool,
) {
    let theme = app.theme.clone();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(area);

    let prompt = Span::styled("\u{203a} ", Style::default().fg(theme.accent));
    let query_line = if input_active {
        // Editable strip: live buffer plus the same block cursor the
        // full-width search bar uses.
        let value = input.value();
        if value.is_empty() {
            Line::from(vec![
                prompt,
                Span::styled("type a query", Style::default().fg(theme.text_dim)),
                Span::styled("\u{2588}", Style::default().fg(theme.text)),
            ])
        } else {
            Line::from(vec![
                prompt,
                Span::raw(value),
                Span::styled("\u{2588}", Style::default().fg(theme.text)),
            ])
        }
    } else if app.search_query.is_empty() {
        Line::from(vec![
            prompt,
            Span::styled("press s to search", Style::default().fg(theme.text_dim)),
        ])
    } else {
        // Read-only echo of the last query — editing it again is `s`.
        Line::from(vec![
            prompt,
            Span::styled(
                app.search_query.clone(),
                Style::default().fg(theme.text_dim),
            ),
        ])
    };
    f.render_widget(Paragraph::new(query_line), rows[0]);

    let border_color = if app.focused == Pane::Search {
        theme.accent
    } else {
        theme.border
    };
    let block = search_block(app, border_color);
    render_results(f, rows[1], app, &theme, block);
}

/// Draw the hits (or the spinner / no-results placeholder) into `area`,
/// inside the caller's block. Shared by the full-area view and the pane so
/// the two never drift apart.
/// Below this *inner* width the six-column preset stops fitting and every
/// column truncates its own values ("595.", headers clipped to "Sour"); the
/// compact preset merges seeds/leechers into one `S/L` column and uses the
/// short size format instead.
const COMPACT_MIN_INNER_WIDTH: u16 = 80;

fn render_results(f: &mut Frame, area: Rect, app: &mut App, theme: &Theme, block: Block) {
    // The pane half of the split bottoms out near 46 inner columns on a
    // 120-col terminal (38 on a 100-col one), so compact is the split's
    // normal shape; full-area results always take the wide preset.
    let inner_width = area.width.saturating_sub(2);
    let compact = inner_width < COMPACT_MIN_INNER_WIDTH;
    if app.search_results.is_empty() {
        let msg = if app.search_in_flight {
            let spinner = SPINNER_FRAMES[app.spinner_tick];
            format!("{} Searching '{}'…", spinner, app.search_query)
        } else {
            format!("No results for '{}'", app.search_query)
        };
        let empty_msg = Paragraph::new(Line::from(vec![Span::styled(
            msg,
            Style::default().fg(theme.text_dim),
        )]))
        .block(block)
        .centered();
        f.render_widget(empty_msg, area);
        return;
    }

    let (labels, widths): (&[&str], &[Constraint]) = if compact {
        (
            &["#", "Name", "Size", "S/L", "Src"][..],
            &[
                Constraint::Length(3), // #
                Constraint::Min(8),    // Name
                Constraint::Length(6), // Size (short format, unit always visible)
                Constraint::Length(9), // S/L ("999/999")
                Constraint::Min(5),    // Src
            ][..],
        )
    } else {
        (
            &HEADER_LABELS[..],
            &[
                Constraint::Length(4),  // #
                Constraint::Min(20),    // Name
                Constraint::Length(10), // Size
                Constraint::Length(7),  // Seeds
                Constraint::Length(7),  // Leechs
                Constraint::Length(12), // Source
            ][..],
        )
    };

    let header_cells = labels.iter().map(|h| {
        Cell::from(*h).style(
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    });
    let header = Row::new(header_cells).height(1);

    // Titles are indexer-supplied, i.e. attacker-controlled: strip control
    // chars like the engine does for torrent names, then truncate by terminal
    // columns so a wide name can't push the numeric columns off screen. The
    // budget is whatever the preset leaves after its fixed columns.
    let name_width = if compact {
        // highlight symbol + inter-column spacing + # + Size + S/L; the Src
        // column shares the leftover space with Name via its own Min.
        let used = 2 + 4 + 3 + 6 + 9;
        inner_width.saturating_sub(used).max(8) as usize
    } else {
        (area.width as usize).saturating_sub(CHROME_WIDTH).max(10)
    };
    let rows: Vec<Row> = app
        .search_results
        .iter()
        .enumerate()
        .map(|(i, result)| {
            let name = truncate(&sanitize_display(&result.title), name_width);
            if compact {
                Row::new(vec![
                    Cell::from(format!(" {}", i + 1)),
                    Cell::from(name),
                    Cell::from(format_size_short(result.size_bytes)),
                    Cell::from(format!("{}/{}", result.seeders, result.leechers)),
                    Cell::from(result.provider.clone()),
                ])
            } else {
                Row::new(vec![
                    Cell::from(format!("  {}", i + 1)),
                    Cell::from(name),
                    Cell::from(format_size(result.size_bytes)),
                    Cell::from(result.seeders.to_string())
                        .style(Style::default().fg(theme.download)),
                    Cell::from(result.leechers.to_string())
                        .style(Style::default().fg(theme.warning)),
                    Cell::from(result.provider.clone()).style(Style::default().fg(theme.text_dim)),
                ])
            }
        })
        .collect();

    let table = Table::new(rows, widths.to_vec())
        .header(header)
        .block(block)
        .row_highlight_style(
            Style::default()
                .bg(theme.highlight)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("\u{25b6} ");

    f.render_stateful_widget(table, area, &mut app.search_state);
}
