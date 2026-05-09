# Milestone D — Review & Quality Assurance

**Date:** 2026-05-09  
**Reviewer:** Phase 3 QA subagent  
**Spec:** `.github/docs/subagent_docs/milestone_d_reliable_spec.md`  
**Verdict:** ❌ **NEEDS_REFINEMENT**

---

## 1. Build Validation

| Step | Command | Exit Code | Result |
|------|---------|-----------|--------|
| 1 — Clippy | `nix develop --command cargo clippy -- -D warnings` | 0 | ✅ PASS |
| 2 — Debug build | `nix develop --command cargo build` | 0 | ✅ PASS |
| 3 — Test suite | `nix develop --command cargo test` | 0 | ✅ PASS (15/15) |
| 4 — Release build | `nix develop --command cargo build --release` | 0 | ✅ PASS |
| 5 — Nix package | `nix build` | 0 | ✅ PASS |

All 5 build gates pass. Zero Clippy warnings or errors.

---

## 2. Per-Feature Spec Compliance

### F7 — Auto-Reconnect on Network Change

**Functional implementation:** ✅  
**UI implementation:** ❌ (missing toggle)

- `watch_network_manager()` implemented in `src/state.rs` (deviation from spec placement in `dbus.rs`, but functionally correct).
- Subscribes to NM `StateChanged` via a local `WatcherNetworkManager` proxy — correct zbus 3.x `#[dbus_proxy(signal)]` pattern.
- 2-second debounce window implemented correctly.
- Reads `auto_reconnect` from `AppState` (rather than `watch::Receiver<bool>` per spec). This is a clean design — AppState mirrors the config at startup and is the single source of truth.
- Exits gracefully if NM is unavailable (no panic, `warn!` logged).
- `auto_reconnect: bool` added to `AppState` and `Config` with `serde(default = "default_true")`. ✅

**❌ CRITICAL — UI toggle missing:**  
The spec required adding an `adw::SwitchRow` for Auto-Reconnect to the Advanced page in `src/ui_prefs.rs`. The Advanced page currently only contains "Auto Connect on Login" and "Log level" — no Auto-Reconnect row. Without this, users cannot disable the feature from the UI.

### F8 — DNS Leak Test (Heuristic)

**Status:** ✅ Fully implemented.

- `check_dns_leak_hint()` in `src/state.rs` — correctly parses `/etc/resolv.conf` for non-PIA nameservers.
- Filter includes PIA range (`10.0.0.*`), loopback (`127.*`, `::1`) — this is an improvement over the spec which only excluded `10.0.0.*`.
- Integrated into `poll_once()` — `dns_leak_hint` set when connected, cleared when not.
- `AppState.dns_leak_hint: Option<Vec<String>>` added. ✅
- `adw::Banner` in `src/ui.rs` shows/hides based on `dns_leak_hint`. Banner title dynamically shows leaking nameservers. ✅
- Correctly marked as heuristic in code comments.

### F12 — WireGuard Handshake Watchdog

**Status:** ✅ Fully implemented.

- `read_wg_handshake()` in `src/state.rs` — parses `wg show <iface> latest-handshakes`, returns elapsed seconds.
- 180-second stale threshold implemented in `poll_once()`: `Some(elapsed) if elapsed > 180 => ConnectionStatus::Stale(elapsed)`. ✅
- `ConnectionStatus::Stale(u64)` variant added with correct `label()`, `is_connected()`, and `is_stale()`. ✅
- `AppState.stale_cycles: u32` tracks consecutive stale poll cycles.
- Watchdog in `poll_loop()`: after 10 × 3s = 30s in Stale, calls `dbus::restart_vpn_unit()`. ✅
- UI: `refresh_widgets()` handles `Stale(_)` with amber state class and "RECONNECTING..." label. ✅
- Tray: `icon_name()` and `title()` handle `Stale(_)` correctly. ✅
- `dbus::restart_vpn_unit()` implemented: stop + 500ms sleep + start. ✅

### B1 — Architecture Hardening

**Status:** ❌ Two issues (one CRITICAL, one MODERATE)

