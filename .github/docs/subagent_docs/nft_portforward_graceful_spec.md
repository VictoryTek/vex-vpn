# Graceful Degradation: nft Unavailable & Port Forward Unit Missing

**Feature name:** `nft_portforward_graceful`  
**Date:** 2026-05-10  
**Phase:** 1 — Research & Specification

---

## 1. Problem Statement

When running `nix run github:victorytek/vex-vpn --refresh`, two ERROR-level failures occur on first launch:

```
ERROR vex_vpn::ui: kill switch toggle: helper error: spawn nft: No such file or directory (os error 2)
ERROR vex_vpn::ui: port forward toggle: start_unit(pia-vpn-portforward.service) failed:
  org.freedesktop.systemd1.NoSuchUnit: Unit pia-vpn-portforward.service not found.
```

Both happen because the user clicks a toggle, the async operation fails, the toggle is NOT reverted, and the error message shown in the toast is a raw technical string rather than a user-friendly explanation. Additionally, both operations are logged at ERROR level when they represent a "feature not available in this environment" situation, not a programming error.

---

## 2. Current State Analysis

### 2.1 Kill Switch Flow

**`src/bin/helper.rs` — `nft_binary()` / `run_nft_enable()` / `run_nft_disable()`**

The polkit-gated helper binary (runs as root via `pkexec`) calls `nft` via `std::process::Command`. It probes two hardcoded paths:

```rust
fn nft_binary() -> &'static str {
    if std::path::Path::new("/run/current-system/sw/bin/nft").exists() {
        "/run/current-system/sw/bin/nft"   // NixOS system profile
    } else {
        "/usr/sbin/nft"                    // FHS fallback
    }
}
```

Under `nix run`, neither path exists and `nft` is not on PATH. `std::process::Command::new("/usr/sbin/nft")` fails with `ENOENT`, producing:

```json
{"ok": false, "error": "spawn nft: No such file or directory (os error 2)"}
```

**`src/helper.rs` — `apply_kill_switch()`**

Spawns `pkexec vex-vpn-helper` asynchronously, writes a JSON command, reads the JSON response. On `ok: false`, returns:

```rust
bail!("helper error: {}", resp.error.unwrap_or_default())
// → Err("helper error: spawn nft: No such file or directory (os error 2)")
```

**`src/ui.rs` — kill switch toggle handler (lines ~783–810)**

```rust
move |active| {
    if guard.get() { return; }
    let state = state_c.clone();
    let toasts = toasts_ks.clone();
    glib::spawn_future_local(async move {
        let iface = state.read().await.interface.clone();
        let res = if active {
            crate::helper::apply_kill_switch(&iface).await
        } else {
            crate::helper::remove_kill_switch().await
        };
        match res {
            Ok(()) => { state.write().await.kill_switch_enabled = active; }
            Err(e) => {
                tracing::error!("kill switch toggle: {}", e);  // ← ERROR level
                toasts.add_toast(adw::Toast::new(&format!("Kill switch error: {e:#}")));
                // ← toggle NOT reverted; state NOT updated; visual mismatch persists
            }
        }
    });
},
```

**Key deficiencies:**
- Toggle is left in the user-chosen position even after the operation fails (visual lie).
- The toast message exposes internal detail: `"Kill switch error: helper error: spawn nft: No such file or directory (os error 2)"`.
- Logged at ERROR, which implies a bug, not a capability absence.
- No startup check to disable the toggle when `nft` is unavailable.

---

### 2.2 Port Forward Flow

**`src/dbus.rs` — `enable_port_forward()` / `is_service_unit_installed()`**

```rust
pub async fn enable_port_forward() -> Result<()> {
    start_unit("pia-vpn-portforward.service").await
}

// Already exists but is NOT called for port forward:
pub async fn is_service_unit_installed(service: &str) -> bool { ... }
```

`is_service_unit_installed` exists and is already used at startup for `pia-vpn.service`. It is **not** used for `pia-vpn-portforward.service`.

**`src/ui.rs` — port forward toggle handler (lines ~814–845)**

```rust
move |active| {
    if guard.get() { return; }
    let toasts = toasts_pf.clone();
    glib::spawn_future_local(async move {
        let res = if active {
            crate::dbus::enable_port_forward().await
        } else {
            crate::dbus::disable_port_forward().await
        };
        if let Err(e) = res {
            tracing::error!("port forward toggle: {}", e);  // ← ERROR level
            toasts.add_toast(adw::Toast::new(&format!("Port forwarding error: {e:#}")));
            // ← toggle NOT reverted; visual mismatch persists
        }
    });
},
```

