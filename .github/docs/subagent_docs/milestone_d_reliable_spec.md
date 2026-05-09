# Milestone D — Make it Reliable: Research & Specification

**Date:** 2026-05-09  
**Author:** Phase 1 Research subagent  
**Status:** DRAFT — pending implementation

---

## 0. Executive Summary

Milestone D targets six engineering tracks that transform vex-vpn from "works on the happy path" to "reliable under real network conditions":

| Track | Items | Ship in D? |
|-------|-------|-----------|
| Auto-reconnect | F7 | ✅ Yes |
| DNS leak test | F8 | ✅ Yes (heuristic only) |
| Handshake watchdog | F12 | ✅ Yes |
| Architecture hardening | B1 | ✅ Yes |
| Error handling hardening | B2 | ✅ Yes |
| D-Bus proxy caching + PropertiesChanged | B3 | ✅ Yes (trigger-on-event pattern, safe) |
| Integration tests | — | ✅ Yes (config, secrets, PIA HTTP) |

---

## 1. Scope Decision

### 1.1 Ship in Milestone D

| Item | Rationale |
|------|-----------|
| **B1** `process::exit` → `app.quit()` | One-line fix, trivially safe. |
| **B1** Tray channel `async_channel` migration | Medium complexity, well-understood pattern; unblocks correct re-activation behaviour. |
| **B2** `Config::load` → `Result<Config>` | All call sites are known; UI banner already exists for surfacing errors. |
| **B2** `anyhow::Context` audit | Pure mechanical change, zero risk. |
| **B3** `SystemdManagerProxy` already cached in `OnceCell` (connection). Cache the *manager proxy* itself via a second `OnceCell`. | Low risk. |
| **B3** `PropertiesChanged` subscription for `ActiveState` via **trigger pattern** (signals trigger `poll_once()`, not direct writes) | Eliminates dual-write race; safe. |
| **F7** NetworkManager `StateChanged` subscriber task | New async task, debounced — risk is managed. |
| **F8** DNS leak heuristic | `/etc/resolv.conf` parse — zero new deps, deterministic. |
| **F12** WireGuard handshake watchdog | Augments existing `poll_once()` — low risk. |
| Integration tests | New files only — no production code touched. |

### 1.2 Deferred to Milestone E

| Item | Reason |
|------|--------|
| F8 tokio async DNS probe (hickory-resolver) | New heavyweight dep; heuristic is sufficient for D. |
| B8 Tray state-change broadcast via `tokio::sync::broadcast` | Non-blocking polish — defer. |
| B9 Config schema version + atomic `config.toml` write | Low severity for D's focus. |
| F9/F10/F11/F13/F14 | Out of scope for reliability milestone. |

### 1.3 Concurrency safety note on B3

The poll loop (`poll_once`) and a `PropertiesChanged` signal stream would **both** update `AppState.status`.  
**Resolution:** The `PropertiesChanged` handler calls `poll_once(state.clone())` rather than writing directly to `AppState`. This keeps a single writer pattern and avoids partial state.

---

## 2. Current Codebase State (from Task A reading)

### 2.1 Key files

| File | Summary |
|------|---------|
| `src/main.rs` | Tokio runtime, GTK app, tray spawn, `connect_activate` with `take()` anti-pattern, `std::process::exit` at end |
| `src/state.rs` | `AppState`, `ConnectionStatus`, `poll_loop`, `poll_once` (7 concurrent I/O ops), `read_wg_stats` |
| `src/config.rs` | `Config::load() -> Self` (swallows parse errors via `unwrap_or_default()`), `Config::save() -> Result<()>` |
| `src/dbus.rs` | `SYSTEM_CONN: OnceCell<Connection>`, `SystemdManagerProxy` rebuilt per call, `SystemdUnitProxy` per call |
| `src/tray.rs` | `PiaTray`, `run_tray()`, "Quit" calls `std::process::exit(0)` |
| `src/ui.rs` | `build_ui()` takes `Option<Receiver<TrayMessage>>` — polling the tray channel |
| `src/ui_prefs.rs` | Three pages: Connection / Privacy / Advanced. No `auto_reconnect` toggle yet. |
| `src/pia.rs` | `PiaClient::generate_token`, `fetch_server_list`, `measure_latency`. Server list format: JSON + `\n` + base64 sig. |
| `src/secrets.rs` | Full implementation with `anyhow::Context`, permissions check — **model for B2 style**. |

### 2.2 Specific error-handling issues found (B2 callouts)

| Location | Issue |
|----------|-------|
| `src/config.rs:73` | `toml::from_str(&content).unwrap_or_default()` — TOML parse errors silently revert all settings to defaults. |
| `src/config.rs:65` | `std::fs::read_to_string(&path)` — `Err` returns `Self::default()` with no log at `warn!` level. |
| `src/state.rs:272` | `parts[1].parse::<u64>().unwrap_or(0)` — silently ignores malformed `wg show transfer` output. |
| `src/state.rs:273` | `parts[2].parse::<u64>().unwrap_or(0)` — same. |
| `src/main.rs:35` | `config::Config::load()` — return type is `Config`, not `Result<Config>`, so callers cannot observe failures. |
| `src/main.rs:242` | `std::process::exit(exit_code.into())` — skips `Drop` for Tokio runtime; pending writes may be lost. |
| `src/tray.rs:113` | `std::process::exit(0)` — same problem, bypasses clean shutdown. |
| `src/main.rs:79` | `tray_rx.lock().unwrap().take()` — `take()` leaves `None` for any subsequent `connect_activate` call (second instance or re-activation). |
| `src/helper.rs:102` | `let config = crate::config::Config::load();` — helper calls `load()` mid-operation; if it silently defaulted, wrong `allowed_ifaces` may be used. |
| `src/ui_prefs.rs:55,72,95` | Three calls to `Config::load()` inside GTK callbacks — repeated disk reads without error surfacing. |

