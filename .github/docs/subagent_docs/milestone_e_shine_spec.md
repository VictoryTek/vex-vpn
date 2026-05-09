# Milestone E — "Make It Shine" — Implementation Specification

**Project:** vex-vpn  
**Phase:** 1 — Research & Specification  
**Date:** 2026-05-09  
**Status:** READY FOR IMPLEMENTATION

---

## 1. Executive Summary & Scope Decision

### What Ships in Milestone E

| Item | Rationale |
|------|-----------|
| **F9** — Connection history pane | No new deps; `serde_json` already present; high UX value |
| **F14** — HiDPI / bundled SVG icons | Medium effort, high value on non-GNOME desktops |
| **B8** — Tray broadcast (replace 3 s poll lag) | Replaces read-on-demand with push; medium effort |
| **B15** — CI/CD: `cargo fmt --check` + GitHub Actions + GitLab CI | High value for project hygiene |
| **Config atomic write + schema version** | Small, targeted, prevents corruption |
| **NixOS DNS `lib.mkDefault`** | One-line Nix fix in `module-gui.nix` |
| **`wg` wrapper path hardening** | Security correctness on NixOS; two-line fix |

### What is Deferred to Milestone F

| Item | Rationale |
|------|-----------|
| **F10** — Localization (`gettext-rs`) | Zero translations ready; scaffolding is low-ROI until app stabilises |
| **F13** — Map view (`libshumate-rs`) | Early-stage bindings; network tile fetching; "nice-to-have" |
| **`oo7` Secret Service** | Still requires `zbus 4.x`; blocked by zbus upgrade path |

### Critical Finding: `thiserror` Must Stay

**The prompt assumes `thiserror` is unused — this is incorrect.**  
`thiserror` is actively used in `src/pia.rs` at line 124:
```rust
#[derive(Debug, thiserror::Error)]
pub enum PiaError { ... }
```
Removing `thiserror` from `Cargo.toml` would break compilation. **Do not remove it.**

---

## 2. Current State Analysis

### 2.1 Confirmed File States (read during Phase 1)

**`Cargo.toml`:**
- `thiserror = "1"` — USED in `src/pia.rs` (PiaError enum)
- `serde_json = "1"` — already present; sufficient for history JSONL
- No `glib-build-tools` build-dependency yet
- No `chrono` — timestamps will use `std::time::SystemTime`

**`src/config.rs`:**
- `Config::save()` uses `std::fs::write` — NOT atomic; needs fix
- No `version: u32` schema field yet
- `config_path()` helper present and correct

**`src/state.rs`:**
- `read_wg_stats()` and `read_wg_handshake()` use bare `tokio::process::Command::new("wg")` — needs path hardening
- `poll_loop()` takes `state: Arc<RwLock<AppState>>` only — needs broadcast sender added
- `watch_vpn_unit_state()` present at line 545 — should also broadcast on state change

**`src/tray.rs`:**
- Uses `ksni::TrayService::new(tray).run()` — blocking; must change to `.spawn()` to allow concurrent broadcast listener
- `read_state()` is called on every menu render — acceptable; tray needs `update()` call to re-render

**`nix/module-gui.nix`:**
- Sets `services.pia-vpn.dnsServers = { ... }.${cfg.dns.provider}` — unconditional; needs `lib.mkDefault`

**`scripts/preflight.sh`:**
- Missing `cargo fmt --check` step — must be added as the first check

**`.github/workflows/`:** Does NOT exist — must be created.

**`src/ui.rs`:**
- `build_ui()` constructs `adw::NavigationView` with dashboard + server list pages
- Sidebar is built by `build_sidebar()` with static nav buttons
- History nav page will be pushed onto the existing `nav_view`

---

## 3. F9 — Connection History Pane

### 3.1 Data Model

**Log file:** `~/.local/state/vex-vpn/history.jsonl`  
Each line is a self-contained JSON object (one line per completed session):

```json
{"ts_start":1746791000,"ts_end":1746791330,"region":"US East","bytes_rx":1048576,"bytes_tx":524288,"disconnect_reason":"user"}
```

**`HistoryEntry` struct:**
```rust
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HistoryEntry {
    pub ts_start: u64,            // Unix epoch seconds (connection established)
    pub ts_end: u64,              // Unix epoch seconds (disconnected)
    pub region: String,           // Human-readable region name
    pub bytes_rx: u64,
    pub bytes_tx: u64,
    pub disconnect_reason: String, // "user", "error", "network", "watchdog"
}
```

