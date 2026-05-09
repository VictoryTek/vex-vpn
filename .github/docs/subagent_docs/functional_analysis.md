# vex-vpn Functional Analysis

> Generated: 2026-05-08  
> Status: **App non-functional — 3 critical bugs identified**

---

## Critical Bugs

### BUG 1 — Wrong State Directory Path
**File:** `src/state.rs`  
**Severity:** Critical

The code reads runtime state from `/var/lib/private/pia-vpn`, which is the systemd `DynamicUser` bind-mount path. The NixOS module (`nix/module-vpn.nix`) does **not** set `DynamicUser = true`, so systemd creates `/var/lib/pia-vpn` instead.

Every file read — region, WireGuard peer info, port forward, traffic stats — fails silently via `.ok()` and returns `None`.

**Symptoms:**
- UI permanently shows "Select a server"
- IP, download, upload, latency, port all show "—" or zero

**Fix:**
```rust
// Before
let state_dir = "/var/lib/private/pia-vpn";

// After
let state_dir = "/var/lib/pia-vpn";
```

- [x] Fixed

---

### BUG 2 — Tray Connect/Disconnect Does Nothing
**File:** `src/tray.rs`  
**Severity:** Critical

The tray creates its own `current_thread` Tokio runtime. Menu `activate` callbacks call `rt.spawn(async { dbus::connect_vpn().await })`. A `current_thread` runtime only polls tasks while `block_on` is actively running. The only `block_on` in the tray reads an `RwLock` (completes in microseconds), so the spawned D-Bus task initiates a socket connection, is immediately suspended waiting for I/O, then is stranded — the reactor is never polled again.

**Symptoms:**
- Clicking "Connect" or "Disconnect" in the tray menu does nothing

**Fix:**  
Pass the main runtime's `Handle` to the tray and use `handle.spawn(...)` instead of creating a separate single-threaded runtime.

```rust
// In main.rs — pass handle to tray
let handle = rt.handle().clone();
tray::run_tray(state.clone(), tx.clone(), handle);

// In tray.rs — store Handle, not Runtime
pub struct PiaTray {
    state: Arc<RwLock<AppState>>,
    rt: tokio::runtime::Handle,  // was: Runtime
    ...
}

// In activate callbacks
tray.rt.spawn(async { ... });  // now schedules on the main multi-threaded runtime
```

- [x] Fixed

---

### BUG 3 — Kill Switch Broken (Missing Privilege Escalation)
**File:** `src/dbus.rs`  
**Severity:** Critical

`apply_kill_switch()` invokes `nft` directly as a subprocess. The NixOS module (`nix/module-gui.nix`) adds a `NOPASSWD` sudo rule for `nft`, but the code never uses `sudo`. The subprocess receives `EPERM` and fails; the error is logged but not surfaced to the user.

**Symptoms:**
- Kill switch toggle appears to work but firewall rules are never applied
- No user-visible error

**Fix:**
```rust
// Before
let mut child = tokio::process::Command::new("nft")
    .arg("-f")
    .arg("-")
    ...

// After
let mut child = tokio::process::Command::new("sudo")
    .arg("nft")
    .arg("-f")
    .arg("-")
    ...
```

- [x] Fixed

---

## High Severity Issues

### BUG 4 — Kill Switch & Port Forward Switches Never Sync from State
**File:** `src/ui.rs`  
**Severity:** High

The `LiveWidgets` struct does not include the `gtk4::Switch` widgets for Kill Switch or Port Forwarding. `refresh_widgets()` reads `s.kill_switch_enabled` and `s.port_forward_enabled` from the polled `AppState` but has nowhere to push those values. Both toggles are hardcoded to `false` on construction.

**Symptoms:**
- If kill switch or port forwarding is active, the UI always shows both as OFF
- Manual toggles are lost on window close/reopen

**Fix:**  
Add both `gtk4::Switch` references to `LiveWidgets`. Update `refresh_widgets()` to call:
```rust
live.kill_switch_sw.set_active(s.kill_switch_enabled);
live.port_forward_sw.set_active(s.port_forward_enabled);
```

- [x] Fixed

---

## Medium Severity Issues

