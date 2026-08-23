use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use crate::ui::theme::{lerp_color, Theme};

const FILLED: char = '█';
const EMPTY: char = '░';

/// Track color for the unfilled tail of a styled bar: a near-background
/// gray (the 256-palette's #303030) so the gradient reads as light on dark
/// without a full-strength `text_dim` competing with it.
const TRACK_COLOR: Color = Color::Indexed(236);

/// Render a block-glyph bar `width` cells wide followed by a right-aligned
/// percentage, one span per cell. Filled cells walk the theme's
/// `progress_gradient` across the *full* bar width — so a half-full bar
/// shows the ramp's early stops rather than a squashed full ramp — while
/// unfilled cells sit in `TRACK_COLOR`. The suffix is always 7 columns, so
/// budget `width + 7` when sizing the containing cell — the table's 24-wide
/// Progress column is a 15-wide bar plus that suffix, not a 24-wide bar.
///
/// When `progress_gradient` is empty the filled cells and suffix use the
/// flat threshold color (`Theme::progress_color`) instead.
///
/// `percent` is clamped to 0..=100 before use: librqbit can briefly report
/// more downloaded than total after a recheck, and an unclamped value would
/// overrun the cell.
pub fn render_progress_bar_styled(percent: f64, width: usize, theme: &Theme) -> Line<'static> {
    let percent = percent.clamp(0.0, 100.0);
    let filled = (((percent / 100.0) * width as f64).round() as usize).min(width);
    let threshold = theme.progress_color(percent);
    // width 1 would divide by zero below; a single cell just takes t = 0.
    let denom = width.saturating_sub(1).max(1) as f32;

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(width + 1);
    for i in 0..width {
        if i < filled {
            let color = if theme.progress_gradient.is_empty() {
                threshold
            } else {
                lerp_color(&theme.progress_gradient, i as f32 / denom)
            };
            spans.push(Span::styled(FILLED.to_string(), Style::default().fg(color)));
        } else {
            spans.push(Span::styled(
                EMPTY.to_string(),
                Style::default().fg(TRACK_COLOR),
            ));
        }
    }
    // The suffix follows the color at the fill position, not the bar's end.
    let suffix_color = if theme.progress_gradient.is_empty() {
        threshold
    } else {
        lerp_color(&theme.progress_gradient, (percent / 100.0) as f32)
    };
    spans.push(Span::styled(
        format!(" {:>5.1}%", percent),
        Style::default().fg(suffix_color),
    ));
    Line::from(spans)
}

/// Braille spinner frames for the "fetching metadata" cell. The length must
/// stay at 10: `App::tick_spinner` advances `spinner_tick` with `% 10` and
/// `table.rs` indexes this slice with it unchecked, so shortening this array
/// panics the render. A test in this module pins the length.
pub const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::default()
    }

    /// Glyph + fg color per span, for compact assertions.
    fn cells(line: &Line) -> Vec<(String, Option<Color>)> {
        line.spans
            .iter()
            .map(|s| (s.content.to_string(), s.style.fg))
            .collect()
    }

    #[test]
    fn styled_bar_zero() {
        let line = render_progress_bar_styled(0.0, 10, &theme());
        let cells = cells(&line);
        assert_eq!(cells.len(), 11); // 10 cells + suffix
        assert!(cells[..10].iter().all(|(g, _)| g == "░"));
        assert!(cells[10].0.contains("0.0%"));
    }

    #[test]
    fn styled_bar_fifty() {
        let line = render_progress_bar_styled(50.0, 10, &theme());
        let cells = cells(&line);
        assert!(cells[..5].iter().all(|(g, _)| g == "█"));
        assert!(cells[5..10].iter().all(|(g, _)| g == "░"));
        assert!(cells[10].0.contains("50.0%"));
    }

    #[test]
    fn styled_bar_walks_the_gradient() {
        // First filled cell takes the first gradient stop; the last cell of a
        // full bar takes the last.
        let theme = theme();
        let first = theme.progress_gradient[0];
        let last = *theme.progress_gradient.last().unwrap();
        let line = render_progress_bar_styled(100.0, 15, &theme);
        let cells = cells(&line);
        assert_eq!(cells[0].1, Some(lerp_color(&theme.progress_gradient, 0.0)));
        assert_eq!(cells[0].1, Some(first));
        assert_eq!(cells[14].1, Some(lerp_color(&theme.progress_gradient, 1.0)));
        assert_eq!(cells[14].1, Some(last));
        // And the ramp actually moves between the endpoints.
        assert_ne!(cells[0].1, cells[7].1);
    }

    #[test]
    fn styled_bar_empty_gradient_falls_back_to_threshold() {
        let mut theme = theme();
        theme.progress_gradient = Vec::new();
        for percent in [0.0, 25.0, 60.0, 90.0, 100.0] {
            let line = render_progress_bar_styled(percent, 10, &theme);
            let cells = cells(&line);
            let expected = theme.progress_color(percent);
            let filled = (((percent / 100.0) * 10.0).round() as usize).min(10);
            for (glyph, fg) in &cells[..filled] {
                assert_eq!(glyph, "█");
                assert_eq!(*fg, Some(expected), "percent {percent}");
            }
            // Suffix follows the threshold color too.
            assert_eq!(cells[10].1, Some(expected), "percent {percent}");
        }
    }

    #[test]
    fn styled_bar_unfilled_cells_use_track_color() {
        let line = render_progress_bar_styled(50.0, 10, &theme());
        let cells = cells(&line);
        assert!(cells[5..10].iter().all(|(_, fg)| *fg == Some(TRACK_COLOR)));
    }

    #[test]
    fn styled_bar_zero_width() {
        // No bar cells, just the suffix — and no divide-by-zero.
        let line = render_progress_bar_styled(50.0, 0, &theme());
        let cells = cells(&line);
        assert_eq!(cells.len(), 1);
        assert!(cells[0].0.contains("50.0%"));
    }

    #[test]
    fn styled_bar_width_one() {
        let line = render_progress_bar_styled(100.0, 1, &theme());
        let cells = cells(&line);
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].0, "█");
    }

    #[test]
    fn styled_bar_clamps_over_100() {
        // Defensive clamp: filled must never exceed width.
        let line = render_progress_bar_styled(150.0, 10, &theme());
        let cells = cells(&line);
        assert!(cells[..10].iter().all(|(g, _)| g == "█"));
        assert!(cells[10].0.contains("100.0%"));
    }

    #[test]
    fn styled_bar_clamps_negative() {
        let line = render_progress_bar_styled(-10.0, 10, &theme());
        let cells = cells(&line);
        assert!(cells[..10].iter().all(|(g, _)| g == "░"));
        assert!(cells[10].0.contains("0.0%"));
    }

    #[test]
    fn spinner_frame_count_matches_app_modulo() {
        // app.rs::tick_spinner cycles spinner_tick with `% 10`, and table.rs
        // indexes SPINNER_FRAMES[spinner_tick]. If this length ever drifts from
        // 10, that index panics — keep them in lockstep.
        assert_eq!(SPINNER_FRAMES.len(), 10);
    }
}
