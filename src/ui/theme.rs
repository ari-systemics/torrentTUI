//! The central palette. Every renderer pulls colors from `Theme` (reached via
//! `App::theme`) instead of hardcoding `Color` literals, so a `[theme]` section
//! in `config.toml` can restyle the whole UI.
//!
//! Colors are stored as ratatui `Color` and serialized as strings — ratatui's
//! own `serde` feature is off, so the `color_string` module below does the
//! conversion. Accepted spellings: named colors (`"cyan"`, `"lightgreen"`,
//! case-insensitive), hex RGB (`"#ff8700"`), and 256-palette indices (`"236"`).

use ratatui::style::Color;
use serde::{Deserialize, Deserializer, Serialize};

/// The full set of colors the UI draws with. Container-level
/// `#[serde(default)]` fills any field missing when a `Theme` is deserialized
/// directly; the config file's `[theme]` section actually parses as
/// `ThemeOverrides`, which adds the `preset` key and resolves to a `Theme`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Theme {
    /// Primary identity color: titles, active tabs, key highlights.
    #[serde(with = "color_string")]
    pub accent: Color,
    #[serde(with = "color_string")]
    pub border: Color,
    /// Primary foreground for dialog body text and cursors.
    #[serde(with = "color_string")]
    pub text: Color,
    /// Secondary foreground: key hints, labels, dimmed placeholder text.
    #[serde(with = "color_string")]
    pub text_dim: Color,
    #[serde(with = "color_string")]
    pub error: Color,
    #[serde(with = "color_string")]
    pub warning: Color,
    /// Informational status-bar messages and inline notices.
    #[serde(with = "color_string")]
    pub info: Color,
    /// Download speed figures.
    #[serde(with = "color_string")]
    pub download: Color,
    /// Upload speed figures.
    #[serde(with = "color_string")]
    pub upload: Color,
    #[serde(with = "color_string")]
    pub status_downloading: Color,
    #[serde(with = "color_string")]
    pub status_paused: Color,
    /// Cell text for `TorrentStatus::Queued` and the status-bar queue
    /// counter (`Q:<n>`).
    #[serde(with = "color_string")]
    pub status_queued: Color,
    #[serde(with = "color_string")]
    pub status_complete: Color,
    #[serde(with = "color_string")]
    pub status_seeding: Color,
    #[serde(with = "color_string")]
    pub status_fetching: Color,
    #[serde(with = "color_string")]
    pub status_error: Color,
    /// Background of marked rows in the torrent table.
    #[serde(with = "color_string")]
    pub marked_bg: Color,
    /// Selection background (table cursor, detail-view cursor) and the
    /// lower-middle progress stop — the "hot" accent of the palette.
    #[serde(with = "color_string")]
    pub highlight: Color,
    /// Progress bar under 25%.
    #[serde(with = "color_string")]
    pub progress_low: Color,
    /// Progress bar 50–75%.
    #[serde(with = "color_string")]
    pub progress_mid: Color,
    /// Progress bar 75–100%.
    #[serde(with = "color_string")]
    pub progress_high: Color,
    /// Progress-bar gradient stops, interpolated per cell by
    /// `render_progress_bar_styled`. An empty list disables the gradient and
    /// falls back to the `progress_low/mid/high` threshold behavior.
    #[serde(with = "color_string_list")]
    pub progress_gradient: Vec<Color>,
    /// Header title gradient stops, interpolated per character. An empty
    /// list falls back to a flat `accent` title.
    #[serde(with = "color_string_list")]
    pub header_gradient: Vec<Color>,
}

impl Default for Theme {
    /// A vivid dark-terminal palette: cyan stays the accent identity, with an
    /// orange hot accent, purple for upload/seeding, and distinct hues per
    /// torrent status.
    fn default() -> Self {
        Self {
            accent: Color::Cyan,
            border: Color::DarkGray,
            text: Color::White,
            text_dim: Color::Gray,
            error: Color::LightRed,
            warning: Color::LightYellow,
            info: Color::Yellow,
            download: Color::Rgb(80, 250, 123),
            upload: Color::Rgb(175, 95, 255),
            status_downloading: Color::Rgb(97, 175, 239),
            status_paused: Color::Yellow,
            status_queued: Color::Rgb(255, 175, 95),
            status_complete: Color::Green,
            status_seeding: Color::Rgb(175, 95, 255),
            status_fetching: Color::Magenta,
            status_error: Color::LightRed,
            marked_bg: Color::Indexed(236),
            highlight: Color::Rgb(255, 135, 0),
            progress_low: Color::LightRed,
            progress_mid: Color::Yellow,
            progress_high: Color::LightGreen,
            progress_gradient: default_progress_gradient(),
            header_gradient: default_header_gradient(),
        }
    }
}