No new dependencies. `ts_start`/`ts_end` are `u64` Unix epoch seconds obtained from `std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()`.

### 3.2 `src/history.rs` — New Module

```rust
//! Connection history — append-only JSONL log at
//! ~/.local/state/vex-vpn/history.jsonl

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct HistoryEntry {
    pub ts_start: u64,
    pub ts_end: u64,
    pub region: String,
    pub bytes_rx: u64,
    pub bytes_tx: u64,
    pub disconnect_reason: String,
}

pub fn history_path() -> PathBuf {
    let base = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".local").join("state")
        });
    base.join("vex-vpn").join("history.jsonl")
}

/// Append one completed-session entry to the JSONL log.
/// The file and its parent directory are created on demand.
/// I/O errors are logged and swallowed — history is best-effort.
pub fn append_entry(entry: &HistoryEntry) {
    let path = history_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let line = match serde_json::to_string(entry) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("history: failed to serialize entry: {}", e);
            return;
        }
    };
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            let _ = writeln!(f, "{}", line);
        }
        Err(e) => tracing::warn!("history: failed to open {:?}: {}", path, e),
    }
}

/// Read the most recent `n` entries in reverse-chronological order.
/// Returns an empty Vec on any I/O or parse error.
pub fn load_recent(n: usize) -> Vec<HistoryEntry> {
    let content = match std::fs::read_to_string(history_path()) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut entries: Vec<HistoryEntry> = content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    // Return newest first — reverse the order and take at most n.
    let start = entries.len().saturating_sub(n);
    entries.drain(..start);
    entries.reverse();
    entries
}

/// Format a duration in seconds as a human-readable string: "2h 5m", "45s".
pub fn format_duration(seconds: u64) -> String {
    if seconds >= 3600 {
        format!("{}h {}m", seconds / 3600, (seconds % 3600) / 60)
    } else if seconds >= 60 {
        format!("{}m {}s", seconds / 60, seconds % 60)
    } else {
        format!("{}s", seconds)
    }
}

/// Format a Unix timestamp as a local date string using offset from now.
/// Example: "Today 14:05", "Yesterday 09:30", "2026-05-07".
pub fn format_timestamp(ts: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let age = now.saturating_sub(ts);
    let hour = (ts % 86400) / 3600;
    let min = (ts % 3600) / 60;
    if age < 86400 {
        format!("Today {:02}:{:02}", hour, min)
    } else if age < 172800 {
        format!("Yesterday {:02}:{:02}", hour, min)
    } else {
        // Days ago — use day offset from epoch (approx; no TZ awareness needed)
        let days = ts / 86400;
        // 1970-01-01 was a Thursday; no proper calendar — just show elapsed days
        format!("{} days ago {:02}:{:02}", age / 86400, hour, min)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(125), "2m 5s");
        assert_eq!(format_duration(7530), "2h 5m");
    }

    #[test]
    fn test_round_trip_jsonl() {
        let e = HistoryEntry {
            ts_start: 1_700_000_000,
            ts_end: 1_700_000_330,
            region: "US East".to_string(),
            bytes_rx: 1024,
            bytes_tx: 512,
            disconnect_reason: "user".to_string(),
        };
        let line = serde_json::to_string(&e).unwrap();
        let decoded: HistoryEntry = serde_json::from_str(&line).unwrap();
        assert_eq!(decoded.region, e.region);
        assert_eq!(decoded.bytes_rx, e.bytes_rx);
    }
}
```

### 3.3 Integration: `src/state.rs`

**New field in `AppState`:**
```rust
/// Unix timestamp (seconds) when the current connection was established.
/// Set to Some when status transitions to Connected; cleared on disconnect.
pub connection_start_ts: Option<u64>,
```

**In `AppState::new()` / `AppState::new_with_config()`:** initialise to `None`.

