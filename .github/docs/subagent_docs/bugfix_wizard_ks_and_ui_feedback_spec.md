# Specification: Bugfix — Kill Switch Wizard State + UI Error Feedback

**Feature name:** `bugfix_wizard_ks_and_ui_feedback`  
**Files to modify:** `src/state.rs`, `src/ui_onboarding.rs`, `src/ui.rs`  
**New dependencies:** None  
**Risk level:** Low (additive changes only; no structural refactors)

---

## 1. Current-State Analysis

### 1.1 `src/state.rs` — `AppState::new_with_config()`

Lines 132–139 of `src/state.rs`:

```rust
pub fn new_with_config(config: &Config) -> Self {
    Self {
        auto_connect: config.auto_connect,
        interface: config.interface.clone(),
        selected_region_id: config.selected_region_id.clone(),
        auto_reconnect: config.auto_reconnect,
        ..Self::new()
    }
}
```

`Self::new()` (lines 111–130) hard-codes `kill_switch_enabled: false`. Because the
`..Self::new()` spread fills in all remaining fields, the explicit struct initialiser
above **never assigns `kill_switch_enabled`**. This means that even when
`config.toml` contains `kill_switch_enabled = true`, the returned `AppState`
always starts with `kill_switch_enabled = false`.

**Confirmed bug:** `kill_switch_enabled` is a field of `Config` (src/config.rs line 35,
with `#[serde(default)]`) and is correctly persisted to disk, but is never copied into
`AppState` at startup.

---

### 1.2 `src/ui_onboarding.rs` — page 3 Next handler

Lines 228–250 (page index 3 branch inside `next_btn.connect_clicked`):

```rust
3 => {
    let ks_active = ks_switch_c.is_active();
    let mut cfg = crate::config::Config::load().unwrap_or_else(|e| {
        tracing::warn!("Failed to load config: {e:#}");
        crate::config::Config::default()
    });
    cfg.kill_switch_enabled = ks_active;
    if let Err(e) = cfg.save() {
        tracing::error!("save config (kill switch): {}", e);
    }

    if ks_active {
        let iface = cfg.interface.clone();
        glib::spawn_future_local(async move {
            if let Err(e) = crate::helper::apply_kill_switch(&iface).await {
                tracing::warn!("apply kill switch (onboarding): {}", e);
            }
        });
    }

    scroll_to_next(&carousel_c, page_idx, n_pages);
}
```

**Confirmed bugs:**

1. `state_c.write().await.kill_switch_enabled` is **never set**. The async block is
   conditionally spawned only when `ks_active` is `true`, and even then it never
   writes back to the shared `Arc<RwLock<AppState>>`. When `on_complete()` fires
   and `build_and_show_main_window()` calls `refresh_widgets()`, the toggle always
   shows unchecked because `AppState::kill_switch_enabled` is still `false`.

2. When `ks_active` is `false`, no async block is spawned at all, so the state
   field is also not reset (harmless for false→false transitions but conceptually
   inconsistent).

---

### 1.3 `src/ui.rs` — connect/disconnect, kill-switch, port-forward handlers

`build_main_page` signature (line ~485):

```rust
fn build_main_page(
    state: Arc<RwLock<AppState>>,
    initial_auto_connect: bool,
) -> (gtk4::Box, LiveWidgets)
```

**Connect button** (lines ~535–573): on D-Bus error, calls `tracing::error!` and
updates the pill label to `"● ERROR"`, but no toast or persistent message is shown.

**Kill switch toggle** (lines ~608–632): on helper error, calls `tracing::error!`
silently — no user-visible feedback.

**Port forward toggle** (lines ~636–652): on D-Bus error, calls `tracing::error!`
silently — no user-visible feedback.

`build_ui()` (lines ~143–232) constructs a `adw::ToolbarView` and sets it as window
content directly — no `adw::ToastOverlay` wrapper exists yet.

---

## 2. Problem Definitions

| # | Bug | Impact |
|---|-----|--------|
| 1a | `new_with_config()` does not copy `kill_switch_enabled` from config | Kill switch preference is always lost at startup |
| 1b | Onboarding wizard page 3 does not write `kill_switch_enabled` back to `AppState` | Main window toggle always starts unchecked after first-run wizard |
| 2 | No `adw::ToastOverlay` — button handler errors have no visible feedback | All action failures are silent to the user |

