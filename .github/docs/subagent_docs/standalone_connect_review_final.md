# Final Review: Standalone Connect — MEDIUM Bug Fixes

**Date:** 2026-05-10  
**Reviewer:** Re-review subagent (Phase 5)  
**Verdict:** APPROVED

---

## 1. Fix Verification

### MEDIUM-1 — Stale Watchdog in `src/state.rs`

**Status: RESOLVED ✔**

Location: `poll_loop()`, lines 207–226.

The watchdog correctly reads `standalone_mode` from the write guard **before** dropping it, then branches:

```rust
let standalone = s.standalone_mode;
drop(s);
if standalone {
    info!("Handshake watchdog (standalone): stale — no auto-restart in standalone mode");
} else {
    info!("Handshake watchdog: restarting pia-vpn.service");
    if let Err(e) = crate::dbus::restart_vpn_unit().await {
        warn!("Watchdog restart failed: {}", e);
    }
}
```

- When `standalone_mode == true`: logs a diagnostic message and **skips** `restart_vpn_unit()`. No D-Bus call is made. No regression risk.  
- When `standalone_mode == false` (normal NixOS path): behavior is identical to pre-fix — `restart_vpn_unit()` is called as before.  
- The lock is released via `drop(s)` before the `await`, preventing a deadlock. Pattern is correct.

### MEDIUM-2 — Cancel-while-Connecting in `src/ui.rs`

**Status: RESOLVED ✔**

Location: `connect_btn` click handler, `ConnectionStatus::Connecting` match arm, lines 601–625.

```rust
ConnectionStatus::Connecting => {
    let (standalone, iface) = {
        let s = state.read().await;
        (s.standalone_mode, s.interface.clone())
    };
    if standalone {
        if let Err(e) = crate::state::standalone_disconnect(&iface).await {
            tracing::error!("cancel standalone: {}", e);
            pill.set_label("● ERROR");
            set_state_class(&pill, "state-error");
            toast.add_toast(adw::Toast::new(&format!("Cancel failed: {e:#}")));
        } else {
            let mut s = state.write().await;
            s.standalone_mode = false;
            s.standalone_privkey = None;
            s.connection = None;
            s.status = crate::state::ConnectionStatus::Disconnected;
        }
    } else if let Err(e) = crate::dbus::disconnect_vpn().await {
        tracing::error!("cancel: {}", e);
        pill.set_label("● ERROR");
        set_state_class(&pill, "state-error");
        toast.add_toast(adw::Toast::new(&format!("Cancel failed: {e:#}")));
    }
}
```

- Reads `standalone_mode` and `interface` from a read guard (no deadlock risk).  
- When `standalone == true`: calls `standalone_disconnect()`, clears `standalone_mode`, `standalone_privkey`, `connection`, and resets `status` to `Disconnected` — full state teardown is correct.  
- When `standalone == false` (normal path): calls `dbus::disconnect_vpn()` — no change to existing behavior.  
- Error paths surface a toast notification in both branches.

### Normal (NixOS) Path — Regression Check

Both changes are strictly conditional on `standalone_mode == true`. The `else` branches in each case preserve the pre-existing call sites (`restart_vpn_unit()` and `disconnect_vpn()`) unchanged. No regressions were introduced.

---

## 2. Build Results

| Step | Command | Result |
|------|---------|--------|
| 1. Clippy | `nix develop --command cargo clippy -- -D warnings` | **PASS** — 0 warnings, 0 errors |
| 2. Debug build | `nix develop --command cargo build` | **PASS** — compiled successfully |
| 3. Test suite | `nix develop --command cargo test` | **PASS** — 33 tests, 0 failures (9 lib + 19 main + 5 integration) |
| 4. Release build | `nix develop --command cargo build --release` | **PASS** — compiled with LTO/strip |
| 5. Nix build | `nix build` | **PASS** — Crane package build succeeded |

---

## 3. Additional vex-vpn-Specific Checks

| Check | Result |
|-------|--------|
| No GTK4 calls outside `src/ui.rs` / GTK main thread | ✔ Pass — fixes are in `state.rs` (async poll loop) and `ui.rs` (GTK main thread) |
| `zbus` usage targets 3.x API only | ✔ Pass — no new zbus calls introduced |
| `Arc<RwLock<AppState>>` used for shared state | ✔ Pass — `state.read().await` / `state.write().await` pattern preserved |
| Config persistence still targets `~/.config/vex-vpn/config.toml` | ✔ Pass — no config changes |
| Binary name `vex-vpn` in `Cargo.toml` | ✔ Pass — unmodified |
| Lock dropped before `await` (no deadlock) | ✔ Pass — `drop(s)` before `restart_vpn_unit().await` in watchdog; read guard released before `standalone_disconnect().await` in UI |

---

## 4. Score Table

| Category | Score | Grade |
|----------|-------|-------|
| Specification Compliance | 100% | A |
| Best Practices | 98% | A |
| Functionality | 100% | A |
| Code Quality | 97% | A |
| Security | 100% | A |
| Performance | 100% | A |
| Consistency | 99% | A |
| Build Success | 100% | A |

**Overall Grade: A (99.25%)**

---

## 5. Verdict

**APPROVED**

Both MEDIUM bugs are fully resolved. All 5 build steps pass. The normal NixOS path has no regressions. No new issues were identified during re-review. The implementation is ready for merge.
