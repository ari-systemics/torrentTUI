use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::search::Provider;
use crate::ui::theme::{deserialize_theme, Theme};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub network: NetworkConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub player: PlayerConfig,
    /// The active palette. In `config.toml` this is a `[theme]` table with an
    /// optional `preset` name plus per-color overrides; deserialization
    /// resolves both into a concrete `Theme` (see `ThemeOverrides`).
    #[serde(default, deserialize_with = "deserialize_theme")]
    pub theme: Theme,
    #[serde(default)]
    pub search: SearchConfig,
    /// Named download groups (`[[groups]]` tables), validated at load time by
    /// `validate_groups`. Each one binds a name to a sub-folder of
    /// `general.download_dir` that grouped adds are routed into.
    #[serde(default)]
    pub groups: Vec<GroupConfig>,
}

/// One download group as configured via a `[[groups]]` table. `dir` is a
/// *relative* sub-folder under `general.download_dir` — absolute paths and
/// `..` components are rejected at load, so the value can be handed straight
/// to librqbit's `AddTorrentOptions::sub_folder` without it escaping the
/// download directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupConfig {
    pub name: String,
    pub dir: String,
}

/// Drop invalid `[[groups]]` entries, warning about each one. An entry is
/// kept only when it has a non-empty name and a non-empty, relative,
/// `..`-free dir, and its name has not been seen before — a group dir that
/// could point outside `download_dir` is a path-injection hole, so these are
/// load-time rejections rather than add-time surprises.
fn validate_groups(groups: Vec<GroupConfig>) -> Vec<GroupConfig> {
    let mut names = std::collections::HashSet::new();
    let mut kept = Vec::new();
    for g in groups {
        let path = std::path::Path::new(&g.dir);
        let reason = if g.name.is_empty() || g.dir.is_empty() {
            Some("name and dir must both be non-empty")
        } else if path.is_absolute() {
            Some("dir must be a relative sub-folder of download_dir")
        } else if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            Some("dir must not contain '..'")
        } else if !names.insert(g.name.clone()) {
            Some("duplicate group name")
        } else {
            None
        };
        match reason {
            Some(r) => tracing::warn!("Ignoring [[groups]] entry {:?}: {}", g.name, r),
            None => kept.push(g),
        }
    }
    kept
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_download_dir")]
    pub download_dir: String,
    #[serde(default = "default_true")]
    pub confirm_on_quit: bool,
    /// Directory watched for `.torrent`/`.magnet` files to add automatically.
    /// Disabled if it resolves to the same directory as `download_dir`, which
    /// would loop. Deleting a torrent also deletes the file that added it from
    /// here — unless the directory is the user's home or a filesystem root, in
    /// which case adding still works but that cleanup is switched off.
    #[serde(default)]
    pub watch_dir: Option<String>,
    /// Maximum number of torrents downloading at once; `0` (the default)
    /// means unlimited, i.e. every add starts immediately as before. Past the
    /// cap, new torrents are paused on arrival and started FIFO as slots free
    /// up, showing as "Queued" in the table. Queue membership is in-memory
    /// only — librqbit persists the paused state but not *why* — so a restart
    /// re-derives the queue from whatever comes back live over the cap.
    #[serde(default)]
    pub max_concurrent_downloads: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// First port of the range the engine binds; it reserves ten consecutive
    /// ports from here. The Dockerfile's EXPOSE range is derived from the same
    /// span, so widening it means editing both.
    #[serde(default = "default_listen_port")]
    pub listen_port: u16,
    #[serde(default = "default_true")]
    pub enable_dht: bool,
    /// UPnP is opt-in. Enabling this opens an external port via your router's
    /// IGD/UPnP service, which exposes you to peers outside your LAN.
    #[serde(default)]
    pub enable_upnp: bool,
    /// Download cap in KB/s, where `0` means unlimited. Enforced by pausing
    /// and unpausing torrents on a duty cycle rather than by rate limiting,
    /// since librqbit offers no limiter — expect capped torrents to show as
    /// "Throttled" in the table.
    #[serde(default)]
    pub max_download_speed_kbps: u64,
    /// Upload cap in KB/s, `0` for unlimited. Same duty-cycle mechanism as the
    /// download cap, but with one global budget instead of a bucket per
    /// torrent.
    #[serde(default)]
    pub max_upload_speed_kbps: u64,
    /// Bind address for the embedded HTTP API that serves file-stream URLs to
    /// external media players. Default `127.0.0.1:0` (auto-assigned port,
    /// loopback only). The API is mounted read-only (no add/pause/delete
    /// routes) *and* behind HTTP basic auth with a random per-session password
    /// — read-only alone is not enough, because librqbit registers two POST
    /// routes outside that gate. The credentials ride in the stream URL handed
    /// to the media player.
    ///
    /// Binding to a non-loopback host still deserves care: basic auth over
    /// plaintext HTTP is readable by anyone on the path, and the URL (with its
    /// credentials) is visible in the player's argv. The app logs a warning and
    /// shows a status message when it binds off-loopback.
    #[serde(default = "default_http_api_bind")]
    pub http_api_bind: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PlayerConfig {
    /// External program used to open stream URLs from the Files tab. Empty
    /// means "use the OS default opener" (`xdg-open` / `open` / `start`).
    /// Examples: `mpv`, `vlc`, `iina`.
    #[serde(default)]
    pub command: String,
    /// Extra arguments inserted before the URL when invoking `command`.
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchConfig {
    /// Provider names to query, in the form `providers = ["apibay",
    /// "jackett-all"]`. "apibay" needs no further configuration; any other
    /// name must match a `[[search.torznab]]` entry's `name`.
    #[serde(default = "default_search_providers")]
    pub providers: Vec<String>,
    #[serde(default)]
    pub torznab: Vec<TorznabConfig>,
}

/// One Torznab indexer (Jackett/Prowlarr) as configured via
/// `[[search.torznab]]` tables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TorznabConfig {
    pub name: String,
    pub url: String,
    pub api_key: String,
}