**In `poll_loop`:** track `prev_status` (already exists) and add history write logic:
```rust
// After computing new_status, before the sleep:
let prev_disc = std::mem::discriminant(&prev_status);
let new_disc  = std::mem::discriminant(&new_status);
if prev_disc != new_disc {
    // Transition: was connected (or stale), now disconnected/error → write record
    if matches!(prev_status, ConnectionStatus::Connected | ConnectionStatus::KillSwitchActive | ConnectionStatus::Stale(_))
       && !new_status.is_connected()
    {
        let s = state.read().await;
        if let Some(ts_start) = s.connection_start_ts {
            let reason = match &new_status {
                ConnectionStatus::Error(_) => "error",
                ConnectionStatus::Disconnected => "user",
                _ => "network",
            };
            let entry = crate::history::HistoryEntry {
                ts_start,
                ts_end: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                region: s.region.as_ref().map(|r| r.name.clone()).unwrap_or_default(),
                bytes_rx: s.connection.as_ref().map(|c| c.rx_bytes).unwrap_or(0),
                bytes_tx: s.connection.as_ref().map(|c| c.tx_bytes).unwrap_or(0),
                disconnect_reason: reason.to_string(),
            };
            drop(s);
            tokio::task::spawn_blocking(move || crate::history::append_entry(&entry));
        }
        state.write().await.connection_start_ts = None;
    }

    // Transition: now connected → record start time
    if new_status.is_connected() && !matches!(prev_status, ConnectionStatus::Connected | ConnectionStatus::KillSwitchActive | ConnectionStatus::Stale(_)) {
        state.write().await.connection_start_ts = Some(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
    }
}
```

### 3.4 UI: History Nav Page in `src/ui.rs`

**New public function `build_history_page()`:**
```rust
pub fn build_history_page() -> adw::NavigationPage {
    let list_box = gtk4::ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    let entries = crate::history::load_recent(100);
    if entries.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title("No connections recorded yet");
        list_box.append(&row);
    } else {
        for e in &entries {
            let duration = crate::history::format_duration(e.ts_end.saturating_sub(e.ts_start));
            let when = crate::history::format_timestamp(e.ts_start);
            let row = adw::ActionRow::new();
            row.set_title(&e.region);
            row.set_subtitle(&format!("{} — {}", when, duration));
            list_box.append(&row);
        }
    }

    let scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .vexpand(true)
        .child(&list_box)
        .build();

    let clamp = adw::Clamp::new();
    clamp.set_child(Some(&scroll));
    clamp.set_maximum_size(600);
    clamp.set_margin_top(12);
    clamp.set_margin_bottom(12);
    clamp.set_margin_start(12);
    clamp.set_margin_end(12);

    adw::NavigationPage::builder()
        .title("Connection History")
        .child(&clamp)
        .build()
}
```

**Wiring in `build_ui()`:** After the server-row connection:
```rust
// Wire a History button in the sidebar to push the history page.
// The sidebar's "History" nav button (see build_sidebar) triggers this.
// Pass nav_view to build_sidebar or connect the signal after construction.
```

**Change to `build_sidebar()`:** Add a "History" `gtk4::Button` at the bottom of the sidebar nav buttons. Return it (or add a callback parameter) so `build_ui` can connect `nav_view.push(build_history_page())`.

**Simplest implementation:** Change `build_sidebar()` to return `(gtk4::Box, gtk4::Button)` where the second element is the history button, then in `build_ui`:
```rust
let (sidebar, history_btn) = build_sidebar();
root.append(&sidebar);
// ...
{
    let nav_view_h = nav_view.clone();
    history_btn.connect_clicked(move |_| {
        nav_view_h.push(&build_history_page());
    });
}
```

**`build_sidebar` return type change** is the only breaking internal change in `ui.rs`.

### 3.5 `src/lib.rs` — Expose `history` Module

```rust
pub mod config;
pub mod history;
```

---

## 4. F14 — HiDPI / Bundled SVG Icons

### 4.1 Context7-Verified Approach

Source: gtk4-rs book — confirmed pattern:

1. Create GResource XML manifest at `assets/icons/icons.gresource.xml`
2. Add `glib-build-tools` as a **build-dependency** in `Cargo.toml`
3. Create `build.rs` calling `glib_build_tools::compile_resources`
4. In `main.rs` (before `adw::Application::new()`), call `gio::resources_register_include!("icons.gresource")`
5. In the GTK `activate` handler, add the resource path to the icon theme

### 4.2 Icon Files to Create

**Path:** `assets/icons/`

```
assets/icons/
  icons.gresource.xml
  hicolor/
    scalable/
      apps/
        vex-vpn.svg                         # app icon (colour)
    symbolic/
      apps/
        network-vpn-symbolic.svg            # connected
        network-vpn-offline-symbolic.svg    # disconnected
        network-vpn-acquiring-symbolic.svg  # connecting / stale
        network-vpn-no-route-symbolic.svg   # kill switch active
```