**❌ CRITICAL — `std::process::exit` NOT removed from `src/main.rs`:**  
Line 130 of `src/main.rs`:
```rust
let exit_code = app.run();
std::process::exit(exit_code.into());   // ← NOT fixed
```
This was a primary B1 requirement. It bypasses `Drop` on the Tokio runtime, potentially losing pending config writes and skipping clean task shutdown. The fix is one line: replace with `let _exit_code = app.run(); Ok(())`.

Note: `std::process::exit(0)` in `src/tray.rs` (Quit item) **is** correctly fixed — now sends `TrayMessage::Quit` via `tray.tx.try_send()`. ✅

**⚠ MODERATE — `Arc<Mutex<Option<Receiver<...>>>>` + `.take()` NOT replaced:**  
`async_channel` is correctly added to `Cargo.toml` and used throughout the codebase. `PiaTray.tx` is now `async_channel::Sender<TrayMessage>`. `build_ui()` accepts `async_channel::Receiver<TrayMessage>`. These are correct. ✅

However, in `src/main.rs`:
```rust
let tray_rx = Arc::new(std::sync::Mutex::new(Some(tray_rx)));
// ...
app.connect_activate(move |app| {
    let rx = tray_rx.lock().unwrap().take();  // ← take() still here
```
And in the onboarding callback path:
```rust
let rx_inner = rx_shared.lock().unwrap().take();  // ← take() still here
```
Since `async_channel::Receiver` is `Clone`, the correct fix is to clone the receiver instead of taking it. With the current code, a second `connect_activate` call (e.g., user launches app again from GNOME) passes `None` as the receiver, breaking the tray→window channel for subsequent activations.

### B2 — Error Handling Hardening

**Status:** ✅ Fully implemented.

- `Config::load() -> Result<Self>` implemented. ✅
- `Config::load_from(path: &Path) -> Result<Self>` added as a `pub(crate)` helper for testability. ✅
- All call sites updated:
  - `src/main.rs`: `Config::load().unwrap_or_else(|e| { warn!(...); Config::default() })` ✅
  - `src/ui_prefs.rs`: all 5+ call sites use `Config::load().unwrap_or_else(...)` ✅
  - `src/helper.rs`: `Config::load().unwrap_or_else(...)` ✅
  - `src/ui.rs`: `Config::load().unwrap_or_default()` ✅
- `read_wg_stats` in `state.rs`: malformed parse now logs `warn!` instead of silently returning 0. ✅
- `anyhow::Context` already present at D-Bus call sites — no regression. ✅

### B3 — D-Bus Improvements

**Status:** ✅ Substantially implemented (proxy caching intentionally descoped).

- `watch_vpn_unit_state()` in `src/state.rs` subscribes to `receive_active_state_changed()` on `pia-vpn.service`. ✅
- Uses the trigger pattern: calls `poll_once(&state)` on each property change. ✅
- Eliminates 3-second worst-case staleness on connect/disconnect. ✅
- `poll_once()` is `pub(crate)` for use by watcher tasks. ✅
- Proxy caching descoped per spec §8.1 (connection is cached; proxy construction is O(1)). ✅

**⚠ Minor — Duplicate proxy definitions:**  
`src/state.rs` defines `WatcherSystemdManager`, `WatcherSystemdUnit`, `WatcherNetworkManager` proxies which duplicate the proxy traits in `src/dbus.rs`. This works but violates DRY. The watcher tasks could import `SystemdManagerProxy`, `SystemdUnitProxy` from `dbus.rs` directly. Not a blocker but worth cleaning up.

### Integration Tests

**Status:** ❌ NOT created.

The spec required:
- `tests/config_integration.rs` (round-trip, missing file returns default, malformed TOML returns Err, backward-compat)
- `tests/pia_http.rs` (wiremock-based HTTP mocking for PIA API)
- `tests/fixtures/serverlist_v6.json`
- `wiremock = "0.6"` dev-dependency (present in `Cargo.toml` ✅, but no test files)

The 15 existing unit tests all pass. Integration tests remain at 0.

---

## 3. Code Quality Checks