---

## 3. F7 — Auto-Reconnect on Network Change

### 3.1 Motivation

When the host system switches networks (Wi-Fi roam, DHCP renewal, suspend/resume), the WireGuard kernel interface typically loses its handshake. systemd may report the service as still `active`, so the VPN appears connected but traffic is dropped.

### 3.2 NetworkManager D-Bus interface

**Interface:** `org.freedesktop.NetworkManager`  
**Path:** `/org/freedesktop/NetworkManager`  
**Signal:** `StateChanged(u state)` — emitted when overall connectivity changes.  
**Property:** `State` (u32) — same enum values.

NM State enum:

| Value | Constant | Meaning |
|-------|----------|---------|
| 0 | NM_STATE_UNKNOWN | Unknown |
| 10 | NM_STATE_ASLEEP | Wifi/networking disabled |
| 20 | NM_STATE_DISCONNECTED | No active network connection |
| 30 | NM_STATE_DISCONNECTING | Teardown in progress |
| 40 | NM_STATE_CONNECTING | Establishing connection |
| 50 | NM_STATE_CONNECTED_LOCAL | Has local connectivity only |
| 60 | NM_STATE_CONNECTED_SITE | Has site-wide connectivity |
| 70 | NM_STATE_CONNECTED_GLOBAL | Fully connected to internet |

### 3.3 zbus 3.x proxy definition

Add to `src/dbus.rs`:

```rust
#[dbus_proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait NetworkManager {
    /// Emitted when overall connectivity state changes.
    /// `state`: one of the NM_STATE_* u32 constants.
    #[dbus_proxy(signal)]
    fn state_changed(&self, state: u32) -> zbus::Result<()>;

    /// Current global connectivity state.
    #[dbus_proxy(property)]
    fn state(&self) -> zbus::Result<u32>;
}
```

Generated by the macro: `receive_state_changed().await?` → `impl Stream<Item = StateChangedMessage>`.

### 3.4 Subscriber task design

Add to `src/dbus.rs`:

```rust
/// Subscribe to NetworkManager StateChanged and restart the VPN unit when the
/// connection comes back up. Runs as a background tokio task.
pub async fn watch_network_manager(
    state: Arc<RwLock<AppState>>,
    auto_reconnect_rx: tokio::sync::watch::Receiver<bool>,
) {
    let conn = match system_conn().await {
        Ok(c) => c,
        Err(e) => { warn!("NM watch: D-Bus unavailable: {}", e); return; }
    };
    let proxy = match NetworkManagerProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => { warn!("NM watch: proxy unavailable: {}", e); return; }
    };
    let mut stream = match proxy.receive_state_changed().await {
        Ok(s) => s,
        Err(e) => { warn!("NM watch: StateChanged subscribe failed: {}", e); return; }
    };

    let mut prev_nm_state: u32 = 0;
    while let Some(msg) = stream.next().await {
        let args = match msg.args() {
            Ok(a) => a,
            Err(_) => continue,
        };
        let new_nm_state = args.state;

        // React only when transitioning TO fully connected FROM a non-connected state.
        let was_disconnected = prev_nm_state != NM_CONNECTED_GLOBAL;
        let now_connected   = new_nm_state == NM_CONNECTED_GLOBAL;

        if now_connected && was_disconnected {
            let is_auto = *auto_reconnect_rx.borrow();
            let vpn_is_connected = state.read().await.status.is_connected();

            if is_auto && vpn_is_connected {
                info!("Network restored — debouncing VPN reconnect (2 s)");
                tokio::time::sleep(Duration::from_secs(2)).await;
                // Confirm still connected to NM and VPN still reports connected.
                if state.read().await.status.is_connected() {
                    info!("Auto-reconnect: restarting pia-vpn.service");
                    if let Err(e) = restart_vpn_unit().await {
                        warn!("Auto-reconnect failed: {}", e);
                    }
                }
            }
        }
        prev_nm_state = new_nm_state;
    }
    warn!("NM StateChanged stream ended unexpectedly");
}

pub const NM_CONNECTED_GLOBAL: u32 = 70;

async fn restart_vpn_unit() -> Result<()> {
    stop_unit("pia-vpn.service").await?;
    tokio::time::sleep(Duration::from_millis(500)).await;
    start_unit("pia-vpn.service").await
}
```

Spawned from `src/main.rs` poll_loop startup:
```rust
let (auto_reconnect_tx, auto_reconnect_rx) =
    tokio::sync::watch::channel(cfg.auto_reconnect);

let state_for_nm = app_state.clone();
rt.spawn(async move {
    dbus::watch_network_manager(state_for_nm, auto_reconnect_rx).await;
});
```

The `watch::Sender<bool>` is updated when the user toggles the preference.

### 3.5 Config field

Add to `src/config.rs` `Config` struct:
```rust
#[serde(default = "default_true")]
pub auto_reconnect: bool,
```
```rust
fn default_true() -> bool { true }
```

### 3.6 Preferences UI

In `src/ui_prefs.rs` `build_advanced_page()`, add after the auto-connect row:

```rust
let ar_row = adw::SwitchRow::builder()
    .title("Auto-Reconnect")
    .subtitle("Restart VPN tunnel when network connectivity is restored")
    .active(cfg.auto_reconnect)
    .build();
ar_row.connect_active_notify(move |row| {
    let mut c = Config::load();  // note: after B2, this is Config::load().unwrap_or_default()
    c.auto_reconnect = row.is_active();
    if let Err(e) = c.save() {
        tracing::error!("save config (auto_reconnect): {}", e);
    }
    // TODO: update the watch::Sender — requires threading sender through to UI.
    // Defer this cross-thread update to a future cleanup. For D, the pref persists
    // and takes effect on next launch.
});
group.add(&ar_row);
```