**Key deficiencies:**
- Toggle not reverted on failure.
- Toast is raw D-Bus error: `"Port forwarding error: start_unit(pia-vpn-portforward.service) failed: org.freedesktop.systemd1.NoSuchUnit: ..."`.
- No startup check to disable the toggle when the unit file is absent.
- `pia-vpn-portforward.service` only exists when the NixOS module (`module.nix`) is activated; it never exists under `nix run`.

---

### 2.3 What the Startup Flow Already Does (Reference)

In `build_ui`, after `live` (the `LiveWidgets` struct) is constructed but **before** it is moved into the `glib::timeout_add_seconds_local` closure, a startup check already exists:

```rust
let startup_connect_btn = live.connect_btn.clone();   // clone before move

// 3-second poll — moves 'live' into closure
glib::timeout_add_seconds_local(3, move || { ... });

// Startup check (already present)
{
    let connect_btn_ref = startup_connect_btn;
    glib::spawn_future_local(async move {
        if !crate::dbus::is_service_unit_installed("pia-vpn.service").await {
            connect_btn_ref.set_sensitive(false);
            show_service_install_dialog(...).await;
        }
    });
}
```

`kill_switch_sw` and `port_forward_sw` are available as `live.kill_switch_sw` and `live.port_forward_sw` inside `LiveWidgets`. They can be cloned before `live` is moved, exactly like `startup_connect_btn`.

The `refresh_widgets` function (called every 3 seconds) calls `set_active` on both switches but does **not** call `set_sensitive`. So startup-set sensitivity is preserved across polls.

---

## 3. Proposed Fix

### 3.1 Strategy

Apply **two layers of defense** for each feature:

| Layer | Kill Switch | Port Forward |
|-------|-------------|--------------|
| Startup disable | Check nft paths; disable switch + tooltip if absent | Check unit installed; disable switch + tooltip if absent |
| Toggle revert | Revert `sw.set_active(!active)` on any `Err` | Revert `sw.set_active(!active)` on any `Err` |
| Toast message | User-friendly; distinguish nft-absent from other errors | User-friendly; distinguish NoSuchUnit from other errors |
| Log level | WARN (not ERROR) | WARN (not ERROR) |

---

### 3.2 New Utility: `nft_available()` in `src/helper.rs`

Add a public, synchronous function that checks whether the `nft` binary is reachable. It mirrors the same probe logic used in `nft_binary()` in the helper binary, plus a PATH scan.

```rust
/// Returns `true` if the `nft` (nftables) binary is present at any known path.
/// Synchronous — does not spawn a process.
pub fn nft_available() -> bool {
    if std::path::Path::new("/run/current-system/sw/bin/nft").exists() {
        return true;
    }
    if std::path::Path::new("/usr/sbin/nft").exists() {
        return true;
    }
    // Scan PATH for 'nft'
    if let Ok(path_env) = std::env::var("PATH") {
        for dir in path_env.split(':') {
            if std::path::Path::new(dir).join("nft").exists() {
                return true;
            }
        }
    }
    false
}
```

**Location:** `src/helper.rs`, exported as `pub fn nft_available() -> bool`.  
**No new dependencies.** Pure `std`.

---

### 3.3 Toggle Revert Pattern

The toggle signal callback currently does not have a reference to its own `gtk4::Switch`. The revert needs that reference.

**Pattern to use:** `Rc<std::cell::OnceCell<gtk4::Switch>>`

Since the signal fires only after user interaction (always after the setup closure runs), the `OnceCell` is guaranteed to be populated before any toggle:

```rust
let ks_sw_cell: Rc<std::cell::OnceCell<gtk4::Switch>> = Rc::new(std::cell::OnceCell::new());
let ks_sw_cell_cap = ks_sw_cell.clone();     // captured into the toggle closure
let guard_cap = kill_switch_updating.clone(); // captured into the toggle closure

let (row, sw) = make_toggle_row(
    ...,
    move |active| {
        if guard.get() { return; }
        let sw_ref = ks_sw_cell_cap.get().expect("switch set before first toggle").clone();
        let guard_c = guard_cap.clone();
        glib::spawn_future_local(async move {
            // ... perform operation ...
            Err(e) => {
                // Revert toggle
                guard_c.set(true);
                sw_ref.set_active(!active);
                guard_c.set(false);
                // show toast ...
            }
        });
    },
);
// Populate the cell immediately after make_toggle_row returns
ks_sw_cell.set(sw.clone()).ok();
```

The same pattern applies to the port forward switch (`pf_sw_cell`).

---

### 3.4 Changes to `src/ui.rs`

#### 3.4.1 Startup checks (in `build_ui`)