**Minimal SVG for `network-vpn-symbolic.svg`** (symbolic icons must use `currentColor`):
```xml
<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16">
  <!-- Shield / lock symbol — pure path, currentColor, no fills -->
  <path fill="currentColor"
    d="M8 1 L14 4 L14 8 C14 12 8 15 8 15 C8 15 2 12 2 8 L2 4 Z
       M8 4 L5 5.5 L5 8.5 C5 10.5 8 12 8 12 C8 12 11 10.5 11 8.5 L11 5.5 Z"/>
</svg>
```

The implementation phase should use actual SVG paths. Minimal working stubs are sufficient for compilation; final artwork can be refined post-merge.

### 4.3 `assets/icons/icons.gresource.xml`

```xml
<?xml version="1.0" encoding="UTF-8"?>
<gresources>
  <gresource prefix="/com/vex/vpn/icons/hicolor/symbolic/apps">
    <file>hicolor/symbolic/apps/network-vpn-symbolic.svg</file>
    <file>hicolor/symbolic/apps/network-vpn-offline-symbolic.svg</file>
    <file>hicolor/symbolic/apps/network-vpn-acquiring-symbolic.svg</file>
    <file>hicolor/symbolic/apps/network-vpn-no-route-symbolic.svg</file>
  </gresource>
  <gresource prefix="/com/vex/vpn/icons/hicolor/scalable/apps">
    <file>hicolor/scalable/apps/vex-vpn.svg</file>
  </gresource>
</gresources>
```

### 4.4 `build.rs` (New File)

```rust
fn main() {
    glib_build_tools::compile_resources(
        &["assets/icons"],
        "assets/icons/icons.gresource.xml",
        "icons.gresource",
    );
}
```

The compiled `.gresource` file is placed in `$OUT_DIR` by `glib_build_tools`.

### 4.5 `Cargo.toml` Change

Add build-dependency:
```toml
[build-dependencies]
glib-build-tools = "0.18"
```

Version `0.18` matches `glib = "0.18"` in the existing `[dependencies]`.

### 4.6 `main.rs` Registration

Add **before** the `adw::Application::builder()` call:
```rust
// Embed and register compiled icon resources.
gio::resources_register_include!("icons.gresource")
    .expect("failed to register bundled GResources");
```

Add **inside `app.connect_activate`**, before building the UI:
```rust
// Register the bundled icon resource path with the default icon theme
// so that GTK can find our fallback symbolic icons.
if let Some(display) = gtk4::gdk::Display::default() {
    gtk4::IconTheme::for_display(&display)
        .add_resource_path("/com/vex/vpn/icons");
}
```

### 4.7 NixOS Package Icon Install (`flake.nix`)

Add to the `vex-vpn = craneLib.buildPackage` block:
```nix
postInstall = ''
  for size in scalable; do
    install -Dm644 assets/icons/hicolor/$size/apps/vex-vpn.svg \
      $out/share/icons/hicolor/$size/apps/vex-vpn.svg
  done
  for icon in network-vpn-symbolic network-vpn-offline-symbolic \
              network-vpn-acquiring-symbolic network-vpn-no-route-symbolic; do
    install -Dm644 assets/icons/hicolor/symbolic/apps/${icon}.svg \
      $out/share/icons/hicolor/symbolic/apps/${icon}.svg
  done
  install -Dm644 assets/icons/icons.gresource.xml \
    $out/share/vex-vpn/icons.gresource.xml
'';
```

### 4.8 `flake.nix` Source Filter Update

Update `certFilter` (which filters sources for the Crane build) to include SVG and gresource.xml:
```nix
certFilter = path: type:
  type == "directory" ||
  builtins.match ".*\\.crt$"          path != null ||
  builtins.match ".*\\.ui$"           path != null ||
  builtins.match ".*\\.policy$"       path != null ||
  builtins.match ".*\\.svg$"          path != null ||
  builtins.match ".*\\.gresource\\.xml$" path != null;
```

### 4.9 `flake.nix` NativeBuildInputs Update

Add `pkgs.glib` to `nativeBuildInputs` to ensure `glib-compile-resources` is available during the Crane build phase:
```nix
nativeBuildInputs = with pkgs; [
  pkg-config
  wrapGAppsHook4
  gobject-introspection
  glib   # provides glib-compile-resources for build.rs
];
```

### 4.10 System Tray Icon Limitation