**Note:** Live propagation of the preference to the already-running NM watcher requires threading a `tokio::sync::watch::Sender<bool>` through to the UI. This is acceptable complexity for D — implement the persistence now, live update as a follow-up.

### 3.7 AppState field

Add to `AppState`:
```rust
pub auto_reconnect: bool,  // mirrors Config::auto_reconnect at startup
```

### 3.8 Risks

| Risk | Mitigation |
|------|-----------|
| NM not present (non-NM systems, plain dhcpcd) | `watch_network_manager` exits gracefully on proxy failure — no panic. |
| Thrashing on brief disconnects | 2 s debounce window. |
| Double-restart if poll_loop also triggers | The NM watcher calls `restart_vpn_unit()` which is an explicit stop+start. The poll_loop only checks service state, not triggers restarts. No conflict. |

---

## 4. F8 — DNS Leak Test (Heuristic)

### 4.1 Motivation

A DNS leak occurs when the OS resolves hostnames via nameservers outside the VPN tunnel. PIA's WireGuard gateway assigns `10.0.0.241` as the DNS server. If `/etc/resolv.conf` lists other nameservers while the VPN is connected, DNS queries may escape the tunnel.

### 4.2 Design (heuristic, no new deps)

Add a pure function in `src/state.rs`:

```rust
/// Returns `None` if no leak detected, `Some(Vec<String>)` of non-PIA
/// nameserver IPs found in /etc/resolv.conf when the VPN is connected.
/// This is a heuristic — it does not probe live DNS traffic.
pub fn check_dns_leak_hint() -> Option<Vec<String>> {
    let content = std::fs::read_to_string("/etc/resolv.conf").ok()?;
    let non_pia: Vec<String> = content
        .lines()
        .filter(|l| l.starts_with("nameserver"))
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter(|ip| !ip.starts_with("10.0.0."))  // PIA DNS range
        .map(|s| s.to_string())
        .collect();
    if non_pia.is_empty() { None } else { Some(non_pia) }
}
```

### 4.3 AppState field

Add to `AppState`:
```rust
/// Set when connected and non-PIA nameservers are detected in /etc/resolv.conf.
pub dns_leak_hint: Option<Vec<String>>,
```

### 4.4 Integration in poll_once

In `poll_once()`, after computing `new_status`:
```rust
let dns_leak = if new_status.is_connected() {
    check_dns_leak_hint()
} else {
    None
};
// In the write block:
s.dns_leak_hint = dns_leak;
```

### 4.5 UI integration

In `src/ui_prefs.rs` Advanced page (or a new "Diagnostics" section in the Connection page), add an `adw::ActionRow` that shows the result of the leak check when connected:

```rust
let dns_row = adw::ActionRow::builder()
    .title("DNS Leak Check")
    .subtitle("Checks /etc/resolv.conf for non-PIA nameservers")
    .build();
// Updated by the refresh timer in ui.rs
```

The `ui.rs` refresh timer (already exists, runs every 3 s) should update this row's subtitle with either "No leak detected" or "⚠ Non-VPN DNS: 8.8.8.8" using the state's `dns_leak_hint`.

**Label it clearly as a heuristic**: "Heuristic check — does not test live DNS traffic."

### 4.6 Risks

| Risk | Mitigation |
|------|-----------|
| `/etc/resolv.conf` may be a symlink (systemd-resolved) | `read_to_string` follows symlinks — works correctly. |
| PIA assigns a different DNS range in some regions | Check against common PIA ranges (`10.0.0.*`). Acceptable false-positive rate for a heuristic. |
| On NixOS, `resolvconf` or `networkmanager` manages the file | Same — file always present; content varies. |

---

## 5. F12 — WireGuard Handshake Watchdog

### 5.1 Motivation

`wg show … latest-handshakes` reports the timestamp of the last successful handshake per peer. If no handshake in > 180 s, the tunnel is stale even if systemd shows the service as `active`.

### 5.2 New `ConnectionStatus` variant

In `src/state.rs`:
```rust
pub enum ConnectionStatus {
    // ... existing ...
    /// Tunnel is up (systemd active) but WireGuard peer handshake is stale.
    /// Elapsed seconds since last handshake provided for UI display.
    Stale(u64),
}
```

Update `label()`:
```rust
Self::Stale(_) => "Reconnecting…",
```

Update `is_connected()`:
```rust
pub fn is_connected(&self) -> bool {
    matches!(self, Self::Connected | Self::KillSwitchActive | Self::Stale(_))
}
```

Add:
```rust
pub fn is_stale(&self) -> bool {
    matches!(self, Self::Stale(_))
}
```

### 5.3 `read_wg_handshake` function

Add to `src/state.rs`:
```rust
/// Parse `wg show <iface> latest-handshakes`.
/// Returns the seconds elapsed since the most recent peer handshake,
/// or `None` if no handshake has occurred yet or the command fails.
async fn read_wg_handshake(interface: &str) -> Option<u64> {
    let output = tokio::process::Command::new("wg")
        .args(["show", interface, "latest-handshakes"])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    // Format: <pubkey>\t<unix_timestamp>
    // Timestamp is 0 if no handshake.
    let mut latest: Option<u64> = None;
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            if let Ok(ts) = parts[1].parse::<u64>() {
                if ts > 0 {
                    latest = Some(latest.map_or(ts, |prev| prev.max(ts)));
                }
            }
        }
    }

    let ts = latest?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(now.saturating_sub(ts))
}
```

