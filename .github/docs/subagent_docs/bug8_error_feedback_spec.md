# BUG 8 — Spec: Immediate Error Feedback on Connect/Disconnect Failure

> Feature: `bug8_error_feedback`  
> Severity: Low  
> File(s): `src/ui.rs`  
> Date: 2026-05-09

---

## 1. Current State Analysis

### 1.1 Connect Button Click Handler (lines ~320–380, `src/ui.rs`)

The connect button uses a single `connect_clicked` closure that branches on the current `ConnectionStatus`.
All widget handles available in scope inside the closure:

| Variable | Type | Description |
|----------|------|-------------|
| `pill` | `gtk4::Label` | status pill (`status_pill` clone) |
| `btn` | `gtk4::Button` | connect button clone |
| `lbl` | `gtk4::Label` | button label clone |
| `icon` | `gtk4::Image` | button icon clone |
| `state` | `Arc<RwLock<AppState>>` | shared state |

#### Branch A — `Connected | KillSwitchActive` (calls `disconnect_vpn`)

```rust
pill.set_label("● DISCONNECTING...");
set_state_class(&pill, "state-connecting");
set_state_class(&btn, "state-connecting");
lbl.set_label("CANCEL");
icon.set_icon_name(Some("network-vpn-acquiring-symbolic"));

if let Err(e) = crate::dbus::disconnect_vpn().await {
    tracing::error!("disconnect: {}", e);
    // ← NO UI UPDATE — pill stays "● DISCONNECTING..." until next 3-second poll tick
}
```

#### Branch B — `Connecting` (calls `disconnect_vpn` as cancel)

```rust
let _ = crate::dbus::disconnect_vpn().await;
// ← error silently discarded
```

#### Branch C — `Disconnected` / default (calls `connect_vpn`)

```rust
pill.set_label("● CONNECTING...");
set_state_class(&pill, "state-connecting");
set_state_class(&btn, "state-connecting");
lbl.set_label("CANCEL");
icon.set_icon_name(Some("network-vpn-acquiring-symbolic"));

if let Err(e) = crate::dbus::connect_vpn().await {
    tracing::error!("connect: {}", e);
    // ← NO UI UPDATE — pill stays "● CONNECTING..." until next 3-second poll tick
}
```

### 1.2 D-Bus Function Signatures (`src/dbus.rs`)

```rust
pub async fn connect_vpn() -> Result<()>     // anyhow::Result<()>
pub async fn disconnect_vpn() -> Result<()>  // anyhow::Result<()>
```

Both return `anyhow::Result<()>`. The error path is a recoverable, non-panicking failure.

### 1.3 `refresh_widgets` Already Has an Error State

`refresh_widgets` (lines ~525–565) handles `ConnectionStatus::Error(_)`:

```rust
ConnectionStatus::Error(_) => (
    "● ERROR",
    "state-error",
    "RETRY",
    "network-vpn-disabled-symbolic",
),
```

CSS for `.status-pill.state-error` is already defined:

```css
.status-pill.state-error { background: rgba(255,80,80,.10); color: #ff5050; }
```

The `set_state_class` helper (already in scope) cycles through all four state classes
(`state-connected`, `state-disconnected`, `state-connecting`, `state-error`) and applies only the new one.

### 1.4 `adw::ToastOverlay` — NOT Present in Widget Tree

Current widget hierarchy in `build_ui`:

```
adw::ApplicationWindow
  └─ gtk4::Box (root, horizontal)
       ├─ gtk4::Box  (sidebar — build_sidebar())
       └─ gtk4::Box  (main_page — build_main_page() → LiveWidgets)
```

There is **no** `adw::ToastOverlay` anywhere in the widget tree.
No call to `adw::Toast` / `add_toast` exists anywhere in `src/ui.rs`.

---

## 2. Problem Definition

When `connect_vpn()` or `disconnect_vpn()` fails, the UI is left in a stale intermediate state:

- Status pill shows `"● CONNECTING..."` or `"● DISCONNECTING..."` (the yellow state-connecting style)
- This state persists until the next 3-second `glib::timeout_add_seconds_local` poll tick fires,
  reads `AppState`, and calls `refresh_widgets`
- The user sees no indication that an error occurred; the spinner-like visual implies "still working"
- In the worst case (D-Bus/systemd unavailable), this could stall for up to 3 seconds with no feedback

---

## 3. Proposed Solution

### 3.1 Approach Comparison

| Approach | Structural changes | User experience | Implementation effort |
|----------|--------------------|-----------------|----------------------|
| **A. Status pill → ERROR** | None | Immediate, visible, in-context | Minimal (2–4 lines per branch) |
| B. `adw::ToastOverlay` toast | Add `ToastOverlay` wrapper, thread ref into closure | Temporary dismissable overlay | Medium (structural refactor) |
| C. Hybrid (A + B) | Same as B | Best UX | Higher effort |

**Recommendation: Approach A** — immediately set the status pill and button to the error state on failure.

Rationale:
- All required widget references (`pill`, `btn`, `lbl`, `icon`) are **already captured** in the existing
  click closure. Zero new closure captures or structural changes are needed.