Add two clones **before** `live` is moved into the timeout closure:

```rust
let startup_ks_sw = live.kill_switch_sw.clone();
let startup_pf_sw = live.port_forward_sw.clone();
let startup_connect_btn = live.connect_btn.clone();

// (existing) glib::timeout_add_seconds_local moves `live`
glib::timeout_add_seconds_local(3, move || { ... });
window.present();

// NEW: Kill switch availability check
{
    let ks_sw = startup_ks_sw;
    glib::spawn_future_local(async move {
        if !crate::helper::nft_available() {
            ks_sw.set_sensitive(false);
            ks_sw.set_tooltip_text(Some("Kill switch requires nftables — not available in this environment"));
        }
    });
}

// NEW: Port forward unit check
{
    let pf_sw = startup_pf_sw;
    glib::spawn_future_local(async move {
        if !crate::dbus::is_service_unit_installed("pia-vpn-portforward.service").await {
            pf_sw.set_sensitive(false);
            pf_sw.set_tooltip_text(Some("Port forwarding requires the NixOS VPN module"));
        }
    });
}

// (existing) pia-vpn.service check
{
    let connect_btn_ref = startup_connect_btn;
    glib::spawn_future_local(async move { ... });
}
```

Note: `nft_available()` is synchronous so it can run inside `glib::spawn_future_local` without blocking the GTK main thread (it only does path stat calls, no process spawning). It is wrapped in `spawn_future_local` purely for consistency with the other startup checks; it could alternatively be called directly before `build_main_page`.

#### 3.4.2 Kill switch toggle handler (in `build_main_page`)

**Replace** the current kill switch block with the version that:
1. Uses `Rc<OnceCell<gtk4::Switch>>` to capture a reference to the switch for revert.
2. Changes log level to `tracing::warn!`.
3. Shows a user-friendly toast message, distinguishing nft-absent from other errors.
4. Reverts the toggle on any failure.
5. Does **not** update `state.kill_switch_enabled` on failure.

```rust
let kill_switch_updating = std::rc::Rc::new(std::cell::Cell::new(false));
let kill_switch_sw = {
    let state_c = state.clone();
    let guard = kill_switch_updating.clone();
    let toasts_ks = toasts.clone();

    let ks_cell: std::rc::Rc<std::cell::OnceCell<gtk4::Switch>> =
        std::rc::Rc::new(std::cell::OnceCell::new());
    let ks_cell_cap = ks_cell.clone();
    let guard_cap = kill_switch_updating.clone();

    let (row, sw) = make_toggle_row(
        "network-vpn-symbolic",
        "Kill Switch",
        "Block all traffic if VPN drops",
        initial_kill_switch,
        move |active| {
            if guard.get() {
                return;
            }
            let state = state_c.clone();
            let toasts = toasts_ks.clone();
            let sw_ref = ks_cell_cap
                .get()
                .expect("kill_switch_sw populated before first toggle")
                .clone();
            let guard_c = guard_cap.clone();
            glib::spawn_future_local(async move {
                let iface = state.read().await.interface.clone();
                let res = if active {
                    crate::helper::apply_kill_switch(&iface).await
                } else {
                    crate::helper::remove_kill_switch().await
                };
                match res {
                    Ok(()) => {
                        state.write().await.kill_switch_enabled = active;
                    }
                    Err(e) => {
                        tracing::warn!("kill switch toggle: {}", e);
                        // Revert toggle
                        guard_c.set(true);
                        sw_ref.set_active(!active);
                        guard_c.set(false);
                        // User-friendly message
                        let msg = if e.to_string().contains("spawn nft")
                            || e.to_string().contains("No such file")
                        {
                            "Kill switch requires nftables — not available in this environment"
                                .to_string()
                        } else {
                            format!("Kill switch error: {e:#}")
                        };
                        toasts.add_toast(adw::Toast::new(&msg));
                    }
                }
            });
        },
    );
    ks_cell.set(sw.clone()).ok();
    feats.append(&row);
    sw
};
```

#### 3.4.3 Port forward toggle handler (in `build_main_page`)

**Replace** the current port forward block with the version that:
1. Uses `Rc<OnceCell<gtk4::Switch>>` for revert.
2. Changes log level to `tracing::warn!`.
3. Distinguishes `NoSuchUnit` errors with a user-friendly message.
4. Reverts the toggle on any failure.