/// Red → orange → yellow → green, so a bar reads hotter the fuller it is.
fn default_progress_gradient() -> Vec<Color> {
    vec![
        Color::Rgb(255, 85, 85),   // #ff5555
        Color::Rgb(255, 184, 108), // #ffb86c
        Color::Rgb(241, 250, 140), // #f1fa8c
        Color::Rgb(80, 250, 123),  // #50fa7b
    ]
}

/// Cyan → purple, matching the accent/upload poles of the palette.
fn default_header_gradient() -> Vec<Color> {
    vec![
        Color::Rgb(0, 215, 255),  // #00d7ff
        Color::Rgb(175, 95, 255), // #af5fff
    ]
}

/// Shorthand for preset constructors: palettes are written as hex strings and
/// parsed through the same `parse_color` the config file uses, so a typo'd
/// preset color fails loudly in tests rather than rendering black.
fn hex(s: &str) -> Color {
    parse_color(s).expect("preset palettes only contain valid colors")
}

impl Theme {
    /// Preset names in the order `T` cycles through them. "default" is the
    /// built-in palette in `Theme::default()`.
    pub const PRESET_NAMES: &'static [&'static str] = &[
        "default",
        "monokai-pro",
        "dracula",
        "gruvbox-dark",
        "catppuccin-mocha",
    ];

    /// Look up a named preset, `None` for anything unknown. Every preset sets
    /// every field explicitly so a theme switch is total — there is no
    /// inheritance between palettes to leave stale colors behind.
    pub fn preset(name: &str) -> Option<Theme> {
        match name {
            "default" => Some(Theme::default()),
            "monokai-pro" => Some(Self::monokai_pro()),
            "dracula" => Some(Self::dracula()),
            "gruvbox-dark" => Some(Self::gruvbox_dark()),
            "catppuccin-mocha" => Some(Self::catppuccin_mocha()),
            _ => None,
        }
    }

    /// Monokai Pro (classic filter): warm dark grays behind the signature
    /// red/orange/yellow/green/cyan/purple accent set.
    fn monokai_pro() -> Theme {
        Theme {
            accent: hex("#78dce8"),
            border: hex("#403e41"),
            text: hex("#fcfcfa"),
            text_dim: hex("#727072"),
            error: hex("#ff6188"),
            warning: hex("#fc9867"),
            info: hex("#ffd866"),
            download: hex("#a9dc76"),
            upload: hex("#ab9df2"),
            status_downloading: hex("#78dce8"),
            status_paused: hex("#ffd866"),
            status_queued: hex("#fc9867"),
            status_complete: hex("#a9dc76"),
            status_seeding: hex("#ab9df2"),
            status_fetching: hex("#ab9df2"),
            status_error: hex("#ff6188"),
            marked_bg: hex("#403e41"),
            highlight: hex("#fc9867"),
            progress_low: hex("#ff6188"),
            progress_mid: hex("#ffd866"),
            progress_high: hex("#a9dc76"),
            progress_gradient: vec![
                hex("#ff6188"),
                hex("#fc9867"),
                hex("#ffd866"),
                hex("#a9dc76"),
            ],
            header_gradient: vec![hex("#78dce8"), hex("#ab9df2")],
        }
    }

    /// Dracula: blue-gray surfaces with the pastel cyan/green/pink/purple
    /// accent set.
    fn dracula() -> Theme {
        Theme {
            accent: hex("#8be9fd"),
            border: hex("#44475a"),
            text: hex("#f8f8f2"),
            text_dim: hex("#6272a4"),
            error: hex("#ff5555"),
            warning: hex("#ffb86c"),
            info: hex("#f1fa8c"),
            download: hex("#50fa7b"),
            upload: hex("#bd93f9"),
            status_downloading: hex("#8be9fd"),
            status_paused: hex("#f1fa8c"),
            status_queued: hex("#ffb86c"),
            status_complete: hex("#50fa7b"),
            status_seeding: hex("#bd93f9"),
            status_fetching: hex("#ff79c6"),
            status_error: hex("#ff5555"),
            marked_bg: hex("#44475a"),
            highlight: hex("#ff79c6"),
            progress_low: hex("#ff5555"),
            progress_mid: hex("#f1fa8c"),
            progress_high: hex("#50fa7b"),
            progress_gradient: vec![
                hex("#ff5555"),
                hex("#ffb86c"),
                hex("#f1fa8c"),
                hex("#50fa7b"),
            ],
            header_gradient: vec![hex("#8be9fd"), hex("#bd93f9")],
        }
    }

    /// Gruvbox dark: muted retro groove — earthy browns behind softened
    /// red/green/yellow/blue/aqua accents.
    fn gruvbox_dark() -> Theme {
        Theme {
            accent: hex("#83a598"),
            border: hex("#3c3836"),
            text: hex("#ebdbb2"),
            text_dim: hex("#928374"),
            error: hex("#fb4934"),
            warning: hex("#fe8019"),
            info: hex("#fabd2f"),
            download: hex("#b8bb26"),
            upload: hex("#d3869b"),
            status_downloading: hex("#83a598"),
            status_paused: hex("#fabd2f"),
            status_queued: hex("#fe8019"),
            status_complete: hex("#b8bb26"),
            status_seeding: hex("#d3869b"),
            status_fetching: hex("#8ec07c"),
            status_error: hex("#fb4934"),
            marked_bg: hex("#3c3836"),
            highlight: hex("#fe8019"),
            progress_low: hex("#fb4934"),
            progress_mid: hex("#fabd2f"),
            progress_high: hex("#b8bb26"),
            progress_gradient: vec![
                hex("#fb4934"),
                hex("#fe8019"),
                hex("#fabd2f"),
                hex("#b8bb26"),
            ],
            header_gradient: vec![hex("#83a598"), hex("#d3869b")],
        }
    }

    /// Catppuccin Mocha: deep blue-night base with the pastel
    /// rosewater/peach/yellow/green/sky/mauve accent set.
    fn catppuccin_mocha() -> Theme {
        Theme {
            accent: hex("#89dceb"),
            border: hex("#313244"),
            text: hex("#cdd6f4"),
            text_dim: hex("#a6adc8"),
            error: hex("#f38ba8"),
            warning: hex("#fab387"),
            info: hex("#f9e2af"),
            download: hex("#a6e3a1"),
            upload: hex("#cba6f7"),
            status_downloading: hex("#89b4fa"),
            status_paused: hex("#f9e2af"),
            status_queued: hex("#fab387"),
            status_complete: hex("#a6e3a1"),
            status_seeding: hex("#cba6f7"),
            status_fetching: hex("#cba6f7"),
            status_error: hex("#f38ba8"),
            marked_bg: hex("#313244"),
            highlight: hex("#fab387"),
            progress_low: hex("#f38ba8"),
            progress_mid: hex("#f9e2af"),
            progress_high: hex("#a6e3a1"),
            progress_gradient: vec![
                hex("#f38ba8"),
                hex("#fab387"),
                hex("#f9e2af"),
                hex("#a6e3a1"),
            ],
            header_gradient: vec![hex("#89dceb"), hex("#cba6f7")],
        }
    }

    /// The progress-bar gradient: red under 25%, orange to 50%, yellow to 75%,
    /// light green to 100%, and the "complete" green at 100%. The 25–50% stop
    /// deliberately reuses `highlight`, the palette's orange, instead of
    /// carrying a fourth progress field.
    pub fn progress_color(&self, percent: f64) -> Color {
        if percent >= 100.0 {
            self.status_complete
        } else if percent >= 75.0 {
            self.progress_high
        } else if percent >= 50.0 {
            self.progress_mid
        } else if percent >= 25.0 {
            self.highlight
        } else {
            self.progress_low
        }
    }
}

