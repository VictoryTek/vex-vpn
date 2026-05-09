# BUG 4 Specification — Kill Switch & Port Forward Switch Sync

**Feature name:** `bug4_switch_sync`  
**Severity:** High  
**File:** `src/ui.rs`  
**Date:** 2026-05-08

---

## 1. Current State Analysis

### 1.1 `LiveWidgets` Struct (ui.rs lines 115–126)

```rust
struct LiveWidgets {
    status_pill: gtk4::Label,
    connect_btn: gtk4::Button,
    btn_icon: gtk4::Image,
    btn_label: gtk4::Label,
    location_label: gtk4::Label,
    ip_label: gtk4::Label,
    dl_value: gtk4::Label,
    ul_value: gtk4::Label,
    lat_value: gtk4::Label,
    port_value: gtk4::Label,
}
```

**Observation:** No `gtk4::Switch` fields exist. Kill switch and port forward switches are NOT reachable from outside `build_main_page`.

---

### 1.2 `make_toggle_row` Function (ui.rs lines 598–619)

```rust
fn make_toggle_row(
    icon: &str,
    title: &str,
    subtitle: &str,
    default: bool,
    on_toggle: impl Fn(bool) + 'static,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.set_subtitle(subtitle);

    let img = gtk4::Image::from_icon_name(icon);
    img.set_pixel_size(16);
    row.add_prefix(&img);

    let sw = gtk4::Switch::new();
    sw.set_active(default);
    sw.set_valign(gtk4::Align::Center);
    sw.connect_active_notify(move |s| on_toggle(s.is_active()));
    row.add_suffix(&sw);
    row.set_activatable_widget(Some(&sw));

    row
}
```

**Observation:**
- The `gtk4::Switch` (`sw`) is created locally and embedded in the returned `adw::ActionRow`.
- The `Switch` is **not returned** to the caller — it is unreachable once the function returns.
- The signal handler is `connect_active_notify`, which fires on **any** change to the `active` property, including programmatic calls to `set_active()`.

---

### 1.3 Kill Switch and Port Forward Toggle Creation (ui.rs lines 406–455)

**Kill switch (lines 406–435):**
```rust
{
    let state_c = state.clone();
    feats.append(&make_toggle_row(
        "network-vpn-symbolic",
        "Kill Switch",
        "Block all traffic if VPN drops",
        false,   // ← hardcoded; never reflects AppState
        move |active| {
            let state = state_c.clone();
            glib::spawn_future_local(async move {
                let iface = state.read().await.interface.clone();
                let res = if active {
                    crate::dbus::apply_kill_switch(&iface).await
                } else {
                    crate::dbus::remove_kill_switch().await
                };
                if let Err(e) = res {
                    tracing::error!("kill switch toggle: {}", e);
                }
            });
        },
    ));
}
```

**Port forward (lines 436–454):**
```rust
{
    feats.append(&make_toggle_row(
        "network-transmit-receive-symbolic",
        "Port Forwarding",
        "Allow inbound connections through VPN",
        false,   // ← hardcoded; never reflects AppState
        move |active| {
            glib::spawn_future_local(async move {
                let res = if active {
                    crate::dbus::enable_port_forward().await
                } else {
                    crate::dbus::disable_port_forward().await
                };
                if let Err(e) = res {
                    tracing::error!("port forward toggle: {}", e);
                }
            });
        },
    ));
}
```

**Observation:** Both toggles pass `false` as `default` and the `Switch` references are immediately discarded.

---

### 1.4 `refresh_widgets` Function (ui.rs lines 502–570)