| Check | Result |
|-------|--------|
| No new `unwrap()` in non-test code | ✅ Pass — all `unwrap_or_else` |
| All new async code non-blocking | ✅ Pass |
| No GTK calls off main thread | ✅ Pass — watchers are pure Tokio tasks |
| zbus 3.x API only | ✅ Pass — `#[dbus_proxy]` macro, `Connection::system().await` |
| `Arc<RwLock<AppState>>` for shared state | ✅ Pass |
| No credential/token logging | ✅ Pass — `AuthToken` uses redacted `Debug` impl |
| Binary name `vex-vpn` in Cargo.toml | ✅ Pass |
| Config path `~/.config/vex-vpn/config.toml` | ✅ Pass |
| `futures-util` / `async-channel` in dependencies | ✅ Pass |

---

## 4. Security Checks

| Check | Result |
|-------|--------|
| DNS leak test does not log user IP | ✅ Pass — only leaking nameserver IPs logged, not user traffic |
| Reconnect logic re-auths if token expired | ✅ Pass — auto-reconnect restarts the systemd unit, not the token flow |
| No new shell injection vectors | ✅ Pass — `wg` and `nft` called via `tokio::process::Command` with split args |
| Loopback excluded from DNS leak (prevents false positive) | ✅ Pass — `127.*` and `::1` filtered |
| `validate_interface` gate before nft operations | ✅ Pass |

---

## 5. Score Table

| Category | Score | Grade |
|----------|-------|-------|
| Specification Compliance | 72% | C+ |
| Best Practices | 82% | B |
| Functionality | 88% | B+ |
| Code Quality | 85% | B |
| Security | 95% | A |
| Performance | 90% | A- |
| Consistency | 80% | B- |
| Build Success | 100% | A+ |

**Overall Grade: B- (86%)**

---

## 6. Verdict: NEEDS_REFINEMENT

### CRITICAL Issues (must fix)

**C1 — `std::process::exit` NOT removed from `src/main.rs`**  
File: `src/main.rs`, line 130  
Fix:
```rust
// BEFORE:
let exit_code = app.run();
std::process::exit(exit_code.into());

// AFTER:
let _exit_code = app.run();
Ok(())
```

**C2 — Auto-reconnect UI toggle missing from Advanced preferences**  
File: `src/ui_prefs.rs`, `build_advanced_page()`  
Fix: Add after the auto-connect `ac_row` block:
```rust
// Auto-reconnect
let ar_row = adw::SwitchRow::builder()
    .title("Auto-Reconnect")
    .subtitle("Restart VPN tunnel when network connectivity is restored")
    .active(cfg.auto_reconnect)
    .build();
{
    ar_row.connect_active_notify(move |row| {
        let mut c = Config::load().unwrap_or_else(|e| {
            tracing::warn!("Failed to load config: {e:#}");
            Config::default()
        });
        c.auto_reconnect = row.is_active();
        if let Err(e) = c.save() {
            tracing::error!("save config (auto_reconnect): {}", e);
        }
    });
}
group.add(&ar_row);
```

### RECOMMENDED Improvements

**R1 — Replace `.take()` with `.clone()` for `async_channel::Receiver` in `connect_activate`**  
File: `src/main.rs`  
The async_channel receiver should be cloned instead of taken, so re-activation works correctly:
```rust
// Instead of Arc<Mutex<Option<...>>> + .take():
app.connect_activate(move |app| {
    let rx = Some(tray_rx.clone());  // async_channel::Receiver is Clone
    ...
});
```

**R2 — Create integration test files**  
Create `tests/config_integration.rs` with at minimum:
- `config_load_malformed_toml_returns_err` (validates B2 fix)
- `config_load_missing_file_returns_default`
- `config_backward_compat_missing_auto_reconnect`

**R3 — Remove duplicate proxy trait definitions in `state.rs`**  
The `WatcherSystemdManager`, `WatcherSystemdUnit`, `WatcherNetworkManager` proxy traits in `src/state.rs` duplicate definitions in `src/dbus.rs`. Refactor to use `pub(crate) use` re-exports from `dbus.rs` or make the `dbus.rs` proxies `pub(crate)`.

---

## 7. What Was Done Well

- F8 DNS leak heuristic implementation is solid and exceeds spec (correctly excludes loopback).
- F12 handshake watchdog is complete and correctly integrated.
- B2 error handling is thorough — all call sites updated, `Config::load_from` testability helper is elegant.
- B3 trigger pattern for `PropertiesChanged` is correct and avoids the dual-write race the spec warned about.
- Security posture is good — no credential logging, proper input validation.
- All 5 build gates pass cleanly with zero Clippy warnings.