---

## 3. Proposed Solution Architecture

### Fix 1a — One-line addition to `AppState::new_with_config()`

Add `kill_switch_enabled: config.kill_switch_enabled` to the explicit fields list
before the `..Self::new()` spread. No other changes to `state.rs`.

### Fix 1b — Restructure page 3 async block to always update AppState

Replace the conditional `if ks_active { glib::spawn_future_local(...) }` block with
an unconditional `glib::spawn_future_local` that:

1. Always writes `state_for_ks.write().await.kill_switch_enabled = ks_active`.
2. Conditionally calls `apply_kill_switch` only when `ks_active` is `true`.

The clone of `state_c` needed for the move must be captured before the block.
The `iface` clone from `cfg.interface` can be unconditional since the clone is cheap.

### Fix 2 — Add `adw::ToastOverlay` wrapper in `build_ui()`

Wrap the `adw::ToolbarView` in an `adw::ToastOverlay` inside `build_ui()`.
Pass the `adw::ToastOverlay` (cloned) into `build_main_page()` as an additional
parameter so the connect, kill-switch, and port-forward handlers can call
`toast_overlay.add_toast(...)` on failure.

`adw::Toast` is available in libadwaita 0.5.x (the pinned version) — no new
dependencies are required.

---

## 4. Exact Code Changes

### 4.1 `src/state.rs`

**Location:** `AppState::new_with_config()`, inside the `Self { … }` initialiser.

**Before:**

```rust
    pub fn new_with_config(config: &Config) -> Self {
        Self {
            auto_connect: config.auto_connect,
            interface: config.interface.clone(),
            selected_region_id: config.selected_region_id.clone(),
            auto_reconnect: config.auto_reconnect,
            ..Self::new()
        }
    }
```

**After:**

```rust
    pub fn new_with_config(config: &Config) -> Self {
        Self {
            auto_connect: config.auto_connect,
            interface: config.interface.clone(),
            selected_region_id: config.selected_region_id.clone(),
            auto_reconnect: config.auto_reconnect,
            kill_switch_enabled: config.kill_switch_enabled,
            ..Self::new()
        }
    }
```

**Diff summary:** +1 line (`kill_switch_enabled: config.kill_switch_enabled,`)

---

### 4.2 `src/ui_onboarding.rs`

**Location:** page 3 branch inside `next_btn.connect_clicked`, the block starting at
`3 => {`.

**Before:**

```rust
                3 => {
                    // Kill Switch → Done: save kill switch choice
                    let ks_active = ks_switch_c.is_active();
                    let mut cfg = crate::config::Config::load().unwrap_or_else(|e| {
                        tracing::warn!("Failed to load config: {e:#}");
                        crate::config::Config::default()
                    });
                    cfg.kill_switch_enabled = ks_active;
                    if let Err(e) = cfg.save() {
                        tracing::error!("save config (kill switch): {}", e);
                    }

                    if ks_active {
                        let iface = cfg.interface.clone();
                        glib::spawn_future_local(async move {
                            if let Err(e) = crate::helper::apply_kill_switch(&iface).await {
                                tracing::warn!("apply kill switch (onboarding): {}", e);
                            }
                        });
                    }

                    scroll_to_next(&carousel_c, page_idx, n_pages);
                }
```

**After:**

```rust
                3 => {
                    // Kill Switch → Done: save kill switch choice
                    let ks_active = ks_switch_c.is_active();
                    let mut cfg = crate::config::Config::load().unwrap_or_else(|e| {
                        tracing::warn!("Failed to load config: {e:#}");
                        crate::config::Config::default()
                    });
                    cfg.kill_switch_enabled = ks_active;
                    if let Err(e) = cfg.save() {
                        tracing::error!("save config (kill switch): {}", e);
                    }

                    // Always update AppState so refresh_widgets() sees the correct
                    // value when the main window opens after on_complete().
                    let state_for_ks = state_c.clone();
                    let iface = cfg.interface.clone();
                    glib::spawn_future_local(async move {
                        state_for_ks.write().await.kill_switch_enabled = ks_active;
                        if ks_active {
                            if let Err(e) = crate::helper::apply_kill_switch(&iface).await {
                                tracing::warn!("apply kill switch (onboarding): {}", e);
                            }
                        }
                    });

                    scroll_to_next(&carousel_c, page_idx, n_pages);
                }
```