### 5.4 Integration in `poll_once`

In `poll_once`, after computing `new_status`:

```rust
// Handshake watchdog — only meaningful when service is active.
let new_status = if matches!(new_status, ConnectionStatus::Connected) {
    let elapsed = read_wg_handshake(&interface).await;
    match elapsed {
        Some(e) if e > 180 => ConnectionStatus::Stale(e),
        _ => ConnectionStatus::Connected,
    }
} else {
    new_status
};
```

### 5.5 Auto-restart after prolonged Stale

Track consecutive stale cycles in `AppState`:
```rust
/// How many consecutive 3-second poll cycles the status has been Stale.
pub stale_cycles: u32,
```

In `poll_loop`, after status transition detection:
```rust
if let ConnectionStatus::Stale(_) = new_status {
    let mut s = state.write().await;
    s.stale_cycles += 1;
    if s.stale_cycles >= 10 {  // 10 × 3 s = 30 s
        s.stale_cycles = 0;
        drop(s);
        info!("Handshake watchdog: restarting pia-vpn.service");
        if let Err(e) = dbus::restart_vpn_unit().await {
            warn!("Watchdog restart failed: {}", e);
        }
    }
} else {
    state.write().await.stale_cycles = 0;
}
```

**Note:** `restart_vpn_unit()` is defined in `dbus.rs` (see §3.3 above).

### 5.6 UI treatment

In `src/ui.rs` refresh timer, add handling for `Stale`:
```rust
ConnectionStatus::Stale(_) => {
    // Amber warning state — same CSS class as Connecting
    pill.remove_css_class("state-connected");
    pill.add_css_class("state-connecting");
    pill.set_label("RECONNECTING");
    pill.set_tooltip_text(Some("WireGuard handshake is stale — reconnecting…"));
}
```

In `src/tray.rs`, add `Stale` arm to `icon_name()` and `title()`:
```rust
ConnectionStatus::Stale(_) => "network-vpn-acquiring-symbolic",
// title():
ConnectionStatus::Stale(_) => "PIA — Reconnecting…".to_string(),
```

---

## 6. B1 — Architecture Hardening

### 6.1 Replace `std::process::exit`

**Problem:** Two sites call `std::process::exit`, bypassing `Drop` on the Tokio runtime and potentially losing pending config writes.

**Sites:**
- `src/main.rs:242` — `std::process::exit(exit_code.into())`
- `src/tray.rs:113` — `std::process::exit(0)` in the Quit menu item

**Fix for `main.rs`:**  
Remove `std::process::exit`. The `adw::Application::run()` return value is a `glib::ExitCode`. The `main` function signature is `fn main() -> Result<()>`, which returns `Ok(())` after `app.run()`. The runtime `rt` drops at end of `main`. Change:
```rust
// BEFORE:
let exit_code = app.run();
std::process::exit(exit_code.into());

// AFTER:
let _exit_code = app.run();
// Runtime drops here, flushing async tasks.
Ok(())
```

**Fix for `tray.rs`:**  
Replace `std::process::exit(0)` with a `TrayMessage::Quit` send. The UI thread receives it and calls `app.quit()`.

```rust
// tray.rs menu Quit item:
activate: Box::new(|tray: &mut PiaTray| {
    let _ = tray.tx.try_send(TrayMessage::Quit);  // async_channel try_send
}),
```

In `src/ui.rs`, the tray message handler already handles `TrayMessage::ShowWindow`. Add:
```rust
TrayMessage::Quit => {
    app.quit();
}
```

**Note:** `TrayMessage::Quit` is already defined in `tray.rs` (with `#[allow(dead_code)]`).

### 6.2 Replace tray channel with `async_channel`

**Problem:** `Arc<Mutex<Option<Receiver<TrayMessage>>>>` requires `.take()` in `connect_activate`, leaving `None` for any subsequent activation (GNOME can re-activate an existing app instance).

**Fix:**

Add `async-channel = "2"` to `[dependencies]` in `Cargo.toml`.

In `src/main.rs`:
```rust
// BEFORE:
let (tray_tx, tray_rx) = std::sync::mpsc::sync_channel::<TrayMessage>(8);
// ...
let tray_rx = Arc::new(std::sync::Mutex::new(Some(tray_rx)));
// In connect_activate:
let rx = tray_rx.lock().unwrap().take();
build_and_show_main_window(app, state_for_ui.clone(), rx);

// AFTER:
let (tray_tx, tray_rx) = async_channel::bounded::<TrayMessage>(8);
// tray_rx is Clone — no take() needed.
// In connect_activate (closure captures tray_rx by clone):
let tray_rx_clone = tray_rx.clone();
app.connect_activate(move |app| {
    // tray_rx_clone can be cloned repeatedly; each clone receives messages.
    build_and_show_main_window(app, state_for_ui.clone(), tray_rx_clone.clone());
});
```

In `src/tray.rs`:
- Change `tx: std::sync::mpsc::SyncSender<TrayMessage>` to `tx: async_channel::Sender<TrayMessage>`
- Change `tx.send(TrayMessage::ShowWindow)` to `tx.try_send(TrayMessage::ShowWindow)`
- Change `run_tray` signature accordingly

In `src/ui.rs`:
- Change `rx: Option<std::sync::mpsc::Receiver<TrayMessage>>` to `rx: Option<async_channel::Receiver<TrayMessage>>`
- The existing `glib::spawn_future_local` polling loop changes from `std::sync::mpsc` to `async_channel::Receiver::recv().await`:
```rust
// BEFORE (likely uses a glib::MainContext idle handler or blocking recv):
// AFTER:
if let Some(rx) = rx {
    glib::spawn_future_local(async move {
        while let Ok(msg) = rx.recv().await {
            match msg {
                TrayMessage::ShowWindow => { window.present(); }
                TrayMessage::Quit => { app.quit(); }
            }
        }
    });
}
```