The ksni `icon_name()` method is resolved by the **desktop environment's** icon theme lookup, not GTK's. GResource-embedded icons are NOT visible to the D-Bus status notifier protocol. On systems where `network-vpn-symbolic` family icons exist (GNOME with `adwaita-icon-theme`, KDE with `breeze`), the tray will work correctly. On minimal desktops without these icons, the tray will fall back to a blank icon. Implementing `icon_pixmap()` (which returns rasterised pixel data) would require `librsvg` and is deferred to Milestone F.

---

## 5. B8 — Tray Broadcast (Replace 3 s Poll Lag)

### 5.1 Design

Replace the current pattern (tray reads `AppState` on every menu render, only rendering when the menu opens) with a **push-based update** using `tokio::sync::broadcast`.

**Channel:** `tokio::sync::broadcast::channel::<()>(16)` — capacity 16 is sufficient (state changes are infrequent).

**Flow:**
```
poll_loop detects status change
    → sends () on broadcast::Sender<()>
    → tray task receives () via broadcast::Receiver<()>
    → calls ksni::Handle::update() to force tray re-render
    → tray calls icon_name() / title() / menu() with fresh state
```

### 5.2 `src/main.rs` Changes

```rust
// Create state-change broadcast channel.
let (state_change_tx, _dummy_rx) = tokio::sync::broadcast::channel::<()>(16);

// Pass sender to poll_loop.
let state_for_poll = app_state.clone();
let poll_tx = state_change_tx.clone();
rt.spawn(async move {
    state::poll_loop(state_for_poll, poll_tx).await;
});

// Pass sender to watch_vpn_unit_state (for immediate notification on D-Bus signal).
let state_for_vpn_watch = app_state.clone();
let vpn_watch_tx = state_change_tx.clone();
rt.spawn(async move {
    state::watch_vpn_unit_state(state_for_vpn_watch, vpn_watch_tx).await;
});

// Pass a fresh receiver to the tray thread.
let state_rx = state_change_tx.subscribe();
std::thread::spawn(move || {
    tray::run_tray(state_for_tray, tray_tx, tray_handle, state_rx);
});
```

Note: `_dummy_rx` keeps the channel alive until the first subscriber is ready. Alternatively, just drop it — the `subscribe()` call from the tray is the real receiver.

### 5.3 `src/state.rs` — `poll_loop` Signature Change

```rust
pub async fn poll_loop(
    state: Arc<RwLock<AppState>>,
    state_change_tx: tokio::sync::broadcast::Sender<()>,
) {
    let mut prev_status = ConnectionStatus::Disconnected;
    loop {
        // ... existing poll logic ...

        if std::mem::discriminant(&new_status) != std::mem::discriminant(&prev_status) {
            // Broadcast state change to tray (and any other subscribers).
            let _ = state_change_tx.send(());
            // ... existing desktop notification spawn ...
        }
        prev_status = new_status;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
```

### 5.4 `src/state.rs` — `watch_vpn_unit_state` Signature Change

```rust
pub async fn watch_vpn_unit_state(
    state: Arc<RwLock<AppState>>,
    state_change_tx: tokio::sync::broadcast::Sender<()>,
) {
    // ... existing D-Bus watch logic ...
    // After calling poll_once(&state).await, add:
    let _ = state_change_tx.send(());
}
```

### 5.5 `src/tray.rs` Changes

Change `run_tray` to use `ksni::TrayService::spawn()` and add broadcast listener:

```rust
pub fn run_tray(
    state: Arc<RwLock<AppState>>,
    tx: async_channel::Sender<TrayMessage>,
    handle: tokio::runtime::Handle,
    mut state_change_rx: tokio::sync::broadcast::Receiver<()>,
) {
    let tray = PiaTray { state, handle: handle.clone(), tx };

    // spawn() returns a Handle<PiaTray> that exposes update().
    // The tray service runs on ksni's internal thread.
    let tray_handle = ksni::TrayService::new(tray).spawn();

    // Block this OS thread driving the broadcast receiver.
    // On each signal, tell ksni to re-render by calling update().
    handle.block_on(async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            match state_change_rx.recv().await {
                Ok(()) | Err(RecvError::Lagged(_)) => {
                    tray_handle.update(|_| {});
                }
                Err(RecvError::Closed) => break,
            }
        }
    });
}
```

`handle.block_on()` is valid from this OS thread because it is NOT inside any Tokio runtime — it was spawned with `std::thread::spawn`. Internally the future is driven on the main Tokio runtime's thread pool.

