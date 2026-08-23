# TorrentTUI

A terminal-based BitTorrent client built with Rust, ratatui, and librqbit.

[![CI](https://github.com/thijsvos/torrentTUI/actions/workflows/ci.yml/badge.svg)](https://github.com/thijsvos/torrentTUI/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/thijsvos/torrentTUI?sort=semver)](https://github.com/thijsvos/torrentTUI/releases/latest)
![Platforms](https://img.shields.io/badge/platforms-Linux%20%7C%20macOS%20%7C%20Windows-blue)
![Rust](https://img.shields.io/badge/language-Rust-orange)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](./LICENSE)
[![@thijsvos](https://img.shields.io/badge/@thijsvos-000000?logo=x)](https://x.com/thijsvos)

![demo](assets/demo.gif)

## Contents

- [Features](#features) · [Installation](#installation) · [Usage](#usage) · [Keybindings](#keybindings) · [Configuration](#configuration) · [Privacy](#privacy) · [Docker](#docker) · [Contributing](#contributing) · [License](#license)

## Features

- **Magnet link & .torrent file support** — add torrents via magnet links or local `.torrent` files
- **Online search** — press `s` to search torrent indexers (built-in apibay.org, optional Jackett/Prowlarr via Torznab) and add results without leaving the TUI
- **Download queue** — cap concurrent downloads; extra torrents queue up and start automatically as slots free
- **Customizable theme** — every color is configurable via `[theme]` in the config file
- **Stream while downloading** — press `s` on any media file in the Files tab to open it in your default player; pieces are fetched in playback order
- **Real-time progress** — progress bars, download/upload speeds, ETA, and peer counts
- **Sorting & filtering** — sort by any column, search torrents by name
- **Bandwidth throttling** — per-torrent fair throttling with configurable download/upload limits
- **Selective file download** — choose which files to download from multi-file torrents
- **Detail view** — inspect torrent info, individual file progress, and peer details
- **Session persistence** — torrents survive restarts via librqbit's built-in fastresume
- **Disk space monitoring** — free space indicator with low-space warnings
- **Completion notifications** — status bar message, plus a desktop notification on Linux/Windows or a system sound on macOS
- **Mouse support** — click to select torrents in the list
- **Configurable** — TOML config file for download directory, network settings, and more

## Installation

### From releases

Download the latest binary for your platform from [Releases](https://github.com/thijsvos/torrentTUI/releases).

**Linux:**
```bash
tar xzf torrenttui-linux-x86_64.tar.gz
sudo mv torrenttui-linux-x86_64 /usr/local/bin/torrenttui
```

**macOS:**
```bash
tar xzf torrenttui-macos-universal.tar.gz
sudo mv torrenttui-macos-universal /usr/local/bin/torrenttui
```

**Windows:**
Extract `torrenttui-windows-x86_64.zip` and add the directory to your PATH.

### From source

Requires Rust 1.95 or newer (any recent stable toolchain works).

```bash
git clone https://github.com/thijsvos/torrentTUI.git
cd torrentTUI
cargo build --release
```

The binary will be at `target/release/torrenttui`.

## Usage

```bash
torrenttui [OPTIONS] [TORRENT_SOURCE]
```

| Option | Description |
|--------|-------------|
| `<TORRENT_SOURCE>` | Magnet link or `.torrent` file path to add on startup (positional) |
| `-d`, `--download-dir <PATH>` | Override download directory (otherwise read from config) |
| `-h`, `--help` | Print help |
| `-V`, `--version` | Print version |

Examples:

```bash
# Launch the TUI
torrenttui

# Add a magnet link on startup
torrenttui "magnet:?xt=urn:btih:..."

# Add a .torrent file on startup
torrenttui path/to/file.torrent

# Override download directory
torrenttui -d /path/to/downloads
```

## Keybindings

| Key | Action |
|-----|--------|
| `a` | Add magnet link or .torrent file |
| `s` | Open the search pane (apibay / Torznab indexers) |
| `w` | Cycle focus between downloads and search panes |
| `\|` | Toggle split orientation (side-by-side ↔ stacked) |
| `p` | Pause/unpause selected (or all marked) torrents |
| `P` | Pause/unpause all torrents |
| `d` | Delete selected (or all marked) torrents |
| `Enter` | Open detail view |
| `j` / `k` (or `↓` / `↑`) | Move selection down/up |
| `Tab` | Cycle sort column / detail tab |
| `r` | Reverse sort order |
| `/` | Search/filter torrents |
| `t` | Set speed limits |
| `T` | Cycle color theme (session only; set `preset` in config to keep) |
| `Space` | Mark/unmark current torrent (then advances selection) |
| `v` | Mark all visible torrents |
| `V` | Clear all marks |
| `Esc` | Clear marks (or close current dialog) |
| `?` | Toggle help |
| `q` | Quit |
| `Ctrl+C` | Quit (double press to force) |

### Search results

| Key | Action |
|-----|--------|
| `j` / `k` (or `↓` / `↑`) | Move selection down/up |
| `Enter` | Add selected result as a download (stays in the list) |
| `g` | Cycle the target download group for adds |
| `s` | New search (pre-filled with the current query) |
| `Esc` | Close the search pane |

Deleting prompts for `[K]eep files` or `[D]elete files` — that choice is about the
*downloaded data*. Either way, if a watch folder is configured, the matching
`.torrent`/`.magnet` is removed from it so the torrent does not come back on the next
launch. See [Watch folder](#watch-folder).

### Detail view

| Key | Action |
|-----|--------|
| `Tab` | Cycle tabs (Stats → Info → Files → Peers) |
| `j` / `k` | Navigate files (Files tab) or peers (Peers tab) |
| `Space` | Toggle file selection (Files tab) |
| `S` | Apply current file selection to engine (Files tab) |
| `s` | Stream selected file in default media player (Files tab) |
| `Esc` / `q` | Back to list |

### Streaming while downloading

In the Files tab, files with a known media extension are marked with `▶`. Press
`s` on any file (not just media — anything streamable over HTTP) to open it in
your system's default player. Librqbit prioritizes the pieces in playback
order, so videos generally start within seconds of pressing `s` — well before
the download is complete.

The engine binds a small HTTP API on `127.0.0.1` (auto-assigned port) at
startup; the player connects to that URL. The API is loopback-only by default;
see [Configuration](#configuration) and [Privacy](#privacy) before changing the
bind address.

Pick a specific player by setting:

```toml
[player]
command = "mpv"          # or "vlc", "iina", etc.
args = ["--no-terminal"] # optional extra args inserted before the URL
```

Leave `command` empty to use the OS default opener (`xdg-open` on Linux,
`open` on macOS, `start` on Windows).

## Configuration

Config file is created automatically at:
- **Linux:** `~/.config/torrenttui/config.toml`
- **macOS:** `~/Library/Application Support/torrenttui/config.toml`
- **Windows:** `%APPDATA%\torrenttui\config.toml`

### Default config

```toml
[general]
download_dir = "~/Downloads/torrents"
confirm_on_quit = true
# watch_dir = "~/torrents/watch"  # optional; auto-add .torrent files dropped here
max_concurrent_downloads = 0    # 0 = unlimited; >0 queues extra torrents (FIFO)

[network]
listen_port = 6881
enable_dht = true
enable_upnp = false           # opt in to open an external port via UPnP
max_download_speed_kbps = 0   # 0 = unlimited
max_upload_speed_kbps = 0     # 0 = unlimited
http_api_bind = "127.0.0.1:0" # localhost-only by default (0 = auto-assigned port)

[ui]
refresh_rate_ms = 100
enable_notifications = true
search_split = "right"        # search pane dock: "right" (side-by-side) or "bottom"

[player]
command = ""                  # empty = OS default (xdg-open / open / start)
args = []                     # extra args inserted before the URL

[search]
providers = ["apibay"]        # built-in apibay.org; add Torznab names to enable them
# [[search.torznab]]          # e.g. Jackett/Prowlarr — name must also appear in providers
# name = "jackett-all"
# url = "http://127.0.0.1:9117/api/v2.0/indexers/all/results/torznab"
# api_key = "..."

# [[groups]]                 # named sub-folders of download_dir for grouped adds
# name = "linux-isos"        # press 'g' in search results to pick a target group
# dir = "linux"              # must be relative; absolute paths and '..' are rejected

# [theme]                     # every key optional; named ("cyan"), hex ("#ff8700"), or 0-255
# preset = "monokai-pro"      # default, monokai-pro, dracula, gruvbox-dark, catppuccin-mocha
# accent = "cyan"             # individual colors override the preset
# download = "#50fa7b"
# upload = "#af5fff"
# highlight = "#ff8700"
# progress_gradient = ["#ff5555", "#ffb86c", "#f1fa8c", "#50fa7b"]  # bar ramp, [] = thresholds
# header_gradient = ["#00d7ff", "#af5fff"]                          # title ramp, [] = flat accent
```

Paths (`download_dir`, `watch_dir`, `player.command`) may start with `~/`, which expands to your home directory.

### Download queue

With `max_concurrent_downloads` set above 0, new torrents added while that many
downloads are already active (including magnets still fetching metadata) are paused
and shown as **Queued**. They start automatically, oldest first, as slots free up.
Pressing `p` on a queued torrent starts it immediately. Queue membership is held in
memory only — after a restart, queued torrents simply come back paused.

### Download groups

`[[groups]]` entries bind a name to a sub-folder of `download_dir`. In the search
results view, `g` cycles the target group (shown as `→ <group>` in the title) and
`Enter` then drops the added torrent into that folder. Groups show in their own
table column (sortable, and matched by the `/` filter) and are assigned at add
time only — files are never moved afterwards. Invalid entries (absolute path,
`..`, empty/duplicate name) are skipped with a log warning. Group labels persist
across restarts via `groups.json` in the config directory.

### Online search

Press `s` in the main view to search. On terminals with enough room this opens
a search pane next to (or below, with `search_split = "bottom"`) the downloads
table — `w` moves focus between the panes, `|` flips the orientation for the
session, and `Esc` from the focused search pane closes it. On small terminals
search falls back to taking over the full main area. The built-in `apibay`
provider needs no
configuration; results arrive as magnets sorted by seeders. To search your own
indexers, add Torznab entries (Jackett/Prowlarr) under `[search]` as shown above
and list their names in `providers`. Indexers that fail report their error in the
status bar without blocking results from the others.

### Theme

All colors come from the `[theme]` section; omit it to use the built-in palette.
`preset` picks a full named palette — `default`, `monokai-pro`, `dracula`,
`gruvbox-dark`, or `catppuccin-mocha` — and any color keys you set are applied
on top of it. Pressing `T` in the main view cycles the same palettes live for
the current session. Individual keys include `accent`, `border`, `text`, `text_dim`, `error`, `warning`, `info`,
`download`, `upload`, `status_*` (downloading, paused, queued, complete, seeding,
fetching, error), `marked_bg`, `highlight`, and `progress_low/mid/high`. Values are
named colors, `#rrggbb` hex, or a 0–255 palette index. `progress_gradient` and
`header_gradient` take a *list* of color values and are interpolated across the
progress bar / header title; set `progress_gradient = []` to fall back to the
flat `progress_low/mid/high` threshold colors.

### Watch folder

Set `watch_dir` and any `.torrent` file dropped there is added automatically,
both while TorrentTUI is running and on the next startup. `.magnet` files are
picked up too, but only while the app is running — librqbit's startup rescan
reads `.torrent` only, so a `.magnet` dropped while it is closed is ignored.

**Deleting a torrent also deletes its source file from the watch folder.** This applies
to both `[K]eep files` and `[D]elete files` — the choice there is about the downloaded
data, not the metadata. Without this the folder is rescanned at every launch and the
torrent you deleted comes straight back.

Worth knowing if the folder is shared with something else (a Syncthing folder, an \*arr
blackhole directory): TorrentTUI removes only files whose info hash matches a torrent you
deleted, but it does remove them for good. Cleanup is skipped entirely when `watch_dir` is
your home directory or a filesystem root, and never descends into `download_dir` when that
lives inside the watch folder.

### Logging

By default only `torrenttui=warn` is logged to `~/.config/torrenttui/torrenttui.log`. Set `RUST_LOG` to bump verbosity (e.g. `RUST_LOG=torrenttui=debug,librqbit=info`).

## Privacy

A few defaults worth knowing:

- **Logging is filtered.** Only TorrentTUI's own warnings are written to disk; librqbit's INFO-level output (peer IPs, tracker URLs, info hashes) is silenced. Bumping `RUST_LOG` re-enables it — redact before sharing logs.
- **UPnP is off by default.** Enabling it (`network.enable_upnp = true`) opens an external port via your router and exposes you to peers outside your LAN.
- **No telemetry.** TorrentTUI makes no outbound connections except to BitTorrent peers, trackers, and (if DHT is enabled) the DHT network.
- **Notifications.** Control characters are stripped from torrent names at the engine boundary, and names are additionally Pango-escaped before reaching the Linux notification daemon. macOS plays a sound instead of sending a notification, so no name leaves the process there. Disable entirely with `ui.enable_notifications = false`.
- **HTTP streaming API is loopback-only and authenticated.** The embedded API used for the `s` stream keybinding binds to `127.0.0.1:0`, is mounted read-only, and requires HTTP basic auth with a random password generated per run. The credentials ride in the stream URL handed to your media player, which means they are visible in that process's argv while it runs. Changing `network.http_api_bind` to a routable interface sends those credentials over plaintext HTTP — do this only on a trusted LAN.

## Docker

### Build

```bash
cd torrentTUI
docker build -t torrenttui .
```

### Run

```bash
docker run -it \
  -v ~/Downloads/torrents:/downloads \
  -v ~/.config/torrenttui:/home/torrenttui/.config/torrenttui \
  -p 6881-6890:6881-6890 \
  torrenttui
```

Add a magnet link on startup:

```bash
docker run -it \
  -v ~/Downloads/torrents:/downloads \
  -p 6881:6881 \
  torrenttui -d /downloads "magnet:?xt=urn:btih:..."
```

The `-it` flags are required since TorrentTUI is an interactive terminal application. The config volume is optional but enables session persistence across container restarts.

## Built with

- [librqbit](https://github.com/ikatson/librqbit) — BitTorrent engine (also handles fastresume / session persistence)
- [ratatui](https://github.com/ratatui/ratatui) — Terminal UI framework
- [crossterm](https://github.com/crossterm-rs/crossterm) — Terminal manipulation
- [tokio](https://github.com/tokio-rs/tokio) — Async runtime

## Contributing

PRs are welcome. See [CONTRIBUTING.md](./CONTRIBUTING.md) for the dev setup, lint commands, and release process. Found a bug? [Open an issue](https://github.com/thijsvos/torrentTUI/issues/new/choose). Found a security problem? See [SECURITY.md](./SECURITY.md) — please don't open a public issue.

## License

[MIT](./LICENSE) © Thijs Vos