**Note:** Inspect the actual `ui.rs` tray receiver implementation (lines not shown in truncated read) to confirm the exact replacement.

---

## 7. B2 — Error Handling Hardening

### 7.1 `Config::load` signature change

**Before:** `pub fn load() -> Self`  
**After:** `pub fn load() -> Result<Self>` (using `anyhow::Result`)

Implementation:
```rust
pub fn load() -> Result<Self> {
    let path = config_path();
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Self::default());
        }
        Err(e) => return Err(anyhow::Error::from(e))
            .with_context(|| format!("read {}", path.display())),
    };
    let mut cfg: Config = toml::from_str(&content)
        .with_context(|| format!("parse {}", path.display()))?;

    if !validate_interface(&cfg.interface) {
        warn!(
            "Invalid interface name {:?} in config, falling back to \"wg0\"",
            cfg.interface
        );
        cfg.interface = "wg0".to_string();
    }
    Ok(cfg)
}
```

### 7.2 Call site changes

| File | Location | Change |
|------|----------|--------|
| `src/main.rs:35` | `let cfg = config::Config::load();` | `let cfg = config::Config::load().unwrap_or_else(\|e\| { warn!("Config load failed: {}", e); Config::default() });` |
| `src/ui_prefs.rs:55` | `let cfg = Config::load();` | `let cfg = Config::load().unwrap_or_default();` |
| `src/ui_prefs.rs:72` | `let cfg = Config::load();` | `let cfg = Config::load().unwrap_or_default();` |
| `src/ui_prefs.rs:200+` | `let cfg = Config::load();` (Advanced page) | `let cfg = Config::load().unwrap_or_default();` |
| `src/helper.rs:102` | `let config = crate::config::Config::load();` | `let config = crate::config::Config::load().unwrap_or_default();` |
| `src/ui.rs:173` | `crate::config::Config::load().auto_connect` | `crate::config::Config::load().unwrap_or_default().auto_connect` |

**Note:** All GTK callback sites use `unwrap_or_default()` because they cannot propagate errors up. The `main.rs` site emits a `warn!` because it is the primary startup path.

### 7.3 `read_wg_stats` warning on malformed output

In `src/state.rs::read_wg_stats`:
```rust
// BEFORE:
let rx = parts[1].parse::<u64>().unwrap_or(0);
let tx = parts[2].parse::<u64>().unwrap_or(0);

// AFTER:
let rx = parts[1].parse::<u64>().unwrap_or_else(|_| {
    warn!("wg show transfer: malformed rx value {:?}", parts[1]);
    0
});
let tx = parts[2].parse::<u64>().unwrap_or_else(|_| {
    warn!("wg show transfer: malformed tx value {:?}", parts[2]);
    0
});
```

### 7.4 `anyhow::Context` at D-Bus call sites

In `src/dbus.rs`, all `map_err(anyhow::Error::from)` calls should be replaced with `with_context(|| "operation name")`. Example:

```rust
// BEFORE:
let unit_path = manager.load_unit(service).await
    .map_err(|e| anyhow::anyhow!("load_unit({}) failed: {}", service, e))?;

// These already have context — check the other sites:
// start_unit / stop_unit already have .map_err(|e| anyhow::anyhow!("start_unit({}) failed: ...")
// These are acceptable; no change needed for those.
```

The existing D-Bus call sites in `dbus.rs` already use `anyhow::anyhow!("operation failed: {}", e)` which is equivalent to `with_context`. **No change required here** — these are already adequately annotated.

### 7.5 Deny `clippy::unwrap_used` at crate root (optional for D)

Add to `src/main.rs` top (after reviewing all remaining `unwrap()` calls — the above covers the identified ones):
```rust
// Enable after all unwrap() calls are remediated:
// #![deny(clippy::unwrap_used)]
```

Leave commented for now; enable in Milestone E after a full audit.

---

## 8. B3 — D-Bus Proxy Caching + PropertiesChanged

### 8.1 Cache `SystemdManagerProxy`

`SYSTEM_CONN` is already cached via `OnceCell<Connection>`. Now cache the proxy:

In `src/dbus.rs`:
```rust
static SYSTEMD_MANAGER: OnceCell<SystemdManagerProxy<'static>> = OnceCell::const_new();
```

**Problem:** `SystemdManagerProxy` has a lifetime tied to the `Connection`. In zbus 3.x, proxies hold a reference to the connection. Storing a `'static` proxy in a static requires the proxy to own the connection or use an `Arc`-backed connection.

**Practical approach:** Use a `tokio::sync::OnceCell<Arc<SystemdManagerProxy<'_>>>` scoped to the task lifetime, or lazily cache per-call using a function-local static. 

**Simpler, safe approach:** Store the manager proxy in `AppState` as `Option<SystemdManagerProxy<'static>>` — but lifetime issues make this complex.

**Recommended for D:** Use a module-level `OnceLock<Arc<Mutex<SystemdManagerProxy<'_>>>>` — complex lifetime.

**Actually simplest:** Keep the `connection` cached (already done via `SYSTEM_CONN`), and create the manager proxy once per `get_service_status` call since proxy creation is cheap (just a struct wrapping the connection + path/interface strings). The real cost is `Connection::system()` which is already cached. **Skip proxy caching for D** — it adds lifetime complexity for minimal gain.