---

## 6. B15 — CI/CD: `cargo fmt --check`, GitHub Actions, GitLab CI

### 6.1 `scripts/preflight.sh` — Add `cargo fmt --check`

Add as the **first** step (before clippy):
```bash
echo "--- Checking formatting ---"
nix develop --command cargo fmt --check
```

Full updated file:
```bash
#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

echo "--- Checking formatting ---"
nix develop --command cargo fmt --check

echo "--- Running clippy ---"
nix develop --command cargo clippy -- -D warnings

echo "--- Running debug build ---"
nix develop --command cargo build

echo "--- Running tests ---"
nix develop --command cargo test

echo "--- Running release build ---"
nix develop --command cargo build --release

echo "--- Running nix build ---"
nix build

echo "--- Preflight passed ---"
```

### 6.2 `.github/workflows/ci.yml` (New File)

```yaml
name: CI

on:
  push:
    branches: [main]
  pull_request:

jobs:
  build-and-test:
    runs-on: ubuntu-latest
    steps:
      - name: Checkout
        uses: actions/checkout@v4

      - name: Install Nix
        uses: DeterminateSystems/nix-installer-action@v13

      - name: Enable Magic Nix Cache
        uses: DeterminateSystems/magic-nix-cache-action@v7

      - name: Check formatting
        run: nix develop --command cargo fmt --check

      - name: Clippy (zero-warning gate)
        run: nix develop --command cargo clippy -- -D warnings

      - name: Run tests
        run: nix develop --command cargo test

      - name: Nix build (Crane — release + reproducibility check)
        run: nix build
```