impl SearchConfig {
    /// Map the configured provider names to runnable providers. Unknown names
    /// are ignored with a warning rather than failing the whole search setup.
    pub fn build_providers(&self) -> Vec<Provider> {
        let mut providers = Vec::new();
        for name in &self.providers {
            match name.as_str() {
                "apibay" => providers.push(Provider::Apibay),
                other => match self.torznab.iter().find(|t| t.name == other) {
                    Some(cfg) => providers.push(Provider::Torznab(cfg.clone())),
                    None => tracing::warn!("Ignoring unknown search provider: {other}"),
                },
            }
        }
        providers
    }
}

fn default_search_providers() -> Vec<String> {
    vec!["apibay".to_string()]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Idle repaint interval. Clamped to 16-1000 ms at startup, so values
    /// outside that are accepted by the parser and then quietly ignored.
    /// Rendering is change-driven, so this only paces animations and the ageing
    /// out of status messages — it is not a frame rate floor.
    #[serde(default = "default_refresh_rate")]
    pub refresh_rate_ms: u64,
    #[serde(default = "default_true")]
    pub enable_notifications: bool,
    /// Where the search pane docks when opened: `"right"` (side-by-side, the
    /// default) or `"bottom"` (stacked under the downloads table). main.rs
    /// maps this onto `App::split_vertical` as `search_split == "right"`, so
    /// an unrecognized value behaves like `"bottom"` rather than failing to
    /// parse. `|` flips the orientation for the session without touching this.
    #[serde(default = "default_search_split")]
    pub search_split: String,
}

fn default_download_dir() -> String {
    dirs::download_dir()
        .unwrap_or_else(|| PathBuf::from("./downloads"))
        .join("torrents")
        .to_string_lossy()
        .to_string()
}

fn default_true() -> bool {
    true
}

fn default_listen_port() -> u16 {
    6881
}

fn default_refresh_rate() -> u64 {
    100
}

fn default_search_split() -> String {
    "right".to_string()
}

fn default_http_api_bind() -> String {
    "127.0.0.1:0".to_string()
}