**Decision:** B3 proxy caching is **descoped** from D. The connection is already cached. Proxy construction is O(1) — one heap alloc. The perf gain is negligible.

### 8.2 `PropertiesChanged` subscription (trigger pattern)

This is the valuable part of B3: receive `ActiveState` change notifications in near-real-time instead of waiting for the 3 s poll.

Add to `src/dbus.rs`:

```rust
/// Subscribe to ActiveState property changes on the pia-vpn.service unit.
/// When a change is received, triggers a full poll_once() refresh.
/// Runs as a background tokio task.
pub async fn watch_vpn_unit_state(state: Arc<RwLock<AppState>>) {
    let conn = match system_conn().await {
        Ok(c) => c,
        Err(e) => { warn!("unit watch: D-Bus unavailable: {}", e); return; }
    };

    // Get the object path for the pia-vpn.service unit.
    let manager = match SystemdManagerProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => { warn!("unit watch: manager proxy failed: {}", e); return; }
    };

    let unit_path = match manager.load_unit("pia-vpn.service").await {
        Ok(p) => p,
        Err(e) => { warn!("unit watch: load_unit failed: {}", e); return; }
    };

    let unit = match SystemdUnitProxy::builder(&conn)
        .path(unit_path.as_ref())
        .and_then(|b| Ok(b))  // builder returns Result in zbus 3.x
    {
        // Use the full builder chain as in get_service_status():
        _ => todo!(), // see implementation note below
    };
    // ... subscribe to receive_active_state_changed()
}
```

**Implementation note:** The `SystemdUnitProxy` builder chain in `get_service_status()` (lines ~50-62 of `dbus.rs`) is the reference pattern. Duplicate it here to construct the unit proxy at the watchdog path, then:

```rust
let mut stream = unit_proxy.receive_active_state_changed().await;
while let Some(_change) = stream.next().await {
    // Don't write directly to AppState — trigger a full consistent poll.
    match crate::state::poll_once(&state).await {
        Ok(()) => debug!("PropertiesChanged triggered poll"),
        Err(e) => warn!("Triggered poll error: {}", e),
    }
}
```

**Note:** `poll_once` must be made `pub(crate)` in `state.rs`.

Spawned from `src/main.rs`:
```rust
let state_for_unit_watch = app_state.clone();
rt.spawn(async move {
    dbus::watch_vpn_unit_state(state_for_unit_watch).await;
});
```

### 8.3 Required zbus 3.x API notes (verified via Context7)

- `#[dbus_proxy(signal)]` on proxy trait method generates `receive_<signal>().await?`
- `#[dbus_proxy(property)]` generates `receive_<prop>_changed().await` (no `?`)
- `StreamExt::next()` from `futures_util` is needed to iterate signal streams
- **Add `futures-util = "0.3"` to `[dependencies]` in `Cargo.toml`**

---

## 9. Integration Tests Design

### 9.1 Directory structure

```
tests/
  config_integration.rs
  secrets_integration.rs   (extend existing unit tests)
  pia_http.rs
  state_machine.rs
  fixtures/
    serverlist_v6.json
```

### 9.2 `tests/config_integration.rs`

```rust
#[test]
fn config_round_trip_all_fields() {
    // Write a Config with every field set, serialize, deserialize, assert equal.
}

#[test]
fn config_load_missing_file_returns_default() {
    // Point config_path to a temp dir that doesn't contain the file.
    // Config::load() should return Ok(Config::default()).
}

#[test]
fn config_load_malformed_toml_returns_err() {
    // Write garbled TOML. Config::load() should return Err (not silently default).
    // This test validates the B2 fix.
}

#[test]
fn config_backward_compat_missing_auto_reconnect() {
    // Old TOML without auto_reconnect should deserialize to auto_reconnect = true (the new default).
}
```

**Note:** `config_path()` is `fn` (not `pub`) — the tests need either:
- A `#[cfg(test)]` override via env var (`XDG_CONFIG_HOME`), or
- The function made `pub(crate)` for test use.

Recommend: `Config::load_from(path: &Path) -> Result<Self>` as a new `pub(crate)` function; `load()` calls it with `config_path()`.

### 9.3 `tests/pia_http.rs`

Uses `wiremock 0.6` (dev-dependency).

```rust
use wiremock::{MockServer, Mock, ResponseTemplate};
use wiremock::matchers::{method, path};

#[tokio::test]
async fn generate_token_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/client/v2/token"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"token": "test_token_abc"}))
        )
        .mount(&server)
        .await;

    // PiaClient needs a base_url override for testing.
    // This requires adding an optional `base_url: Option<String>` to PiaClient
    // or a `PiaClient::with_base_url(url: &str)` constructor for tests.
    // See §9.5 for the PiaClient testability change.
}

#[tokio::test]
async fn generate_token_unauthorized_returns_auth_failed() { ... }

#[tokio::test]
async fn fetch_server_list_parses_regions() {
    // Mock returns fixture JSON from tests/fixtures/serverlist_v6.json
    // followed by a fake signature line.
    let fixture = include_str!("fixtures/serverlist_v6.json");
    let body = format!("{}\nZmFrZXNpZ25hdHVyZQ==");  // JSON + \n + base64

    Mock::given(method("GET"))
        .and(path("/vpninfo/servers/v6"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .mount(&server)
        .await;

    let list = client.fetch_server_list().await.unwrap();
    assert_eq!(list.regions.len(), 2);
    assert_eq!(list.regions[0].id, "us_west");
    assert!(list.regions[0].port_forward);
}
```

### 9.4 `tests/fixtures/serverlist_v6.json`

Minimal fixture matching `src/pia.rs::Region` type:

```json
{
  "regions": [
    {
      "id": "us_west",
      "name": "US West",
      "country": "US",
      "geo": false,
      "port_forward": true,
      "servers": {
        "wg": [
          {"ip": "1.2.3.4", "cn": "us-west-1.example.com"}
        ],
        "meta": [
          {"ip": "10.0.0.1", "cn": "meta-us-west.example.com"}
        ],
        "ovpntcp": [],
        "ovpnudp": []
      }
    },
    {
      "id": "de_berlin",
      "name": "DE Berlin",
      "country": "DE",
      "geo": false,
      "port_forward": false,
      "servers": {
        "wg": [
          {"ip": "5.6.7.8", "cn": "de-berlin-1.example.com"}
        ],
        "meta": [
          {"ip": "10.0.0.2", "cn": "meta-de-berlin.example.com"}
        ],
        "ovpntcp": [],
        "ovpnudp": []
      }
    }
  ]
}
```

### 9.5 PiaClient testability change (required for `pia_http.rs` tests)

Add to `src/pia.rs`:

```rust
impl PiaClient {
    /// For integration tests only — allows overriding the base URLs.
    #[cfg(test)]
    pub(crate) fn with_base_url(base_url: String) -> Result<Self, PiaError> {
        // Build clients pointing at base_url instead of PIA's production endpoints.
        // Store base_url in the struct.
    }
}
```

Add `base_url: String` field to `PiaClient` (defaulting to `"https://www.privateinternetaccess.com"` in production), and use it to construct request URLs. This is the minimal change to make the PIA HTTP tests work.

### 9.6 `tests/state_machine.rs`

```rust
#[test]
fn connection_status_stale_is_connected() {
    assert!(ConnectionStatus::Stale(200).is_connected());
    assert!(ConnectionStatus::Stale(200).is_stale());
    assert!(!ConnectionStatus::Connected.is_stale());
}

#[test]
fn connection_status_stale_label() {
    assert_eq!(ConnectionStatus::Stale(200).label(), "Reconnecting…");
}

#[test]
fn decode_port_payload_round_trip() { /* already exists */ }
```

### 9.7 `tests/secrets_integration.rs`

Extend existing unit tests (move from `#[cfg(test)]` in `src/secrets.rs` to a top-level integration test):

```rust
#[test]
fn permissions_check_warns_on_open_file() {
    // Create a credentials file with mode 0644 in a temp dir.
    // Call load_sync() and verify warning is emitted.
    // Use tracing_test or log capture to assert the warning.
}
```

---

## 10. Cargo.toml Changes

### 10.1 `[dependencies]` additions

```toml
# Async multi-producer multi-consumer channel for tray→UI (B1)
async-channel = "2"

# Stream utilities for zbus signal iteration (B3, F7)
futures-util = "0.3"
```

### 10.2 `[dev-dependencies]` additions

```toml
# HTTP mocking for PIA API integration tests
wiremock = "0.6"

# Async test runtime (tokio already in [dependencies]; re-declare for dev only
# if needed — but since tokio is already a full dep, no change needed)
```

**wiremock 0.6** is compatible with tokio 1.x (uses hyper + tokio internally). `#[tokio::test]` works without additional setup.

### 10.3 No other new dependencies

- `check_dns_leak_hint()` uses only `std::fs::read_to_string` — no new dep.
- `read_wg_handshake()` uses existing `tokio::process::Command`.
- `NetworkManagerProxy` uses existing `zbus = "3"`.
- `async-channel` replaces `std::sync::mpsc` — no net dep increase.

---

## 11. Implementation Checklist (Files to Modify/Create, In Order)

### Phase 1 — Pure mechanical changes (lowest risk, do first)

1. **`src/config.rs`**
   - Change `Config::load() -> Self` to `Config::load() -> Result<Self>`
   - Add `load_from(path: &Path) -> Result<Self>` (for test isolation)
   - Add `auto_reconnect: bool` field with `#[serde(default = "default_true")]`
   - Add `fn default_true() -> bool { true }`

2. **`src/state.rs`**
   - Add `ConnectionStatus::Stale(u64)` variant
   - Update `label()`, `is_connected()`, add `is_stale()`
   - Add `stale_cycles: u32` and `dns_leak_hint: Option<Vec<String>>` to `AppState`
   - Add `auto_reconnect: bool` to `AppState`
   - Update `AppState::new()` and `new_with_config()`
   - Add `read_wg_handshake()` async fn
   - Add `check_dns_leak_hint()` pure fn
   - Make `poll_once` `pub(crate)`
   - Integrate handshake watchdog + DNS leak into `poll_once`
   - Add `warn!` logging in `read_wg_stats` for malformed values
   - Update `poll_loop` to track stale cycles and trigger restart

3. **`src/main.rs`**
   - Replace `std::process::exit(exit_code.into())` with `let _exit_code = app.run(); Ok(())`
   - Change `Config::load()` call to `Config::load().unwrap_or_else(...)`
   - Replace `std::sync::mpsc` channel with `async_channel::bounded`
   - Remove `Arc<Mutex<Option<Receiver>>>` wrapper
   - Spawn `dbus::watch_network_manager()` task
   - Spawn `dbus::watch_vpn_unit_state()` task
   - Pass `tokio::sync::watch::Receiver<bool>` for auto_reconnect

4. **`src/tray.rs`**
   - Change `tx: std::sync::mpsc::SyncSender` to `tx: async_channel::Sender`
   - Replace `std::process::exit(0)` with `tray.tx.try_send(TrayMessage::Quit)`
   - Add `Stale` arm to `icon_name()` and `title()`