**Diff summary:**
- Remove: `if ks_active { let iface = … glib::spawn_future_local(…) }` (5 lines)
- Add: unconditional `let state_for_ks`/`let iface` + `glib::spawn_future_local` that
  always writes the AppState field and conditionally calls the helper (8 lines)

---

### 4.3 `src/ui.rs`

#### 4.3.1 Add `adw::ToastOverlay` in `build_ui()` and thread it into `build_main_page()`

**Location A — `build_ui()` call site of `build_main_page`:**

**Before:**

```rust
    let (main_page, live) = build_main_page(state.clone(), initial_auto_connect);
```

**After:**

```rust
    let toast_overlay = adw::ToastOverlay::new();
    let (main_page, live) = build_main_page(state.clone(), initial_auto_connect, toast_overlay.clone());
```

**Location B — `build_ui()` — window content assembly (where `toolbar_view` is set as
window content):**

**Before:**

```rust
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&root));

    window.set_content(Some(&toolbar_view));
```

**After:**

```rust
    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&root));

    toast_overlay.set_child(Some(&toolbar_view));
    window.set_content(Some(&toast_overlay));
```

#### 4.3.2 Update `build_main_page()` signature

**Before:**

```rust
fn build_main_page(
    state: Arc<RwLock<AppState>>,
    initial_auto_connect: bool,
) -> (gtk4::Box, LiveWidgets) {
```

**After:**

```rust
fn build_main_page(
    state: Arc<RwLock<AppState>>,
    initial_auto_connect: bool,
    toast_overlay: adw::ToastOverlay,
) -> (gtk4::Box, LiveWidgets) {
```

#### 4.3.3 Connect button — replace silent error logging with toast

**Location:** Inside the `connect_btn.connect_clicked` closure, inside
`glib::spawn_future_local`, the three error arms.

Capture `toast_overlay` by clone at each closure level following the existing
`state_c`, `pill_c` pattern.

**Before (disconnect arm):**

```rust
                        if let Err(e) = crate::dbus::disconnect_vpn().await {
                            tracing::error!("disconnect: {}", e);
                            pill.set_label("● ERROR");
                            set_state_class(&pill, "state-error");
                        }
```

**After:**

```rust
                        if let Err(e) = crate::dbus::disconnect_vpn().await {
                            let msg = format!("Disconnect failed: {}", e);
                            tracing::error!("{}", msg);
                            pill.set_label("● ERROR");
                            set_state_class(&pill, "state-error");
                            let toast = adw::Toast::builder().title(&msg).timeout(5).build();
                            toast_overlay_ref.add_toast(toast);
                        }
```

**Before (cancel/connecting arm):**

```rust
                        if let Err(e) = crate::dbus::disconnect_vpn().await {
                            tracing::error!("cancel: {}", e);
                            pill.set_label("● ERROR");
                            set_state_class(&pill, "state-error");
                        }
```

**After:**

```rust
                        if let Err(e) = crate::dbus::disconnect_vpn().await {
                            let msg = format!("Cancel failed: {}", e);
                            tracing::error!("{}", msg);
                            pill.set_label("● ERROR");
                            set_state_class(&pill, "state-error");
                            let toast = adw::Toast::builder().title(&msg).timeout(5).build();
                            toast_overlay_ref.add_toast(toast);
                        }
```

**Before (connect arm):**

```rust
                        if let Err(e) = crate::dbus::connect_vpn().await {
                            tracing::error!("connect: {}", e);
                            pill.set_label("● ERROR");
                            set_state_class(&pill, "state-error");
                        }
```

**After:**

```rust
                        if let Err(e) = crate::dbus::connect_vpn().await {
                            let msg = format!("Connection failed: {}", e);
                            tracing::error!("{}", msg);
                            pill.set_label("● ERROR");
                            set_state_class(&pill, "state-error");
                            let toast = adw::Toast::builder().title(&msg).timeout(5).build();
                            toast_overlay_ref.add_toast(toast);
                        }
```

The variable `toast_overlay_ref` must be captured inside the connect button closure.
Add the following clones in the outer closure capture block (alongside `state_c`,
`pill_c`, etc.):

```rust
        let toast_overlay_c = toast_overlay.clone();
```