```rust
fn refresh_widgets(live: &LiveWidgets, s: &AppState) {
    // ... status pill, connect button labels/icons ...

    // Location / IP
    if let Some(region) = &s.region {
        live.location_label.set_label(&region.name);
    } else {
        live.location_label.set_label(if s.status.is_connected() {
            "Connected"
        } else {
            "Select a server"
        });
    }

    if let Some(conn) = &s.connection {
        live.ip_label.set_label(&conn.peer_ip);
        live.dl_value.set_label(&format_bytes(conn.rx_bytes));
        live.ul_value.set_label(&format_bytes(conn.tx_bytes));
    } else {
        live.ip_label.set_label("—");
        live.dl_value.set_label("0 B");
        live.ul_value.set_label("0 B");
    }

    // Latency
    match s.latency_ms {
        Some(ms) => live.lat_value.set_label(&format!("{} ms", ms)),
        None => live.lat_value.set_label("— ms"),
    }

    // Port forwarding
    match s.forwarded_port {
        Some(port) => live.port_value.set_label(&port.to_string()),
        None => live.port_value.set_label("—"),
    }
    // ← NO kill_switch or port_forward set_active calls here
}
```

**Observation:** `s.kill_switch_enabled` and `s.port_forward_enabled` are read by `poll_once` and stored in `AppState` but are never consumed by `refresh_widgets`.

---

### 1.5 `LiveWidgets` Manual Clone in the Timer Callback (ui.rs lines 184–196)

```rust
glib::timeout_add_seconds_local(3, move || {
    let state = state.clone();
    let live = LiveWidgets {
        status_pill: live.status_pill.clone(),
        connect_btn: live.connect_btn.clone(),
        btn_icon: live.btn_icon.clone(),
        btn_label: live.btn_label.clone(),
        location_label: live.location_label.clone(),
        ip_label: live.ip_label.clone(),
        dl_value: live.dl_value.clone(),
        ul_value: live.ul_value.clone(),
        lat_value: live.lat_value.clone(),
        port_value: live.port_value.clone(),
    };
    glib::spawn_future_local(async move {
        let s = state.read().await.clone();
        refresh_widgets(&live, &s);
    });
    glib::ControlFlow::Continue
});
```

**Observation:** Every new field added to `LiveWidgets` must be cloned here explicitly (`LiveWidgets` does not derive `Clone`).

---

### 1.6 `AppState` Fields in Question (state.rs lines 65–67)

```rust
pub struct AppState {
    // ...
    pub kill_switch_enabled: bool,
    pub port_forward_enabled: bool,
    // ...
}
```

These are updated every poll cycle by `poll_once` (state.rs):
```rust
s.kill_switch_enabled = kill_switch_active;  // from check_kill_switch()
s.port_forward_enabled = pf_active;          // from pia-vpn-portforward.service status
```

**Observation:** Neither field is persisted to `config.toml`. The `Config` struct contains only `auto_connect`, `interface`, `max_latency_ms`, and `dns_provider`. The fix does NOT interact with config loading.

---

## 2. Problem Definition

When the VPN daemon activates the kill switch or enables port forwarding (e.g., on system boot with auto-connect, or by an external systemd command), `poll_once` updates `AppState.kill_switch_enabled` and `AppState.port_forward_enabled` to `true`. However, `refresh_widgets` has no reference to either `gtk4::Switch` widget and therefore cannot reflect these values in the UI.

**Consequences:**
1. **Kill Switch and Port Forward switches always appear OFF** — even when active.
2. **Manual toggles are lost on window close/reopen** — because the window is rebuilt from scratch and `default = false` is hardcoded.
3. **State mismatch** — the user may re-toggle an already-active feature, causing an unexpected D-Bus action (disable then re-enable).

---

## 3. Signal Feedback Loop Risk Assessment

### 3.1 Root Cause

`make_toggle_row` connects the signal with `connect_active_notify`:

```rust
sw.connect_active_notify(move |s| on_toggle(s.is_active()));
```

`connect_active_notify` fires on **every** change to the `active` property — including programmatic calls made by `refresh_widgets`. This means:

1. Poll completes → `AppState.kill_switch_enabled = true`
2. Timer fires → `refresh_widgets` calls `kill_switch_sw.set_active(true)`
3. `connect_active_notify` fires → `on_toggle(true)` runs
4. `on_toggle(true)` calls `crate::dbus::apply_kill_switch()` — a D-Bus call already in the desired state
5. On the next poll, the cycle repeats

This is an **infinite signal-driven feedback loop** that causes repeated unnecessary D-Bus round-trips every 3 seconds.

### 3.2 Mitigation Strategy — Reentrancy Guard (`Rc<Cell<bool>>`)

Because `refresh_widgets` and all signal handlers execute exclusively on the **GTK main thread**, `Rc<Cell<bool>>` is safe to use (no cross-thread sharing needed; `Rc` is deliberately `!Send`).

The pattern:

```rust
// Guard created once in build_main_page:
let ks_guard = Rc::new(Cell::new(false));

// Passed into the signal handler (via closure clone):
{
    let guard = ks_guard.clone();
    sw.connect_active_notify(move |s| {
        if guard.get() { return; }  // ← skip when refresh is driving the change
        on_toggle(s.is_active());
    });
}

// Stored in LiveWidgets so refresh_widgets can set it:
struct LiveWidgets {
    // ...
    kill_switch_sw: gtk4::Switch,
    kill_switch_guard: Rc<Cell<bool>>,
    // ...
}

// In refresh_widgets:
live.kill_switch_guard.set(true);
live.kill_switch_sw.set_active(s.kill_switch_enabled);
live.kill_switch_guard.set(false);
```

This is identical to the "reentrancy guard" pattern used in GTK application development to prevent programmatic widget state changes from triggering user-intent signal handlers.

**Auto-connect** does not need a guard because its `on_toggle` only writes to `config.toml` and does not modify `AppState` or fire D-Bus actions that the poll loop would then pick up in a feedback-creating way.

---

## 4. Proposed Solution — Exact Changes

### 4.1 Change 1: Add `use std::{rc::Rc, cell::Cell};` import

At the top of `src/ui.rs`, alongside existing `use` statements:

```rust
use std::rc::Rc;
use std::cell::Cell;
```

---

### 4.2 Change 2: Modify `make_toggle_row` to return `(adw::ActionRow, gtk4::Switch)`

**Old signature and body:**
```rust
fn make_toggle_row(
    icon: &str,
    title: &str,
    subtitle: &str,
    default: bool,
    on_toggle: impl Fn(bool) + 'static,
) -> adw::ActionRow {
    // ...
    sw.connect_active_notify(move |s| on_toggle(s.is_active()));
    row.add_suffix(&sw);
    row.set_activatable_widget(Some(&sw));
    row
}
```

**New signature and body:**

```rust
fn make_toggle_row(
    icon: &str,
    title: &str,
    subtitle: &str,
    default: bool,
    guard: Option<Rc<Cell<bool>>>,
    on_toggle: impl Fn(bool) + 'static,
) -> (adw::ActionRow, gtk4::Switch) {
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.set_subtitle(subtitle);

    let img = gtk4::Image::from_icon_name(icon);
    img.set_pixel_size(16);
    row.add_prefix(&img);

    let sw = gtk4::Switch::new();
    sw.set_active(default);
    sw.set_valign(gtk4::Align::Center);
    sw.connect_active_notify(move |s| {
        if guard.as_ref().map(|g| g.get()).unwrap_or(false) {
            return;
        }
        on_toggle(s.is_active());
    });
    row.add_suffix(&sw);
    row.set_activatable_widget(Some(&sw));

    (row, sw)
}
```

**Key points:**
- `guard: Option<Rc<Cell<bool>>>` — `None` for auto-connect (no feedback risk), `Some(guard)` for kill switch and port forward.
- Returns `(adw::ActionRow, gtk4::Switch)` — caller captures the switch reference.

---

### 4.3 Change 3: Add fields to `LiveWidgets`