/// The xterm default palette for the 16 standard ANSI colors, in `Color`
/// declaration order (black, red, …, bright white). Used to give named and
/// low indexed colors an RGB value so they can take part in gradients.
const ANSI_RGB: [(u8, u8, u8); 16] = [
    (0, 0, 0),       // black
    (205, 0, 0),     // red
    (0, 205, 0),     // green
    (205, 205, 0),   // yellow
    (0, 0, 238),     // blue
    (205, 0, 205),   // magenta
    (0, 205, 205),   // cyan
    (229, 229, 229), // white (light gray)
    (127, 127, 127), // bright black / dark gray
    (255, 0, 0),     // bright red
    (0, 255, 0),     // bright green
    (255, 255, 0),   // bright yellow
    (92, 92, 255),   // bright blue
    (255, 0, 255),   // bright magenta
    (0, 255, 255),   // bright cyan
    (255, 255, 255), // bright white
];

/// Approximate any `Color` as an RGB triple so gradient stops can be
/// interpolated. Named colors map to the xterm defaults in `ANSI_RGB`;
/// `Indexed(n)` follows the xterm 256-color layout (0–15 the same table,
/// 16–231 the 6×6×6 cube, 232–255 the grayscale ramp). `Reset` has no
/// meaningful hue and maps to black.
fn color_to_rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Reset => (0, 0, 0),
        Color::Indexed(n) => indexed_to_rgb(n),
        named => {
            let idx = match named {
                Color::Black => 0,
                Color::Red => 1,
                Color::Green => 2,
                Color::Yellow => 3,
                Color::Blue => 4,
                Color::Magenta => 5,
                Color::Cyan => 6,
                Color::Gray => 7,
                Color::DarkGray => 8,
                Color::LightRed => 9,
                Color::LightGreen => 10,
                Color::LightYellow => 11,
                Color::LightBlue => 12,
                Color::LightMagenta => 13,
                Color::LightCyan => 14,
                Color::White => 15,
                // Rgb/Indexed/Reset handled above; unreachable in practice.
                _ => 0,
            };
            ANSI_RGB[idx]
        }
    }
}