### BUG 5 — `GetUnit` Fails for Unloaded Units
**File:** `src/dbus.rs`  
**Severity:** Medium

`manager.get_unit(service)` raises `org.freedesktop.systemd1.NoSuchUnit` if the unit has never been loaded into systemd's memory (e.g., fresh install, dev environment). `LoadUnit` is the correct method — it loads the unit file if needed before returning the object path.

**Fix:** Replace `get_unit` with `load_unit` and add the corresponding proxy method declaration.

- [x] Fixed

---

### BUG 6 — New D-Bus Connection Created on Every Poll
**File:** `src/dbus.rs`  
**Severity:** Medium

`system_conn()` opens a fresh `Connection` (Unix socket + auth handshake) on every call. The poll loop calls this at least twice per 3-second cycle — 40+ connections per minute.

**Fix:** Store a single `Arc<Connection>` in a `once_cell::sync::OnceCell` or `tokio::sync::OnceCell` and reuse it.

- [x] Fixed

---

### BUG 7 — Latency Timeout Exceeds Poll Interval
**File:** `src/state.rs`  
**Severity:** Medium

The latency TCP probe uses a 5-second timeout, but the poll loop sleeps for 3 seconds. When the PIA meta server is unreachable, one poll cycle blocks for 5+ seconds, making the effective interval 8+ seconds.

**Fix:** Reduce latency timeout to ≤2 seconds, or run the probe concurrently with other poll operations via `tokio::join!`.

- [x] Fixed

---

## Low Severity Issues

### BUG 8 — No User Feedback on Connect/Disconnect Failure
**File:** `src/ui.rs`  
**Severity:** Low

When `connect_vpn()` or `disconnect_vpn()` fails, the error is only logged via `tracing::error!`. The UI stays stuck at "CONNECTING…" until the next 3-second poll tick. No toast or status update is shown.

**Fix:** Show an `adw::Toast` or set the status pill to "ERROR" immediately on failure.

- [x] Fixed

---

### BUG 9 — Servers and Settings Nav Buttons Are Non-Functional
**File:** `src/ui.rs`  
**Severity:** Low

The sidebar builds three nav buttons (Dashboard, Servers, Settings), but only Dashboard content exists. The Servers and Settings buttons have no `connect_clicked` handler. The README describes a Settings page (DNS, interface name, max latency) that was never implemented.

**Fix:** Either implement the pages or hide the buttons until they are ready.

- [x] Fixed

---

### BUG 10 — `src/app.rs` Is a Dead Stub
**File:** `src/app.rs`  
**Severity:** Low

The entire module is:
```rust
// Reserved for future signal bus implementation.
#[allow(dead_code)]
pub struct App;
```

`App` is never constructed or used. No cross-component signal bus was implemented; the app relies entirely on the 3-second timer poll for coordination.

**Fix:** Either implement the signal bus or remove the module.

- [x] Fixed

---

## Summary

| # | Severity | File | Issue | Fixed |
|---|----------|------|-------|-------|
| 1 | Critical | `src/state.rs` | Wrong state dir path (`/var/lib/private/` vs `/var/lib/`) | [x] |
| 2 | Critical | `src/tray.rs` | Tray uses dead single-threaded runtime; D-Bus calls stranded | [x] |
| 3 | Critical | `src/dbus.rs` | `nft` called without `sudo`; kill switch always fails | [x] |
| 4 | High | `src/ui.rs` | Kill switch / port forward switches not synced from state | [x] |
| 5 | Medium | `src/dbus.rs` | `GetUnit` fails for unloaded units; should use `LoadUnit` | [x] |
| 6 | Medium | `src/dbus.rs` | New D-Bus connection per poll (40+/min); should be shared | [x] |
| 7 | Medium | `src/state.rs` | 5s latency timeout > 3s poll interval; causes UI lag | [x] |
| 8 | Low | `src/ui.rs` | No user feedback on connect/disconnect failure | [x] |
| 9 | Low | `src/ui.rs` | Servers/Settings nav buttons have no handlers | [x] |
| 10 | Low | `src/app.rs` | `App` struct is an unused stub | [x] |