```rust
struct LiveWidgets {
    status_pill: gtk4::Label,
    connect_btn: gtk4::Button,
    btn_icon: gtk4::Image,
    btn_label: gtk4::Label,
    location_label: gtk4::Label,
    ip_label: gtk4::Label,
    dl_value: gtk4::Label,
    ul_value: gtk4::Label,
    lat_value: gtk4::Label,
    port_value: gtk4::Label,
    // NEW:
    kill_switch_sw: gtk4::Switch,
    kill_switch_guard: Rc<Cell<bool>>,
    port_forward_sw: gtk4::Switch,
    port_forward_guard: Rc<Cell<bool>>,
}
```

---

### 4.4 Change 4: Update the kill switch and port forward block in `build_main_page`

**Kill switch — replace existing block:**

```rust
// Kill switch
let ks_guard = Rc::new(Cell::new(false));
let (ks_row, kill_switch_sw) = {
    let state_c = state.clone();
    let guard = ks_guard.clone();
    make_toggle_row(
        "network-vpn-symbolic",
        "Kill Switch",
        "Block all traffic if VPN drops",
        false,
        Some(guard),
        move |active| {
            let state = state_c.clone();
            glib::spawn_future_local(async move {
                let iface = state.read().await.interface.clone();
                let res = if active {
                    crate::dbus::apply_kill_switch(&iface).await
                } else {
                    crate::dbus::remove_kill_switch().await
                };
                if let Err(e) = res {
                    tracing::error!("kill switch toggle: {}", e);
                }
            });
        },
    )
};
feats.append(&ks_row);
```

**Port forwarding — replace existing block:**

```rust
// Port forwarding
let pf_guard = Rc::new(Cell::new(false));
let (pf_row, port_forward_sw) = make_toggle_row(
    "network-transmit-receive-symbolic",
    "Port Forwarding",
    "Allow inbound connections through VPN",
    false,
    Some(pf_guard.clone()),
    move |active| {
        glib::spawn_future_local(async move {
            let res = if active {
                crate::dbus::enable_port_forward().await
            } else {
                crate::dbus::disable_port_forward().await
            };
            if let Err(e) = res {
                tracing::error!("port forward toggle: {}", e);
            }
        });
    },
);
feats.append(&pf_row);
```

**Auto connect — update call to pass `None` guard and destructure:**

```rust
// Auto connect — persisted via config.toml
let (ac_row, _) = make_toggle_row(
    "system-run-symbolic",
    "Auto Connect",
    "Connect on graphical login",
    initial_auto_connect,
    None,
    move |active| {
        let mut cfg = crate::config::Config::load();
        cfg.auto_connect = active;
        if let Err(e) = cfg.save() {
            tracing::error!("Failed to save config: {}", e);
        }
    },
);
feats.append(&ac_row);
```

---

### 4.5 Change 5: Update `LiveWidgets` construction in `build_main_page`

```rust
let live = LiveWidgets {
    status_pill,
    connect_btn,
    btn_icon,
    btn_label,
    location_label,
    ip_label,
    dl_value,
    ul_value,
    lat_value,
    port_value,
    // NEW:
    kill_switch_sw,
    kill_switch_guard: ks_guard,
    port_forward_sw,
    port_forward_guard: pf_guard,
};
```

---

### 4.6 Change 6: Update the timer callback manual clone in `build_ui`

Add the four new fields to the manual clone block in the `glib::timeout_add_seconds_local` closure:

```rust
let live = LiveWidgets {
    status_pill: live.status_pill.clone(),
    connect_btn: live.connect_btn.clone(),
    btn_icon: live.btn_icon.clone(),
    btn_label: live.btn_label.clone(),
    location_label: live.location_label.clone(),
    ip_label: live.ip_label.clone(),
    dl_value: live.dl_value.clone(),
    ul_value: live.ul_value.clone(),
    lat_value: live.lat_value.clone(),
    port_value: live.port_value.clone(),
    // NEW:
    kill_switch_sw: live.kill_switch_sw.clone(),
    kill_switch_guard: live.kill_switch_guard.clone(),
    port_forward_sw: live.port_forward_sw.clone(),
    port_forward_guard: live.port_forward_guard.clone(),
};
```