**Notes:**
- `DeterminateSystems/nix-installer-action@v13` — installs Nix with flakes enabled
- `DeterminateSystems/magic-nix-cache-action@v7` — caches the Nix store (Determinate Systems' free tier); can be swapped for `cachix/cachix-action` if a Cachix cache is preferred
- `nix build` invokes Crane's `buildPackage` which runs the full release build + all checks implicitly; this is the strongest CI gate

### 6.3 `.gitlab-ci.yml` (New File)

```yaml
stages:
  - validate
  - build
  - test

variables:
  NIX_CONFIG: "experimental-features = nix-command flakes"

default:
  image: nixos/nix:latest
  before_script:
    - nix --version
    # Ensure flake features are enabled for the Nix invocations below
    - echo "experimental-features = nix-command flakes" >> /etc/nix/nix.conf

fmt:
  stage: validate
  script:
    - nix develop --command cargo fmt --check

clippy:
  stage: validate
  script:
    - nix develop --command cargo clippy -- -D warnings

test:
  stage: test
  script:
    - nix develop --command cargo test

nix-build:
  stage: build
  script:
    - nix build
  artifacts:
    paths:
      - result/
    expire_in: 1 week
```

**Caching note:** GitLab CI's built-in cache can be added with:
```yaml
cache:
  key: nix-store-$CI_COMMIT_REF_SLUG
  paths:
    - /nix/store
```
However, caching `/nix/store` in GitLab CI is less effective than Cachix/magic-nix-cache. The initial pipeline will be slow; subsequent runs benefit from layer caching via the Docker image. A Cachix integration can be added post-Milestone-E.

---

## 7. Small Fixes

### 7.1 `config.toml` Atomic Write + Schema Version

**Location:** `src/config.rs`

**Add `version` field to `Config`:**
```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_schema_version")]
    pub version: u32,
    pub auto_connect: bool,
    // ... rest unchanged ...
}

fn default_schema_version() -> u32 { 1 }
```

In `Default for Config`, add `version: 1`.

**Replace `Config::save()` with atomic write:**
```rust
pub fn save(&self) -> Result<()> {
    self.save_to(&config_path())
}

pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(self)?;
    let tmp_path = path.with_extension("toml.tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp_path)?;
        f.write_all(content.as_bytes())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}
```

**Also expose `save_to` for integration test isolation.** (The existing `load_from` already does this; `save_to` mirrors it.)

**Update integration test `tests/config_integration.rs`** to include `version` in the `original` struct and assert round-trip.

### 7.2 NixOS DNS `lib.mkDefault` — `nix/module-gui.nix`

**Current (unconditional assignment):**
```nix
services.pia-vpn.dnsServers = {
  pia        = [ "10.0.0.241" "10.0.0.242" ];
  google     = [ "8.8.8.8" "8.8.4.4" ];
  cloudflare = [ "1.1.1.1" "1.0.0.1" ];
  custom     = cfg.dns.customServers;
}.${cfg.dns.provider};
```

**Fix (user's explicit `dnsServers` takes precedence):**
```nix
services.pia-vpn.dnsServers = lib.mkDefault (
  {
    pia        = [ "10.0.0.241" "10.0.0.242" ];
    google     = [ "8.8.8.8" "8.8.4.4" ];
    cloudflare = [ "1.1.1.1" "1.0.0.1" ];
    custom     = cfg.dns.customServers;
  }.${cfg.dns.provider}
);
```

This allows a user with both `services.vex-vpn.enable = true` and `services.pia-vpn.dnsServers = [ "9.9.9.9" ]` in their system config to override the GUI module's default without a NixOS merge conflict.

### 7.3 `wg` Wrapper Path Hardening — `src/state.rs`

**Current (both functions):**
```rust
let output = tokio::process::Command::new("wg")
```

**Fix — add a module-level helper:**
```rust
/// Returns the path to the `wg` binary, preferring the NixOS capability wrapper
/// at `/run/wrappers/bin/wg` (which has `CAP_NET_ADMIN` set via `security.wrappers`).
/// Falls back to `wg` in PATH for non-NixOS environments.
fn wg_binary() -> &'static str {
    if std::path::Path::new("/run/wrappers/bin/wg").exists() {
        "/run/wrappers/bin/wg"
    } else {
        "wg"
    }
}
```

Apply in `read_wg_stats`:
```rust
let output = tokio::process::Command::new(wg_binary())
    .args(["show", interface, "transfer"])
    .output()
    .await?;
```

Apply in `read_wg_handshake`:
```rust
let output = tokio::process::Command::new(wg_binary())
    .args(["show", interface, "latest-handshakes"])
    .output()
    .await
    .ok()?;
```

Note: `std::path::Path::new(...).exists()` is synchronous. Calling it from an async context inside `poll_once` is acceptable because it is a single `stat(2)` syscall (sub-millisecond) and does not block the thread pool.

---

## 8. Cargo.toml Changes Summary

```toml
[build-dependencies]
glib-build-tools = "0.18"
```

No other `[dependencies]` changes. `thiserror` stays.

---

## 9. Files to Modify / Create (Implementation Checklist)

### New Files

| File | Purpose |
|------|---------|
| `src/history.rs` | F9 history module (HistoryEntry, append_entry, load_recent) |
| `build.rs` | F14 compile_resources invocation |
| `assets/icons/icons.gresource.xml` | F14 GResource manifest |
| `assets/icons/hicolor/scalable/apps/vex-vpn.svg` | F14 app icon |
| `assets/icons/hicolor/symbolic/apps/network-vpn-symbolic.svg` | F14 |
| `assets/icons/hicolor/symbolic/apps/network-vpn-offline-symbolic.svg` | F14 |
| `assets/icons/hicolor/symbolic/apps/network-vpn-acquiring-symbolic.svg` | F14 |
| `assets/icons/hicolor/symbolic/apps/network-vpn-no-route-symbolic.svg` | F14 |
| `.github/workflows/ci.yml` | B15 GitHub Actions |
| `.gitlab-ci.yml` | B15 GitLab CI |

### Modified Files

| File | Changes |
|------|---------|
| `src/main.rs` | Wire broadcast channel; `gio::resources_register_include!`; icon theme path |
| `src/state.rs` | `poll_loop` + `watch_vpn_unit_state` signature (broadcast sender); `wg_binary()` helper; `connection_start_ts` in AppState; history write in poll_loop |
| `src/tray.rs` | `run_tray` uses `.spawn()` + broadcast receiver |
| `src/config.rs` | `version: u32` field; atomic `save_to()`; expose `save_to` |
| `src/ui.rs` | `build_sidebar()` returns `(gtk4::Box, gtk4::Button)`; wire history nav page in `build_ui`; new `build_history_page()` function |
| `src/lib.rs` | Add `pub mod history` |
| `Cargo.toml` | Add `[build-dependencies]` section with `glib-build-tools = "0.18"` |
| `flake.nix` | Add source filter for `.svg`/`.gresource.xml`; add `pkgs.glib` to nativeBuildInputs; add `postInstall` for icons |
| `nix/module-gui.nix` | Wrap DNS assignment in `lib.mkDefault` |
| `scripts/preflight.sh` | Add `cargo fmt --check` as first step |
| `tests/config_integration.rs` | Add `version` field to test structs; add `save_to` round-trip test |

---

## 10. Testing Additions

### New Unit Tests (in `src/history.rs`)

- `test_format_duration` — validates duration formatting
- `test_round_trip_jsonl` — serialize → deserialize round trip
- `test_load_recent_empty` — returns empty vec when file absent
- `test_load_recent_order` — entries returned in reverse-chronological order
- `test_history_path` — respects `XDG_STATE_HOME` env var

### New Integration Tests (add to `tests/config_integration.rs`)

- `save_to_path_round_trip` — tests `Config::save_to()` + `Config::load_from()` atomicity
- `version_field_defaults_to_1` — old TOML without `version` field deserializes with `version = 1`

### Preflight Validation

`scripts/preflight.sh` must pass end-to-end with all five steps including the new `cargo fmt --check`.

---

## 11. Risks and Mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|-----------|
| `glib-compile-resources` not in PATH during `nix build` | Medium | Add `pkgs.glib` to `nativeBuildInputs` in `flake.nix`; confirmed approach from gtk4-rs book |
| `ksni::TrayService::spawn()` API may differ from `.run()` | Low | ksni 0.2.x exposes `spawn()` → `Handle<T>` with `update()`; verify during impl |
| `handle.block_on()` from tray OS thread panics if runtime shuts down first | Low | The main runtime outlives the tray thread (tray spawned before `app.run()`); ordering guaranteed by `main.rs` teardown sequence |
| History JSONL grows unboundedly | Low | `load_recent(100)` bounds UI; file size grows ~200 B/session; at 1 session/hour = ~1.7 MB/year. Acceptable without pruning. Add note to README |
| Atomic rename across filesystems | Very Low | `config.toml` and `.tmp` are in the same `~/.config/vex-vpn/` dir; same inode, same filesystem |
| SVG icon sizes incorrect for HiDPI | Medium | GTK4 SVG rendering is resolution-independent; symbolic icons at 16×16 viewport are standard for GNOME |
| GitLab CI `/nix/store` cache not persisting | Medium | First pipeline slow; subsequent run benefits from Docker layer cache on `nixos/nix:latest` image. Acceptable for Milestone E |
| `cargo fmt --check` fails if code was committed without `rustfmt` pass | High (first run) | Implementation agent must run `cargo fmt` before committing. Preflight enforces this going forward |

---

## 12. Deferred Items (Milestone F)

- **F10** — `gettext-rs` + `.po` file scaffolding + `cargo i18n`
- **F13** — `libshumate-rs` map view
- **Tray pixel icon** — `librsvg`-based `icon_pixmap()` for non-freedesktop desktops
- **Cachix integration** — for faster GitLab CI nix-store caching
- **`clippy::unwrap_used`** — deny at crate root (noted in `docs/PROJECT_ANALYSIS.md`)
- **`wiremock` PIA HTTP fixtures** — `tests/pia_integration.rs`
- **`oo7` Secret Service** — blocked on `oo7` migrating away from `zbus 4.x`

---

## 13. Implementation Verification Checklist

The implementation agent MUST verify:

- [ ] `thiserror` remains in `Cargo.toml` (it IS used in `src/pia.rs`)
- [ ] `cargo fmt --check` passes before any commit
- [ ] `nix develop --command cargo clippy -- -D warnings` exits 0
- [ ] `nix develop --command cargo build` exits 0
- [ ] `nix develop --command cargo test` exits 0 (including new history tests)
- [ ] `nix develop --command cargo build --release` exits 0
- [ ] `nix build` exits 0 (validates Crane filter includes `.svg`/`.gresource.xml`)
- [ ] `scripts/preflight.sh` exits 0 end-to-end
- [ ] History file is created at `~/.local/state/vex-vpn/history.jsonl` on first disconnect
- [ ] History nav page shows "No connections recorded yet" when empty
- [ ] Tray icon changes immediately (< 1 poll cycle) when VPN connects/disconnects
- [ ] GTK app resolves `network-vpn-symbolic` from GResource on a desktop without GNOME icon theme
- [ ] `config.toml` save is atomic (no partial write visible on power loss via rename semantics)
- [ ] `services.pia-vpn.dnsServers` can be overridden by user config without Nix merge error
- [ ] `/run/wrappers/bin/wg` is used when present; falls back gracefully