5. **`src/dbus.rs`**
   - Add `NetworkManagerProxy` with `StateChanged` signal and `State` property
   - Add `watch_network_manager()` async fn
   - Add `watch_vpn_unit_state()` async fn
   - Add `restart_vpn_unit()` async fn
   - Add `pub const NM_CONNECTED_GLOBAL: u32 = 70`
   - Add `futures_util` import: `use futures_util::stream::StreamExt`

6. **`src/ui.rs`**
   - Change `rx: Option<std::sync::mpsc::Receiver<TrayMessage>>` to `Option<async_channel::Receiver<TrayMessage>>`
   - Update tray receiver loop to use `rx.recv().await`
   - Add `TrayMessage::Quit` handling
   - Add `ConnectionStatus::Stale` CSS treatment in refresh timer
   - Add DNS leak hint display in refresh timer

7. **`src/ui_prefs.rs`**
   - Change all `Config::load()` to `Config::load().unwrap_or_default()`
   - Add `Auto-Reconnect` toggle in Advanced page

8. **`src/pia.rs`**
   - Add `base_url: String` field to `PiaClient`
   - Update URL construction to use `self.base_url`
   - Add `#[cfg(test)] pub(crate) fn with_base_url(...)` constructor

9. **`src/helper.rs`**
   - Change `Config::load()` to `Config::load().unwrap_or_default()`

### Phase 2 — New files

10. **`Cargo.toml`**
    - Add `async-channel = "2"` to `[dependencies]`
    - Add `futures-util = "0.3"` to `[dependencies]`
    - Add `wiremock = "0.6"` to `[dev-dependencies]`

11. **`tests/config_integration.rs`** (new)
12. **`tests/pia_http.rs`** (new)
13. **`tests/state_machine.rs`** (new)
14. **`tests/fixtures/serverlist_v6.json`** (new)

---

## 12. Risks & Mitigations

| Risk | Severity | Mitigation |
|------|----------|-----------|
| F7: NM not present on system | Low | `watch_network_manager` returns early on proxy failure — fully graceful. |
| F7: Reconnect loop if NM rapidly oscillates | Medium | 2 s debounce; only trigger on transition FROM disconnected TO connected. |
| F12: `wg` binary not on PATH | Low | `read_wg_handshake` returns `None`; no status change without data. |
| F12: Stale detection during legitimate reconnect | Medium | Restart window is 30 s after Stale — long enough for normal reconnects to complete. |
| B1: `app.quit()` from tray thread | Low | `TrayMessage::Quit` crosses thread boundary via `async_channel`; `app.quit()` is called on GTK main thread in `glib::spawn_future_local`. GTK-safe. |
| B1: `async_channel` Sender dropped before receiver | Low | Sender is kept alive in tray thread; receiver in UI. Both live for app lifetime. |
| B2: `Config::load() -> Result` breaks all callers | Medium | All call sites enumerated in §7.2; use `unwrap_or_else` / `unwrap_or_default` at GTK callbacks. |
| B3: `poll_once` called from two concurrent tasks | Medium | Write lock is held briefly; `poll_once` itself is idempotent. Both callers serialize on the `RwLock`. Acceptable. |
| B3: Unit object path changes across systemd restarts | Low | `load_unit` is called at watcher startup. If path changes, stream ends and watcher logs a warning. Re-subscription on failure is a Milestone E improvement. |
| Integration tests: wiremock incompatibility | Low | wiremock 0.6 explicitly targets tokio 1.x; verified by Context7 documentation. |
| `futures-util` transitive conflict | Low | `futures-util = "0.3"` is widely used; `tokio`, `zbus`, and `glib` all depend on it. A direct dep pins the minor version floor — no conflict expected. |

---

## 13. Sources Consulted

1. **zbus 3.x documentation** (Context7 `/z-galaxy/zbus`): `#[dbus_proxy(signal)]` → `receive_<signal>().await?`, `#[dbus_proxy(property)]` → `receive_<prop>_changed().await`. Signal stream requires `futures_util::StreamExt::next()`.
2. **wiremock-rs documentation** (Context7 `/lukemathwalker/wiremock-rs`): `MockServer::start().await`, `Mock::given(method("GET")).respond_with(ResponseTemplate::new(200).set_body_json(...)).mount(&server).await`. Compatible with tokio 1.x.
3. **NetworkManager D-Bus specification** (freedesktop.org): `org.freedesktop.NetworkManager` interface, `StateChanged(u state)` signal, `State` property, NM_STATE_* enum values.
4. **systemd D-Bus API** (freedesktop.org wiki): `org.freedesktop.systemd1.Unit` `ActiveState` property; `PropertiesChanged` mechanism.
5. **WireGuard `wg(8)` man page**: `wg show <iface> latest-handshakes` → `<pubkey>\t<unix_timestamp>` format.
6. **GNOME HIG — Application lifecycle**: `app.quit()` as the canonical GTK application quit mechanism; avoid `std::process::exit`.
7. **Tokio documentation**: `tokio::sync::watch` for broadcasting configuration changes to background tasks; `tokio::time::sleep` for debounce.
8. **async-channel crate** (crates.io): `async_channel::bounded(n)` → `(Sender<T>, Receiver<T>)` where both are `Clone`; `try_send()` for sync contexts.
9. **Rust `anyhow` crate**: `Context` trait for adding `.with_context(|| "message")` to `Result` chains.
10. **OWASP ASVS §2 (Authentication)**: Config files with credentials must have mode `0o600`; error messages must not leak internal state to UI.
11. **vex-vpn `docs/PROJECT_ANALYSIS.md`**: Section B1–B3, F7, F8, F12 requirements and existing code references.
12. **vex-vpn codebase** (full read): Exact `unwrap_or_default` call sites, `std::process::exit` locations, channel type, proxy construction patterns.