fn indexed_to_rgb(n: u8) -> (u8, u8, u8) {
    match n {
        0..=15 => ANSI_RGB[n as usize],
        16..=231 => {
            let c = n - 16;
            // Cube channels run 0, 95, 135, 175, 215, 255.
            let channel = |v: u8| if v == 0 { 0 } else { 55 + 40 * v };
            (channel(c / 36), channel((c / 6) % 6), channel(c % 6))
        }
        232..=255 => {
            let gray = 8 + 10 * (n - 232);
            (gray, gray, gray)
        }
    }
}

/// Piecewise-linear interpolation across gradient stops: with n stops there
/// are n−1 segments, and `t` (clamped to 0..=1) picks the two bracketing
/// stops and blends them channel-wise in RGB space. The result is always
/// `Color::Rgb` — named and indexed stops are converted via `color_to_rgb`.
///
/// An empty stop list yields `Color::Reset`; callers are expected to check
/// for it beforehand and fall back to non-gradient behavior (as
/// `render_progress_bar_styled` does with the threshold colors).
pub fn lerp_color(stops: &[Color], t: f32) -> Color {
    let Some((&first, rest)) = stops.split_first() else {
        return Color::Reset;
    };
    if rest.is_empty() {
        let (r, g, b) = color_to_rgb(first);
        return Color::Rgb(r, g, b);
    }
    let t = t.clamp(0.0, 1.0);
    let scaled = t * (stops.len() - 1) as f32;
    // t == 1.0 lands on `stops.len() - 1`; clamp into the last segment.
    let idx = (scaled.floor() as usize).min(stops.len() - 2);
    let frac = scaled - idx as f32;
    let (r1, g1, b1) = color_to_rgb(stops[idx]);
    let (r2, g2, b2) = color_to_rgb(stops[idx + 1]);
    let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * frac).round() as u8;
    Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
}

/// Parse a color from its config-file spelling. Returns `None` for anything
/// unrecognized; the serde layer turns that into a parse error (which
/// `Config::load` surfaces as its "invalid config" warning).
pub fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim();
    let named = match s.to_lowercase().as_str() {
        "reset" => Color::Reset,
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" => Color::Gray,
        "darkgray" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        _ => {
            if let Some(hex) = s.strip_prefix('#') {
                if hex.len() != 6 {
                    return None;
                }
                let v = u32::from_str_radix(hex, 16).ok()?;
                return Some(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
            }
            // Bare number = 256-palette index. u8 parse rejects negatives,
            // values over 255 and non-numeric strings in one step.
            return s.parse::<u8>().ok().map(Color::Indexed);
        }
    };
    Some(named)
}

/// The inverse of `parse_color`, for writing the default config back out.
/// Only the variants `parse_color` can produce need handling, but every
/// variant gets a spelling so serialization never fails.
fn color_to_string(color: Color) -> String {
    match color {
        Color::Reset => "reset".to_string(),
        Color::Black => "black".to_string(),
        Color::Red => "red".to_string(),
        Color::Green => "green".to_string(),
        Color::Yellow => "yellow".to_string(),
        Color::Blue => "blue".to_string(),
        Color::Magenta => "magenta".to_string(),
        Color::Cyan => "cyan".to_string(),
        Color::Gray => "gray".to_string(),
        Color::DarkGray => "darkgray".to_string(),
        Color::LightRed => "lightred".to_string(),
        Color::LightGreen => "lightgreen".to_string(),
        Color::LightYellow => "lightyellow".to_string(),
        Color::LightBlue => "lightblue".to_string(),
        Color::LightMagenta => "lightmagenta".to_string(),
        Color::LightCyan => "lightcyan".to_string(),
        Color::White => "white".to_string(),
        Color::Rgb(r, g, b) => format!("#{:02x}{:02x}{:02x}", r, g, b),
        Color::Indexed(n) => n.to_string(),
    }
}