/// Expand a leading `~` in a user-supplied path against the home directory.
/// TOML strings are always quoted and the TUI input dialog never involves a
/// shell, so nothing else ever performs this expansion — without it a
/// `"~/downloads"` value is treated as a relative path and materializes as a
/// literal `./~/downloads` directory under the process working directory
/// (issue #33). `~user` forms and mid-string tildes are valid path characters
/// and stay untouched, as does everything when no home directory is known.
pub fn expand_tilde(path: &str) -> String {
    let Some(home) = dirs::home_dir() else {
        return path.to_string();
    };
    if path == "~" {
        return home.to_string_lossy().into_owned();
    }
    let rest = path.strip_prefix("~/");
    #[cfg(windows)]
    let rest = rest.or_else(|| path.strip_prefix("~\\"));
    match rest {
        Some(rest) => home.join(rest).to_string_lossy().into_owned(),
        None => path.to_string(),
    }
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
            confirm_on_quit: true,
            watch_dir: None,
            max_concurrent_downloads: 0,
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_port: default_listen_port(),
            enable_dht: true,
            enable_upnp: false,
            max_download_speed_kbps: 0,
            max_upload_speed_kbps: 0,
            http_api_bind: default_http_api_bind(),
        }
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            refresh_rate_ms: default_refresh_rate(),
            enable_notifications: true,
            search_split: default_search_split(),
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            providers: default_search_providers(),
            torznab: Vec::new(),
        }
    }
}

impl Config {
    /// The app's per-user directory. Holds `config.toml` and the rotating
    /// `torrenttui.log`, so main.rs calls this before any config is loaded.
    /// Falls back to the process working directory when the platform reports
    /// no config dir, which keeps the app runnable in a container with no HOME
    /// rather than failing at startup.
    pub fn config_dir() -> PathBuf {
        dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("torrenttui")
    }

    pub fn config_path() -> PathBuf {
        Self::config_dir().join("config.toml")
    }