```rust
let port_forward_updating = std::rc::Rc::new(std::cell::Cell::new(false));
let port_forward_sw = {
    let guard = port_forward_updating.clone();
    let toasts_pf = toasts.clone();

    let pf_cell: std::rc::Rc<std::cell::OnceCell<gtk4::Switch>> =
        std::rc::Rc::new(std::cell::OnceCell::new());
    let pf_cell_cap = pf_cell.clone();
    let guard_cap = port_forward_updating.clone();

    let (row, sw) = make_toggle_row(
        "network-transmit-receive-symbolic",
        "Port Forwarding",
        "Allow inbound connections through VPN",
        false,
        move |active| {
            if guard.get() {
                return;
            }
            let toasts = toasts_pf.clone();
            let sw_ref = pf_cell_cap
                .get()
                .expect("port_forward_sw populated before first toggle")
                .clone();
            let guard_c = guard_cap.clone();
            glib::spawn_future_local(async move {
                let res = if active {
                    crate::dbus::enable_port_forward().await
                } else {
                    crate::dbus::disable_port_forward().await
                };
                if let Err(e) = res {
                    tracing::warn!("port forward toggle: {}", e);
                    // Revert toggle
                    guard_c.set(true);
                    sw_ref.set_active(!active);
                    guard_c.set(false);
                    // User-friendly message
                    let msg =
                        if e.to_string().contains("NoSuchUnit")
                            || e.to_string().contains("not found")
                        {
                            "Port forwarding requires the NixOS VPN module".to_string()
                        } else {
                            format!("Port forwarding error: {e:#}")
                        };
                    toasts.add_toast(adw::Toast::new(&msg));
                }
            });
        },
    );
    pf_cell.set(sw.clone()).ok();
    feats.append(&row);
    sw
};
```

---

## 4. Files to Modify

| File | Change |
|------|--------|
| `src/helper.rs` | Add `pub fn nft_available() -> bool` |
| `src/ui.rs` | Kill switch toggle handler: OnceCell revert + WARN log + friendly toast |
| `src/ui.rs` | Port forward toggle handler: OnceCell revert + WARN log + friendly toast |
| `src/ui.rs` | `build_ui`: startup checks to disable insensitive toggles + tooltips |

No changes to `src/dbus.rs`, `src/bin/helper.rs`, `src/config.rs`, `src/state.rs`, `Cargo.toml`, or `flake.nix`.

---

## 5. Imports Required

`src/ui.rs` already imports `std::rc::Rc`. Add `std::cell::OnceCell` — this is in `std` (stable since Rust 1.70). No new Cargo dependencies.

---

## 6. Implementation Steps

1. **`src/helper.rs`**: Add `nft_available()` function after `helper_path()` (around line 50).
2. **`src/ui.rs`, `build_main_page`**: Replace kill switch block (~lines 772–813) with the `OnceCell`-based version.
3. **`src/ui.rs`, `build_main_page`**: Replace port forward block (~lines 813–848) with the `OnceCell`-based version.
4. **`src/ui.rs`, `build_ui`**: Before `glib::timeout_add_seconds_local` is called (around line 261), clone `kill_switch_sw` and `port_forward_sw` into `startup_ks_sw` and `startup_pf_sw`. Add two `glib::spawn_future_local` blocks after `window.present()` (before or after the existing service install check — order does not matter).
5. Run `nix develop --command cargo clippy -- -D warnings` to verify zero warnings.

---

## 7. Risks and Mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `OnceCell::get()` panics if switch never populated | Very low | The cell is populated immediately after `make_toggle_row` returns, in the same synchronous GTK main thread call. A toggle signal can only fire after the GTK event loop starts, which is always after setup. |
| `refresh_widgets` re-enables a disabled switch via `set_active` | No | `refresh_widgets` calls `set_active` (state sync) but never `set_sensitive`. Startup-set sensitivity persists. |
| nft becomes available at runtime after startup check disables toggle | Acceptable | This is the same trade-off made for `pia-vpn.service`. The user must restart the app. This is noted as a known limitation. |
| String matching on error text for toast messages is fragile | Low | The error strings are produced by our own code (`"spawn nft: ..."` from `bin/helper.rs`, `"NoSuchUnit"` from D-Bus). They are stable internal strings, not third-party messages. If they change, the fallback toast (`"Kill switch error: {e:#}"`) is still shown — it just won't be as friendly. |
| Startup nft check runs on the Tokio thread inside `glib::spawn_future_local` | No issue | `nft_available()` is pure synchronous `Path::exists()` calls. No blocking I/O concern on the GTK thread. |

---

## 8. Out of Scope

- Detecting nft availability changes at runtime (after initial startup check).
- Showing a persistent banner instead of a toast.
- Auto-installing nftables or the NixOS module from within the app.
- Changes to `pia-vpn-portforward.service` unit template or the `module.nix`.