And inside the inner `glib::spawn_future_local` move block:

```rust
                let toast_overlay_ref = toast_overlay_c.clone();
```

#### 4.3.4 Kill switch toggle — replace silent error logging with toast

**Location:** Inside `make_toggle_row` callback for the kill switch, inside the
`glib::spawn_future_local` block.

Capture `toast_overlay` as `toast_overlay_ks` in the outer closure (before
`make_toggle_row`), then move into the async block.

**Before:**

```rust
                    if let Err(e) = res {
                        tracing::error!("kill switch toggle: {}", e);
                    }
```

**After:**

```rust
                    if let Err(e) = res {
                        let msg = format!("Kill switch error: {}", e);
                        tracing::error!("{}", msg);
                        let toast = adw::Toast::builder().title(&msg).timeout(5).build();
                        toast_overlay_ks.add_toast(toast);
                    }
```

#### 4.3.5 Port forward toggle — replace silent error logging with toast

**Location:** Inside `make_toggle_row` callback for port forwarding, inside the
`glib::spawn_future_local` block.

Capture `toast_overlay` as `toast_overlay_pf` in the outer closure (before
`make_toggle_row`), then move into the async block.

**Before:**

```rust
                    if let Err(e) = res {
                        tracing::error!("port forward toggle: {}", e);
                    }
```

**After:**

```rust
                    if let Err(e) = res {
                        let msg = format!("Port forward error: {}", e);
                        tracing::error!("{}", msg);
                        let toast = adw::Toast::builder().title(&msg).timeout(5).build();
                        toast_overlay_pf.add_toast(toast);
                    }
```

---

## 5. Closure Capture Changes Summary for `src/ui.rs`

Because `toast_overlay` is not `Copy`, it must be explicitly cloned at each closure
boundary. The required capture pattern mirrors how `state_c`, `pill_c`, etc., are
currently handled. The implementation subagent must:

1. Clone `toast_overlay` into the outer `connect_btn.connect_clicked` closure as
   `toast_overlay_c`.
2. Clone `toast_overlay_c` inside the `glib::spawn_future_local` move block as
   `toast_overlay_ref`.
3. Clone `toast_overlay` before the kill switch `make_toggle_row` call as
   `toast_overlay_ks`; move it into the async block.
4. Clone `toast_overlay` before the port forward `make_toggle_row` call as
   `toast_overlay_pf`; move it into the async block.

---

## 6. Implementation Steps

1. Edit `src/state.rs`: add `kill_switch_enabled: config.kill_switch_enabled` to
   `new_with_config()`.
2. Edit `src/ui_onboarding.rs`: replace the conditional `if ks_active { … }` block
   with an unconditional `glib::spawn_future_local` that writes the AppState field
   first, then conditionally applies the kill switch.
3. Edit `src/ui.rs`:
   a. Create `toast_overlay` before calling `build_main_page`.
   b. Update `build_main_page` signature to accept `toast_overlay: adw::ToastOverlay`.
   c. Wrap `toolbar_view` with `toast_overlay` before setting window content.
   d. Add toast emission to connect button error arms (3 arms).
   e. Add toast emission to kill switch toggle error arm.
   f. Add toast emission to port forward toggle error arm.

---

## 7. No New Dependencies

All types used (`adw::ToastOverlay`, `adw::Toast`, `adw::Toast::builder()`) are
present in libadwaita 0.5.x, which is already declared in `Cargo.toml`. No new
`[dependencies]` entries are required.

---

## 8. Risk Assessment

| Area | Risk | Mitigation |
|------|------|------------|
| Fix 1a — state.rs | Very low — single field addition, no logic change | Covered by existing `config_integration` tests |
| Fix 1b — ui_onboarding.rs | Low — async block restructure within existing pattern | Behaviour for `ks_active = false` is now explicit (was implicit no-op) |
| Fix 2 — ui.rs signature change | Low — additive parameter; one call site in `build_ui()` | Compiler enforces all call sites are updated |
| Fix 2 — toast overlay wrapping | Low — `adw::ToastOverlay` is a transparent widget wrapper | Visual regression risk is negligible; overlay passes events through |
| GTK thread safety | None — all changes remain on the GTK main thread | No new threads introduced |
| zbus / async runtime | None — no new D-Bus calls or async tasks introduced | Existing tokio + glib executor pattern unchanged |