Note: `gtk4::Switch` implements `Clone` (it is a GObject wrapper). `Rc<Cell<bool>>` implements `Clone` (increments reference count, sharing the same `Cell`).

---

### 4.7 Change 7: Update `refresh_widgets` to set switch state

Add at the **end** of `refresh_widgets`, after the existing port forwarding block:

```rust
    // Kill switch and port forwarding toggle sync
    live.kill_switch_guard.set(true);
    live.kill_switch_sw.set_active(s.kill_switch_enabled);
    live.kill_switch_guard.set(false);

    live.port_forward_guard.set(true);
    live.port_forward_sw.set_active(s.port_forward_enabled);
    live.port_forward_guard.set(false);
```

---

## 5. Summary of All Modified Locations

| Location | Change |
|---|---|
| `src/ui.rs` — top imports | Add `use std::rc::Rc; use std::cell::Cell;` |
| `src/ui.rs` — `struct LiveWidgets` | Add 4 new fields: 2 `gtk4::Switch`, 2 `Rc<Cell<bool>>` |
| `src/ui.rs` — `make_toggle_row` fn | Add `guard` param; return `(adw::ActionRow, gtk4::Switch)` |
| `src/ui.rs` — kill switch block in `build_main_page` | Create guard; destructure return; append row separately |
| `src/ui.rs` — port forward block in `build_main_page` | Create guard; destructure return; append row separately |
| `src/ui.rs` — auto-connect block in `build_main_page` | Destructure `(ac_row, _)`; pass `None` guard; append row |
| `src/ui.rs` — `LiveWidgets` literal in `build_main_page` | Add 4 new fields |
| `src/ui.rs` — manual clone in `build_ui` timer callback | Add 4 new field clones |
| `src/ui.rs` — `refresh_widgets` | Add 6-line guard+set_active block at end |

**Only file modified:** `src/ui.rs`

---

## 6. Risks and Mitigations

| Risk | Severity | Mitigation |
|---|---|---|
| `connect_active_notify` fires on `set_active()` → repeated D-Bus calls | High | `Rc<Cell<bool>>` guard set before/after `set_active()`; signal handler returns early when guard is `true` |
| Guard is cleared before async D-Bus callback resolves | Low | Guard only blocks the **synchronous** signal dispatch, not async work; the async closure captures `active: bool` before guard clears, so there is no race |
| `Rc` is accidentally sent across threads | None | `Rc` is `!Send`; the compiler would reject any attempt to move it off the GTK main thread; all code paths here are `glib::spawn_future_local` (main thread) |
| `gtk4::Switch::clone()` — GObject clone semantics | None | GTK GObject `clone()` increments the reference count; both the `LiveWidgets` closure copy and the original point to the same widget, which is the desired behavior |
| `build_main_page` signature change needed | None | `build_main_page` is `fn` (not `pub`) and only called once from `build_ui`; no external callers |
| Config interaction | None | `kill_switch_enabled` and `port_forward_enabled` are runtime-only fields in `AppState`; they are NOT in `Config` and NOT written to `~/.config/vex-vpn/config.toml`; the fix does not touch config loading or saving |
| `AppState.kill_switch_enabled` initial value | Low | `AppState::new()` initializes both to `false`; on first poll (within 3 s), the real state is read and `refresh_widgets` corrects the UI |

---

## 7. No External Dependencies Required

This fix uses only:
- `std::rc::Rc` (Rust standard library)
- `std::cell::Cell` (Rust standard library)
- Existing `gtk4::Switch` (already a transitive dependency via `gtk4` crate)
- Existing `gtk4::glib` (already imported)

No new crate dependencies, no `Cargo.toml` changes, no `flake.nix` changes.