- `set_state_class` is already defined and handles `"state-error"` correctly.
- The CSS `.status-pill.state-error` style is already written.
- `refresh_widgets` already maps `ConnectionStatus::Error(_)` to the same visual state,
  so the error display is visually consistent with the poll-loop view.
- Adding `adw::ToastOverlay` would require wrapping the widget tree, returning the overlay from
  `build_main_page` (or `build_ui`), and threading it into the click closure — a moderate refactor
  for marginal UX improvement given the status pill already provides clear error feedback.

A `ToastOverlay` can be added in a future enhancement (e.g., BUG 8b) without touching this fix.

### 3.2 Exact Changes to `src/ui.rs`

#### Change 1 — `disconnect_vpn` error path (Branch A)

**Before:**
```rust
if let Err(e) = crate::dbus::disconnect_vpn().await {
    tracing::error!("disconnect: {}", e);
}
```

**After:**
```rust
if let Err(e) = crate::dbus::disconnect_vpn().await {
    tracing::error!("disconnect: {}", e);
    pill.set_label("● ERROR");
    set_state_class(&pill, "state-error");
    set_state_class(&btn, "state-error");
    lbl.set_label("RETRY");
    icon.set_icon_name(Some("network-vpn-disabled-symbolic"));
}
```

#### Change 2 — `disconnect_vpn` silent-discard cancel path (Branch B)

**Before:**
```rust
let _ = crate::dbus::disconnect_vpn().await;
```

**After:**
```rust
if let Err(e) = crate::dbus::disconnect_vpn().await {
    tracing::error!("disconnect (cancel): {}", e);
}
```

No visual change needed here — the pill was not pre-set to an intermediate state in this branch
(the `Connecting` arm just cancels silently), so logging the error is sufficient.

#### Change 3 — `connect_vpn` error path (Branch C)

**Before:**
```rust
if let Err(e) = crate::dbus::connect_vpn().await {
    tracing::error!("connect: {}", e);
}
```

**After:**
```rust
if let Err(e) = crate::dbus::connect_vpn().await {
    tracing::error!("connect: {}", e);
    pill.set_label("● ERROR");
    set_state_class(&pill, "state-error");
    set_state_class(&btn, "state-error");
    lbl.set_label("RETRY");
    icon.set_icon_name(Some("network-vpn-disabled-symbolic"));
}
```

### 3.3 Why These Widget Values

The values chosen for the error state match exactly what `refresh_widgets` produces for
`ConnectionStatus::Error(_)`:

| Widget | Value |
|--------|-------|
| `pill.set_label` | `"● ERROR"` |
| pill CSS class | `"state-error"` |
| button CSS class | `"state-error"` |
| `lbl.set_label` | `"RETRY"` |
| `icon.set_icon_name` | `"network-vpn-disabled-symbolic"` |

This guarantees visual consistency: the UI looks identical whether the error state is shown
immediately (on failure) or after the next poll tick reads `ConnectionStatus::Error(_)`.

---

## 4. Implementation Steps

1. Open `src/ui.rs`
2. Locate the `connect_btn.connect_clicked` closure — the `glib::spawn_future_local` block
3. Apply Change 1 to the `Connected | KillSwitchActive` branch (`disconnect_vpn` error path)
4. Apply Change 2 to the `Connecting` branch (silent discard → log)
5. Apply Change 3 to the `_ =>` default branch (`connect_vpn` error path)
6. Run `nix develop --command cargo clippy -- -D warnings` — must pass zero warnings
7. Run `nix develop --command cargo build` — must compile
8. Run `nix develop --command cargo test` — must pass

---

## 5. Dependencies

No new crates or dependencies required.  
All widgets, CSS classes, and helper functions already exist in `src/ui.rs`.

---

## 6. Risks and Mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| Error state cleared by next poll tick even if still failing | Low | The poll loop sets `ConnectionStatus::Error(_)` if systemd reports the unit is not active, so `refresh_widgets` will re-apply the error state on each tick |
| `pill` / `btn` captures go stale | None | GTK4 widgets are reference-counted (`Rc`-backed GObjects); clones are always valid for the window lifetime |
| Visual inconsistency between immediate error state and poll-tick state | None | Chosen values exactly match what `refresh_widgets` produces for `ConnectionStatus::Error(_)` |
| Shadowing `pill` / `btn` in inner async block | None | The inner `glib::spawn_future_local` already clones all vars with `let pill = pill_c.clone()` etc.; the error handling appended inside that block uses those same locals |

---

## 7. Out of Scope

- `adw::ToastOverlay` integration (future enhancement)
- Kill switch and port forward toggle error feedback (separate bugs)
- Error state timeout / auto-recovery

---

## Summary

**What:** Three targeted additions to the connect button's `glib::spawn_future_local` async block:
on `connect_vpn()` failure and on `disconnect_vpn()` failure, immediately set the status pill and
button to the error visual state rather than leaving them stuck at "CONNECTING..." or "DISCONNECTING...".

**Why it's the right approach:** All required widget references are already captured in the existing
closure. The error CSS class and its styling are already defined. `refresh_widgets` already defines the
correct error state visual — this fix just applies the same update immediately without waiting for
the next 3-second poll tick.

**`adw::ToastOverlay` verdict:** NOT present in the widget tree, and adding it is not necessary for
this fix. The status pill approach is sufficient and simpler.