/// Serde adapter used per field via `#[serde(with = "color_string")]`, keeping
/// colors as plain strings in `config.toml`. Combined with the container-level
/// `#[serde(default)]` on `Theme`, a missing key never reaches `deserialize` —
/// it is filled from `Theme::default()` instead.
mod color_string {
    use super::{color_to_string, parse_color};
    use ratatui::style::Color;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(color: &Color, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&color_to_string(*color))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Color, D::Error> {
        let s = String::deserialize(deserializer)?;
        parse_color(&s)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid color value: {s:?}")))
    }
}

/// Same string spelling as `color_string`, but for gradient stop lists
/// (`progress_gradient = ["#ff5555", "#50fa7b"]`). An empty list round-trips
/// fine and means "no gradient" to the renderers.
mod color_string_list {
    use super::{color_to_string, parse_color};
    use ratatui::style::Color;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(colors: &[Color], serializer: S) -> Result<S::Ok, S::Error> {
        colors
            .iter()
            .map(|c| color_to_string(*c))
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Color>, D::Error> {
        let strings = Vec::<String>::deserialize(deserializer)?;
        strings
            .iter()
            .map(|s| {
                parse_color(s)
                    .ok_or_else(|| serde::de::Error::custom(format!("invalid color value: {s:?}")))
            })
            .collect()
    }
}

/// The `[theme]` section as written in `config.toml`: an optional preset name
/// plus any number of per-key overrides. Two-phase resolution — `resolve`
/// picks the preset (or the default palette) as the base and then applies
/// each override on top — is what lets `preset = "dracula"` and individual
/// color keys mix without either one half-applying. `Config::theme` stays a
/// concrete `Theme`; the resolution happens during deserialization via
/// `deserialize_theme`.
#[derive(Debug, Default, Deserialize)]
pub struct ThemeOverrides {
    pub preset: Option<String>,
    #[serde(default, deserialize_with = "option_color")]
    pub accent: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub border: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub text: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub text_dim: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub error: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub warning: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub info: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub download: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub upload: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub status_downloading: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub status_paused: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub status_queued: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub status_complete: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub status_seeding: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub status_fetching: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub status_error: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub marked_bg: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub highlight: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub progress_low: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub progress_mid: Option<Color>,
    #[serde(default, deserialize_with = "option_color")]
    pub progress_high: Option<Color>,
    #[serde(default, deserialize_with = "option_color_list")]
    pub progress_gradient: Option<Vec<Color>>,
    #[serde(default, deserialize_with = "option_color_list")]
    pub header_gradient: Option<Vec<Color>>,
}

impl ThemeOverrides {
    /// The resolved palette: preset as the base, every named override applied
    /// on top. An absent or empty `preset` means "default"; an unknown name
    /// falls back to the default palette with a log warning rather than
    /// failing the whole config parse.
    pub fn resolve(self) -> Theme {
        let name = self
            .preset
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("default");
        let mut theme = Theme::preset(name).unwrap_or_else(|| {
            tracing::warn!("Unknown theme preset {name:?}, falling back to \"default\"");
            Theme::default()
        });
        if let Some(c) = self.accent {
            theme.accent = c;
        }
        if let Some(c) = self.border {
            theme.border = c;
        }
        if let Some(c) = self.text {
            theme.text = c;
        }
        if let Some(c) = self.text_dim {
            theme.text_dim = c;
        }
        if let Some(c) = self.error {
            theme.error = c;
        }
        if let Some(c) = self.warning {
            theme.warning = c;
        }
        if let Some(c) = self.info {
            theme.info = c;
        }
        if let Some(c) = self.download {
            theme.download = c;
        }
        if let Some(c) = self.upload {
            theme.upload = c;
        }
        if let Some(c) = self.status_downloading {
            theme.status_downloading = c;
        }
        if let Some(c) = self.status_paused {
            theme.status_paused = c;
        }
        if let Some(c) = self.status_queued {
            theme.status_queued = c;
        }
        if let Some(c) = self.status_complete {
            theme.status_complete = c;
        }
        if let Some(c) = self.status_seeding {
            theme.status_seeding = c;
        }
        if let Some(c) = self.status_fetching {
            theme.status_fetching = c;
        }
        if let Some(c) = self.status_error {
            theme.status_error = c;
        }
        if let Some(c) = self.marked_bg {
            theme.marked_bg = c;
        }
        if let Some(c) = self.highlight {
            theme.highlight = c;
        }
        if let Some(c) = self.progress_low {
            theme.progress_low = c;
        }
        if let Some(c) = self.progress_mid {
            theme.progress_mid = c;
        }
        if let Some(c) = self.progress_high {
            theme.progress_high = c;
        }
        if let Some(c) = self.progress_gradient {
            theme.progress_gradient = c;
        }
        if let Some(c) = self.header_gradient {
            theme.header_gradient = c;
        }
        theme
    }
}

/// Serde field adapter for `Config::theme`: reads the `[theme]` table as
/// `ThemeOverrides` and resolves preset + overrides into a concrete `Theme`.
/// Writing the config back out still uses `Theme`'s own `Serialize`, so a
/// saved config contains colors, not the preset name.
pub fn deserialize_theme<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Theme, D::Error> {
    Ok(ThemeOverrides::deserialize(deserializer)?.resolve())
}

/// `Option<Color>` variant of `color_string`: a present key must parse (same
/// "invalid color value" error), an absent key never reaches this and stays
/// `None` via the field-level `#[serde(default)]`.
fn option_color<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Color>, D::Error> {
    color_string::deserialize(deserializer).map(Some)
}

/// `Option<Vec<Color>>` variant of `color_string_list` — see `option_color`.
fn option_color_list<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Vec<Color>>, D::Error> {
    color_string_list::deserialize(deserializer).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_named_colors_case_insensitive() {
        assert_eq!(parse_color("cyan"), Some(Color::Cyan));
        assert_eq!(parse_color("LightGreen"), Some(Color::LightGreen));
        assert_eq!(parse_color("DARKGRAY"), Some(Color::DarkGray));
    }

    #[test]
    fn parse_hex_rgb() {
        assert_eq!(parse_color("#ff8700"), Some(Color::Rgb(255, 135, 0)));
        assert_eq!(parse_color("#50FA7B"), Some(Color::Rgb(80, 250, 123)));
    }

    #[test]
    fn parse_indexed() {
        assert_eq!(parse_color("236"), Some(Color::Indexed(236)));
        assert_eq!(parse_color("0"), Some(Color::Indexed(0)));
    }

    #[test]
    fn parse_invalid_returns_none() {
        assert_eq!(parse_color("notacolor"), None);
        assert_eq!(parse_color("#12345"), None); // too short for Rgb
        assert_eq!(parse_color("#1234567"), None); // too long
        assert_eq!(parse_color("#gg0000"), None); // not hex
        assert_eq!(parse_color("999"), None); // out of u8 range
        assert_eq!(parse_color("-1"), None);
        assert_eq!(parse_color(""), None);
    }

    #[test]
    fn color_string_round_trip() {
        for color in [
            Color::Cyan,
            Color::LightGreen,
            Color::Rgb(255, 135, 0),
            Color::Indexed(236),
        ] {
            assert_eq!(parse_color(&color_to_string(color)), Some(color));
        }
    }

    #[test]
    fn default_palette_is_the_expected_one() {
        let theme = Theme::default();
        assert_eq!(theme.accent, Color::Cyan);
        assert_eq!(theme.highlight, Color::Rgb(255, 135, 0));
        assert_eq!(theme.upload, Color::Rgb(175, 95, 255));
        assert_eq!(theme.marked_bg, Color::Indexed(236));
    }

    #[test]
    fn partial_toml_overrides_only_named_fields() {
        let theme: Theme = toml::from_str(
            r##"
accent = "#ff8700"
status_seeding = "lightmagenta"
marked_bg = "235"
"##,
        )
        .unwrap();
        assert_eq!(theme.accent, Color::Rgb(255, 135, 0));
        assert_eq!(theme.status_seeding, Color::LightMagenta);
        assert_eq!(theme.marked_bg, Color::Indexed(235));
        // Everything else falls back to the default palette.
        assert_eq!(theme.border, Theme::default().border);
        assert_eq!(theme.download, Theme::default().download);
    }

    #[test]
    fn invalid_color_in_toml_is_a_parse_error() {
        // Config::load turns this into its "using defaults" warning rather
        // than a crash, so a typo'd color costs the user their config, not
        // the app.
        assert!(toml::from_str::<Theme>(r#"accent = "octarine""#).is_err());
    }

    #[test]
    fn default_theme_serializes_back_to_toml() {
        // Config::save writes the default config on first launch; if Theme
        // failed to serialize as a TOML table of strings, that write would
        // error out.
        let serialized = toml::to_string(&Theme::default()).unwrap();
        assert!(serialized.contains("accent = \"cyan\""));
        assert!(serialized.contains("highlight = \"#ff8700\""));
        assert!(serialized.contains("marked_bg = \"236\""));
        let round_tripped: Theme = toml::from_str(&serialized).unwrap();
        assert_eq!(round_tripped, Theme::default());
    }

    #[test]
    fn progress_gradient_thresholds() {
        let theme = Theme::default();
        assert_eq!(theme.progress_color(0.0), theme.progress_low);
        assert_eq!(theme.progress_color(24.9), theme.progress_low);
        assert_eq!(theme.progress_color(25.0), theme.highlight);
        assert_eq!(theme.progress_color(49.9), theme.highlight);
        assert_eq!(theme.progress_color(50.0), theme.progress_mid);
        assert_eq!(theme.progress_color(74.9), theme.progress_mid);
        assert_eq!(theme.progress_color(75.0), theme.progress_high);
        assert_eq!(theme.progress_color(99.9), theme.progress_high);
        assert_eq!(theme.progress_color(100.0), theme.status_complete);
    }

    #[test]
    fn lerp_endpoints_hit_the_stops() {
        let stops = [Color::Rgb(255, 85, 85), Color::Rgb(80, 250, 123)];
        assert_eq!(lerp_color(&stops, 0.0), Color::Rgb(255, 85, 85));
        assert_eq!(lerp_color(&stops, 1.0), Color::Rgb(80, 250, 123));
    }

    #[test]
    fn lerp_midpoint_of_two_stops() {
        let stops = [Color::Rgb(0, 0, 0), Color::Rgb(100, 200, 254)];
        assert_eq!(lerp_color(&stops, 0.5), Color::Rgb(50, 100, 127));
    }

    #[test]
    fn lerp_walks_multi_stop_segments() {
        // Three stops = two segments; t = 0.25 is the midpoint of the first,
        // t = 0.75 the midpoint of the second, t = 0.5 exactly the middle stop.
        let stops = [
            Color::Rgb(0, 0, 0),
            Color::Rgb(100, 100, 100),
            Color::Rgb(200, 200, 200),
        ];
        assert_eq!(lerp_color(&stops, 0.5), Color::Rgb(100, 100, 100));
        assert_eq!(lerp_color(&stops, 0.25), Color::Rgb(50, 50, 50));
        assert_eq!(lerp_color(&stops, 0.75), Color::Rgb(150, 150, 150));
    }

    #[test]
    fn lerp_single_stop_is_constant() {
        let stops = [Color::Rgb(12, 34, 56)];
        assert_eq!(lerp_color(&stops, 0.0), Color::Rgb(12, 34, 56));
        assert_eq!(lerp_color(&stops, 0.7), Color::Rgb(12, 34, 56));
        assert_eq!(lerp_color(&stops, 1.0), Color::Rgb(12, 34, 56));
    }

    #[test]
    fn lerp_empty_returns_reset_for_caller_fallback() {
        // Callers check `is_empty()` first and use their own fallback; Reset
        // is only the defensive answer.
        assert_eq!(lerp_color(&[], 0.5), Color::Reset);
    }

    #[test]
    fn lerp_clamps_out_of_range_t() {
        let stops = [Color::Rgb(10, 20, 30), Color::Rgb(200, 210, 220)];
        assert_eq!(lerp_color(&stops, -1.0), Color::Rgb(10, 20, 30));
        assert_eq!(lerp_color(&stops, 2.0), Color::Rgb(200, 210, 220));
    }

    #[test]
    fn named_and_indexed_stops_convert_to_rgb() {
        // Named colors use the xterm defaults; indexed colors follow the
        // 256-palette layout (cube + grayscale).
        assert_eq!(color_to_rgb(Color::Cyan), (0, 205, 205));
        assert_eq!(color_to_rgb(Color::LightRed), (255, 0, 0));
        assert_eq!(color_to_rgb(Color::Indexed(196)), (255, 0, 0)); // cube 5,0,0
        assert_eq!(color_to_rgb(Color::Indexed(21)), (0, 0, 255)); // cube 0,0,5
        assert_eq!(color_to_rgb(Color::Indexed(236)), (48, 48, 48)); // gray ramp
        assert_eq!(color_to_rgb(Color::Indexed(15)), (255, 255, 255));
        // And they lerp like any RGB stop. Note ratatui's `White` is the
        // *bright* white (255); ANSI-7 light gray is `Gray`.
        let stops = [Color::Black, Color::White];
        assert_eq!(
            lerp_color(&stops, 0.5),
            Color::Rgb(128, 128, 128) // midpoint of (0,0,0) and (255,255,255), rounded
        );
    }

    #[test]
    fn gradient_lists_parse_from_toml() {
        let theme: Theme = toml::from_str(
            r##"
progress_gradient = ["#ff0000", "lightgreen", "196"]
header_gradient = []
"##,
        )
        .unwrap();
        assert_eq!(
            theme.progress_gradient,
            vec![
                Color::Rgb(255, 0, 0),
                Color::LightGreen,
                Color::Indexed(196)
            ]
        );
        // An explicit empty list stays empty — that is the "use thresholds"
        // signal, distinct from the field being absent.
        assert!(theme.header_gradient.is_empty());
    }

    #[test]
    fn gradient_lists_default_when_absent_and_round_trip() {
        // Missing keys fall back to the built-in ramps via the container
        // default, and serializing the default writes them back out as
        // string lists.
        let theme: Theme = toml::from_str("").unwrap();
        assert_eq!(theme.progress_gradient, default_progress_gradient());
        assert_eq!(theme.header_gradient, default_header_gradient());

        let serialized = toml::to_string(&Theme::default()).unwrap();
        assert!(serialized.contains("progress_gradient = [\"#ff5555\""));
        assert!(serialized.contains("header_gradient = [\"#00d7ff\""));
        let round_tripped: Theme = toml::from_str(&serialized).unwrap();
        assert_eq!(round_tripped, Theme::default());
    }

    #[test]
    fn invalid_color_in_gradient_list_is_a_parse_error() {
        assert!(toml::from_str::<Theme>(r#"progress_gradient = ["red", "octarine"]"#).is_err());
    }

    #[test]
    fn every_preset_name_resolves_and_unknown_does_not() {
        for name in Theme::PRESET_NAMES {
            assert!(Theme::preset(name).is_some(), "preset {name:?} must exist");
        }
        assert!(Theme::preset("nord").is_none());
        assert!(Theme::preset("").is_none());
    }

    #[test]
    fn presets_are_complete_and_distinct_palettes() {
        // Sanity, not exact-color pinning: each preset is a total palette
        // (gradients included) and differs from the built-in default
        // somewhere. "default" itself must be exactly `Theme::default()`.
        assert_eq!(Theme::preset("default"), Some(Theme::default()));
        for name in &Theme::PRESET_NAMES[1..] {
            let theme = Theme::preset(name).unwrap();
            assert_ne!(
                theme,
                Theme::default(),
                "preset {name:?} too close to default"
            );
            assert!(!theme.progress_gradient.is_empty(), "{name:?}");
            assert!(!theme.header_gradient.is_empty(), "{name:?}");
        }
    }

    #[test]
    fn monokai_pro_spot_check() {
        let theme = Theme::preset("monokai-pro").unwrap();
        assert_eq!(theme.accent, Color::Rgb(0x78, 0xdc, 0xe8)); // #78dce8
        assert_eq!(theme.error, Color::Rgb(0xff, 0x61, 0x88)); // #ff6188
        assert_eq!(
            theme.header_gradient,
            vec![Color::Rgb(0x78, 0xdc, 0xe8), Color::Rgb(0xab, 0x9d, 0xf2)]
        );
    }

    #[test]
    fn overrides_resolve_preset_as_base() {
        let overrides: ThemeOverrides = toml::from_str(r#"preset = "dracula""#).unwrap();
        assert_eq!(overrides.resolve(), Theme::preset("dracula").unwrap());
    }

    #[test]
    fn overrides_beat_the_preset() {
        let overrides: ThemeOverrides = toml::from_str(
            r##"
preset = "monokai-pro"
accent = "#ff8700"
progress_gradient = []
"##,
        )
        .unwrap();
        let theme = overrides.resolve();
        let base = Theme::preset("monokai-pro").unwrap();
        assert_eq!(theme.accent, Color::Rgb(255, 135, 0));
        assert!(theme.progress_gradient.is_empty());
        // Untouched keys still come from the preset, not the default palette.
        assert_eq!(theme.border, base.border);
        assert_eq!(theme.upload, base.upload);
    }

    #[test]
    fn unknown_or_empty_preset_falls_back_to_default() {
        let overrides: ThemeOverrides = toml::from_str(r#"preset = "octarine""#).unwrap();
        assert_eq!(overrides.resolve(), Theme::default());
        let overrides: ThemeOverrides = toml::from_str(r#"preset = """#).unwrap();
        assert_eq!(overrides.resolve(), Theme::default());
        // Overrides still apply on top of the fallback base.
        let overrides: ThemeOverrides =
            toml::from_str("preset = \"octarine\"\naccent = \"red\"").unwrap();
        assert_eq!(overrides.resolve().accent, Color::Red);
    }
}