    /// Load config from disk. Returns `(config, optional_warning)`. The warning
    /// is set when the config file existed but couldn't be parsed; callers
    /// should surface it to the user (the file is still treated as defaults).
    ///
    /// Not a pure read: when no config file exists this writes one containing
    /// the defaults, so a first launch materializes `config.toml` and its
    /// parent directory — and `Err` covers that write as well as the read. A
    /// file that exists but is unparseable is deliberately left alone for the
    /// user to fix.
    /// Tilde expansion runs only on the parsed-file path; the default and
    /// parse-failure paths return values that never contain `~`.
    pub fn load() -> Result<(Self, Option<String>)> {
        let path = Self::config_path();
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            match toml::from_str::<Config>(&content) {
                Ok(mut config) => {
                    config.expand_paths();
                    config.groups = validate_groups(std::mem::take(&mut config.groups));
                    Ok((config, None))
                }
                Err(e) => {
                    let msg = format!("Invalid config file, using defaults: {}", e);
                    tracing::warn!("{msg}");
                    Ok((Config::default(), Some(msg)))
                }
            }
        } else {
            let config = Config::default();
            config.save()?;
            Ok((config, None))
        }
    }

    /// Expand `~` in every user-supplied path the config carries. Called once
    /// at load time — the single choke point before any of these strings reach
    /// `PathBuf::from` (session root, watch folder, disk-space probe) or
    /// `Command::new` (player spawn). Never runs against an existing config
    /// file's on-disk contents, so users' `~` spellings are preserved there.
    pub fn expand_paths(&mut self) {
        self.general.download_dir = expand_tilde(&self.general.download_dir);
        self.general.watch_dir = self.general.watch_dir.take().map(|d| expand_tilde(&d));
        self.player.command = expand_tilde(&self.player.command);
    }

    /// Write the config out, creating the directory if needed. Serializes the
    /// in-memory values, so any `~` the user wrote has already been expanded by
    /// `load` and gets persisted in expanded form.
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        // Atomic write: tmp file + fsync + rename. A power loss or SIGKILL
        // mid-write would otherwise leave the user with a zero-byte config
        // and silently restored defaults on next launch.
        let tmp = path.with_extension("toml.tmp");
        {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(content.as_bytes())?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_defaults() {
        let config = Config::default();
        assert!(config.general.confirm_on_quit);
        assert!(config.general.watch_dir.is_none());
        assert_eq!(config.general.max_concurrent_downloads, 0);
        assert_eq!(config.network.listen_port, 6881);
        assert!(config.network.enable_dht);
        assert!(!config.network.enable_upnp);
        assert_eq!(config.network.max_download_speed_kbps, 0);
        assert_eq!(config.network.max_upload_speed_kbps, 0);
        // Localhost-only by default — never bind the HTTP API to a routable
        // interface without an explicit user decision.
        assert_eq!(config.network.http_api_bind, "127.0.0.1:0");
        assert_eq!(config.ui.refresh_rate_ms, 100);
        assert!(config.ui.enable_notifications);
        assert!(config.player.command.is_empty());
        assert!(config.player.args.is_empty());
    }

    #[test]
    fn test_search_split_default_is_right() {
        // No [ui] section at all, and a [ui] section without the key, both
        // land on the side-by-side split.
        let config: Config = toml::from_str("[general]\n").unwrap();
        assert_eq!(config.ui.search_split, "right");
        let config: Config = toml::from_str("[ui]\nrefresh_rate_ms = 50\n").unwrap();
        assert_eq!(config.ui.search_split, "right");
    }

    #[test]
    fn test_search_split_parses_bottom() {
        let config: Config = toml::from_str("[ui]\nsearch_split = \"bottom\"\n").unwrap();
        assert_eq!(config.ui.search_split, "bottom");
    }

    #[test]
    fn test_partial_toml() {
        let toml_str = r#"
[general]
confirm_on_quit = false
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(!config.general.confirm_on_quit);
        assert_eq!(config.network.listen_port, 6881);
        assert_eq!(config.ui.refresh_rate_ms, 100);
        assert!(config.ui.enable_notifications);
    }

    #[test]
    fn test_full_toml() {
        let toml_str = r#"
[general]
download_dir = "/tmp/downloads"
confirm_on_quit = false
watch_dir = "/var/torrents/watch"
max_concurrent_downloads = 3

[network]
listen_port = 7000
enable_dht = false
enable_upnp = true
max_download_speed_kbps = 500
max_upload_speed_kbps = 100
http_api_bind = "127.0.0.1:8731"

[ui]
refresh_rate_ms = 200
enable_notifications = false

[player]
command = "mpv"
args = ["--no-terminal"]
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.general.download_dir, "/tmp/downloads");
        assert!(!config.general.confirm_on_quit);
        assert_eq!(
            config.general.watch_dir.as_deref(),
            Some("/var/torrents/watch")
        );
        assert_eq!(config.general.max_concurrent_downloads, 3);
        assert_eq!(config.network.listen_port, 7000);
        assert!(!config.network.enable_dht);
        assert!(config.network.enable_upnp);
        assert_eq!(config.network.max_download_speed_kbps, 500);
        assert_eq!(config.network.max_upload_speed_kbps, 100);
        assert_eq!(config.network.http_api_bind, "127.0.0.1:8731");
        assert_eq!(config.ui.refresh_rate_ms, 200);
        assert!(!config.ui.enable_notifications);
        assert_eq!(config.player.command, "mpv");
        assert_eq!(config.player.args, vec!["--no-terminal".to_string()]);
    }

    #[test]
    fn test_expand_tilde() {
        let home = dirs::home_dir().expect("home dir required for this test");
        assert_eq!(expand_tilde("~"), home.to_string_lossy());
        assert_eq!(
            expand_tilde("~/rtorrent/download"),
            home.join("rtorrent/download").to_string_lossy()
        );
        // Only a leading `~/` is special — `~user` and mid-string tildes are
        // ordinary path characters.
        assert_eq!(expand_tilde("~user/download"), "~user/download");
        assert_eq!(expand_tilde("/tmp/~backup"), "/tmp/~backup");
        assert_eq!(expand_tilde("/absolute/path"), "/absolute/path");
        assert_eq!(expand_tilde("relative/path"), "relative/path");
        assert_eq!(expand_tilde(""), "");
    }

    #[cfg(windows)]
    #[test]
    fn test_expand_tilde_backslash() {
        let home = dirs::home_dir().expect("home dir required for this test");
        assert_eq!(expand_tilde(r"~\dl"), home.join("dl").to_string_lossy());
    }

    #[test]
    fn test_expand_paths_tilde_config() {
        let toml_str = r#"
[general]
download_dir = "~/rtorrent/download"
watch_dir = "~/rtorrent/watch"

[player]
command = "~/bin/mpv"
"#;
        let mut config: Config = toml::from_str(toml_str).unwrap();
        config.expand_paths();
        let home = dirs::home_dir().expect("home dir required for this test");
        assert_eq!(
            config.general.download_dir,
            home.join("rtorrent/download").to_string_lossy()
        );
        assert_eq!(
            config.general.watch_dir.as_deref().unwrap(),
            home.join("rtorrent/watch").to_string_lossy()
        );
        assert_eq!(
            config.player.command,
            home.join("bin/mpv").to_string_lossy()
        );
    }

    #[test]
    fn test_expand_paths_leaves_plain_config_untouched() {
        let mut config = Config::default();
        let download_dir = config.general.download_dir.clone();
        config.expand_paths();
        assert_eq!(config.general.download_dir, download_dir);
        assert!(config.general.watch_dir.is_none());
        assert!(config.player.command.is_empty());
    }

    #[test]
    fn test_theme_section_parses() {
        // A [theme] section overrides only the named colors; everything else
        // falls back to the default palette. Covers all three accepted
        // spellings: hex RGB, named color, palette index.
        let toml_str = r##"
[theme]
accent = "#ff8700"
status_paused = "lightyellow"
marked_bg = "235"
"##;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.theme.accent, ratatui::style::Color::Rgb(255, 135, 0));
        assert_eq!(
            config.theme.status_paused,
            ratatui::style::Color::LightYellow
        );
        assert_eq!(config.theme.marked_bg, ratatui::style::Color::Indexed(235));
        assert_eq!(config.theme.border, Theme::default().border);
        assert_eq!(config.network.listen_port, 6881);
    }

    #[test]
    fn test_theme_gradient_lists_parse() {
        // Gradient stops use the same color spellings as the scalar fields;
        // an absent list keeps the built-in ramp, an explicit empty list
        // disables the gradient (threshold fallback).
        let toml_str = r##"
[theme]
progress_gradient = ["#ff0000", "#00ff00"]
"##;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.theme.progress_gradient,
            vec![
                ratatui::style::Color::Rgb(255, 0, 0),
                ratatui::style::Color::Rgb(0, 255, 0)
            ]
        );
        assert_eq!(
            config.theme.header_gradient,
            Theme::default().header_gradient
        );

        let disabled: Config = toml::from_str("[theme]\nprogress_gradient = []\n").unwrap();
        assert!(disabled.theme.progress_gradient.is_empty());
    }

    #[test]
    fn test_theme_preset_parses() {
        // [theme] preset selects a full palette; no per-color keys needed.
        let config: Config = toml::from_str("[theme]\npreset = \"dracula\"\n").unwrap();
        assert_eq!(config.theme, Theme::preset("dracula").unwrap());
        assert_eq!(
            config.theme.accent,
            ratatui::style::Color::Rgb(0x8b, 0xe9, 0xfd) // #8be9fd
        );
    }

    #[test]
    fn test_theme_preset_plus_override_override_wins() {
        let toml_str = r##"
[theme]
preset = "monokai-pro"
accent = "#ff8700"
"##;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.theme.accent, ratatui::style::Color::Rgb(255, 135, 0));
        // Everything else follows the preset.
        assert_eq!(
            config.theme.text,
            Theme::preset("monokai-pro").unwrap().text
        );
    }

    #[test]
    fn test_theme_unknown_preset_falls_back_to_default() {
        let config: Config = toml::from_str("[theme]\npreset = \"octarine\"\n").unwrap();
        assert_eq!(config.theme, Theme::default());
    }

    #[test]
    fn test_player_section_omitted_defaults_apply() {
        // A config file written before the [player] section existed should
        // still parse cleanly and produce the empty defaults.
        let toml_str = r#"
[general]
download_dir = "/tmp/downloads"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.player.command, "");
        assert!(config.player.args.is_empty());
        assert_eq!(config.network.http_api_bind, "127.0.0.1:0");
    }

    #[test]
    fn test_search_section_omitted_defaults_apply() {
        // No [search] section at all: apibay only, no torznab entries.
        let config: Config = toml::from_str("[general]\n").unwrap();
        assert_eq!(config.search.providers, vec!["apibay".to_string()]);
        assert!(config.search.torznab.is_empty());
    }

    #[test]
    fn test_search_section_full_form() {
        let toml_str = r#"
[search]
providers = ["apibay", "jackett-all"]

[[search.torznab]]
name = "jackett-all"
url = "http://127.0.0.1:9117/api/v2.0/indexers/all/results/torznab"
api_key = "secret123"

[[search.torznab]]
name = "prowlarr-nobits"
url = "http://127.0.0.1:9696/api/v1/indexer/2/torznab"
api_key = "otherkey"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.search.providers,
            vec!["apibay".to_string(), "jackett-all".to_string()]
        );
        assert_eq!(config.search.torznab.len(), 2);
        assert_eq!(config.search.torznab[0].name, "jackett-all");
        assert_eq!(config.search.torznab[0].api_key, "secret123");
        assert_eq!(config.search.torznab[1].name, "prowlarr-nobits");
    }

    #[test]
    fn test_build_providers_mapping() {
        let toml_str = r#"
[search]
providers = ["jackett-all", "apibay", "nonexistent"]

[[search.torznab]]
name = "jackett-all"
url = "http://127.0.0.1:9117/api/v2.0/indexers/all/results/torznab"
api_key = "secret123"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        let providers = config.search.build_providers();
        // "nonexistent" matches no torznab entry and is dropped.
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].name(), "jackett-all");
        assert_eq!(providers[1].name(), "TPB");
    }

    #[test]
    fn test_build_providers_empty_when_providers_empty() {
        let toml_str = r#"
[search]
providers = []
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert!(config.search.build_providers().is_empty());
    }

    #[test]
    fn test_groups_parse() {
        let toml_str = r#"
[[groups]]
name = "linux-isos"
dir = "linux"

[[groups]]
name = "movies"
dir = "video/movies"
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.groups,
            vec![
                GroupConfig {
                    name: "linux-isos".to_string(),
                    dir: "linux".to_string(),
                },
                GroupConfig {
                    name: "movies".to_string(),
                    dir: "video/movies".to_string(),
                },
            ]
        );
    }

    #[test]
    fn test_groups_omitted_defaults_to_empty() {
        let config: Config = toml::from_str("[general]\n").unwrap();
        assert!(config.groups.is_empty());
    }

    #[test]
    fn test_validate_groups_drops_invalid_entries() {
        let groups = vec![
            GroupConfig {
                name: "ok".to_string(),
                dir: "sub/dir".to_string(),
            },
            GroupConfig {
                name: "abs".to_string(),
                dir: "/tmp/escape".to_string(),
            },
            GroupConfig {
                name: "dotdot".to_string(),
                dir: "../escape".to_string(),
            },
            GroupConfig {
                name: "".to_string(),
                dir: "noname".to_string(),
            },
            GroupConfig {
                name: "nodir".to_string(),
                dir: "".to_string(),
            },
            GroupConfig {
                name: "ok".to_string(),
                dir: "other".to_string(),
            },
        ];
        let kept = validate_groups(groups);
        assert_eq!(
            kept,
            vec![GroupConfig {
                name: "ok".to_string(),
                dir: "sub/dir".to_string(),
            }]
        );
    }
}
