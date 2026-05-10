# Review: Standalone Connect Mode (`standalone_connect`)

**Date:** 2026-05-10  
**Reviewer:** QA Subagent  
**Spec:** `.github/docs/subagent_docs/standalone_connect_spec.md`  
**Verdict:** NEEDS_REFINEMENT

---

## 1. Files Reviewed

| File | Purpose |
|---|---|
| `src/pia.rs` | `add_key` implementation, `WgKeyResponse`, `PiaClient` |
| `src/helper.rs` | `write_wireguard_config`, `wg_quick_up`, `wg_quick_down` callers |
| `src/bin/helper.rs` | `WriteWireguardConfig`, `WgQuickUp`, `WgQuickDown` handlers |
| `src/state.rs` | `standalone_connect`, `standalone_disconnect`, `generate_wg_keypair`, `build_wg_config`, `poll_once`, `poll_loop` |
| `src/ui.rs` | `NoSuchUnit` fallback branch in connect button handler, `is_no_such_unit` |

---

## 2. Security Review

### 2.1 Findings — All PASS

| Check | Result | Notes |
|---|---|---|
| Interface name validation in helper caller (`src/helper.rs`) | ✅ PASS | `validate_interface()` called before every `call_helper()` invocation |
| Interface name validation in helper binary (`src/bin/helper.rs`) | ✅ PASS | `is_valid_interface()` checked in every command handler — defense in depth |
| Config file permissions | ✅ PASS | `OpenOptions::mode(0o600)` applied before writing |
| Atomic config write (tmp → rename) | ✅ PASS | Written to `.conf.tmp`, then `std::fs::rename()` to final path |
| `fsync` before rename | ✅ PASS | `f.sync_all()` called before `drop(f)`, ensuring durability |
| Null byte check in config content | ✅ PASS | `config.contains('\0')` rejected in helper binary |
| PIA CA cert pinning | ✅ PASS | `include_bytes!("../assets/ca.rsa.4096.crt")` compiled in; `tls_built_in_root_certs(false)` |
| `add_key` IP/SNI separation | ✅ PASS | `.resolve(wg_hostname, addr)` directs connection to `wg_ip:1337` while TLS verifies `wg_hostname` CN |
| Helper root check | ✅ PASS | `libc::geteuid() != 0` → immediate exit in `main()` |
| Private key in memory only | ✅ PASS | `standalone_privkey` field documented "Never persisted to disk" |
| Private key not passed via CLI | ✅ PASS | `privkey` piped to `wg pubkey` via stdin, never via command arg |
| GTK4 calls from non-main thread | ✅ PASS | `standalone_connect/disconnect` called only from `glib::spawn_future_local` (GTK main loop) |

No security defects found.

---

## 3. PIA API Correctness

### 3.1 `add_key` implementation (`src/pia.rs` lines ~255–297)

| Check | Result |
|---|---|
| Connects to `wg_ip:1337` via `.resolve()` | ✅ PASS |
| Uses `wg_hostname` as TLS SNI | ✅ PASS |
| Pins PIA RSA-4096 CA cert (system roots disabled) | ✅ PASS |
| URL format: `https://<wg_hostname>:1337/addKey` | ✅ PASS |
| Query params: `pt=<token>&pubkey=<pubkey>` | ✅ PASS |
| Validates `status == "OK"` in response body | ✅ PASS |
| 30-second timeout | ✅ PASS |

---

## 4. WireGuard Config (`build_wg_config`)

```
[Interface]
Address = {peer_ip}/32
PrivateKey = {privkey}
DNS = {dns}

[Peer]
PublicKey = {server_key}
AllowedIPs = 0.0.0.0/0
Endpoint = {server_ip}:{server_port}
PersistentKeepalive = 25
```

| Check | Result | Notes |
|---|---|---|
| `AllowedIPs` present | ✅ PASS | `0.0.0.0/0` routes all IPv4 through tunnel |
| DNS present | ✅ PASS | First entry from `dns_servers`, fallback to `10.0.0.241` |
| `PersistentKeepalive` present | ✅ PASS | Value 25 s (standard) |
| IPv6 AllowedIPs | ⚠️ LOW | `::/0` absent — IPv6 traffic bypasses the tunnel (split-tunnel for IPv6) |

---

## 5. Error Handling — Surface to UI

| Error Point | Surface to UI? |
|---|---|
| Not authenticated (no token) | ✅ Returns `Err`, shown via `adw::Toast` |
| No region selected | ✅ Returns `Err`, shown via `adw::Toast` |
| Region not in server list | ✅ Returns `Err`, shown via `adw::Toast` |
| Region has no WG servers | ✅ Returns `Err`, shown via `adw::Toast` |
| `wg genkey`/`wg pubkey` failure | ✅ Returns `Err`, shown via `adw::Toast` |
| `add_key` HTTP failure | ✅ Returns `Err`, shown via `adw::Toast` |
| `write_wireguard_config` failure | ✅ Returns `Err`, shown via `adw::Toast` |
| `wg_quick_up` failure | ✅ Returns `Err`, shown via `adw::Toast` |

On error, `ui.rs` resets the button label/icon/CSS class back to disconnected state. ✅

---

## 6. State Management

| Check | Result | Notes |
|---|---|---|
| `standalone_mode` set on connect | ✅ | Set to `true` in step 7 of `standalone_connect` |
| `standalone_privkey` set on connect | ✅ | Set in same write-lock block |
| `standalone_mode` cleared on explicit disconnect | ✅ | Set to `false` in `ui.rs` disconnect handler after `standalone_disconnect()` succeeds |
| `standalone_privkey` cleared on explicit disconnect | ✅ | Set to `None` in `ui.rs` disconnect handler |
| `standalone_mode` cleared on natural disconnect | ✅ | Cleared in `poll_once` when `wg show` reports interface down |
| `standalone_privkey` cleared on natural disconnect | ✅ | Cleared in same `poll_once` Disconnected branch |
| `connection` cleared on disconnect | ✅ | Both paths clear `s.connection = None` |

---

## 7. Token Access — Race Conditions

`standalone_connect` acquires a **read lock**, clones `auth_token`, and releases the lock before any I/O. The token is only written on the GTK main thread (login flow). `standalone_connect` is called from `glib::spawn_future_local` on the same main thread, so there is no concurrent write hazard. **No race condition.** ✅

---

## 8. `poll_once` — Standalone Path

The standalone branch in `poll_once` correctly short-circuits D-Bus queries and instead queries `wg show` for interface status. Handshake interpretation:

| Handshake | Mapped Status |
|---|---|
| `elapsed > 0 && elapsed < 180` | `Connected` |
| `elapsed >= 180` | `Stale(elapsed)` |
| `None` / `0` | `Connecting` |

Interface down → `Disconnected` → auto-clears `standalone_mode` and `standalone_privkey`. ✅  
Normal (NixOS module) mode is unaffected — the `standalone_mode` guard exits the standalone branch only when `true`. ✅

---

## 9. Critical Issues Found

### MEDIUM-1: Stale Watchdog Ignores `standalone_mode` (FUNCTIONAL BUG)

**Location:** `src/state.rs`, `poll_loop`, stale watchdog block (~line 212–222)

**Problem:** When `stale_cycles >= 10` (30 s stale), `poll_loop` unconditionally calls `crate::dbus::restart_vpn_unit()`, even when `standalone_mode == true`. In standalone mode, `pia-vpn.service` does not exist, so this call returns a `NoSuchUnit` D-Bus error every 30 seconds. The error is swallowed by `warn!()` so the app does not crash, but:

- Generates confusing log noise ("Watchdog restart failed: NoSuchUnit")
- Creates an unnecessary D-Bus round-trip every 30 seconds when stale
- Makes the watchdog ineffective in standalone mode (correct behavior would be to call `standalone_disconnect` or re-run `standalone_connect`)

**Required Fix:**
```rust
if s.stale_cycles >= 10 {
    s.stale_cycles = 0;
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
}
```

---

### MEDIUM-2: Cancel-While-Connecting Does Not Handle Standalone Mode (FUNCTIONAL BUG)

**Location:** `src/ui.rs`, connect button handler, `ConnectionStatus::Connecting => { ... }` arm (~line 608–616)

**Problem:** When the user clicks Cancel while status is `Connecting` (which can occur during standalone connect, since `standalone_connect` sets `status = Connecting` before the `wg-quick up` call), the handler unconditionally calls `crate::dbus::disconnect_vpn()`. In standalone mode this tries to stop a non-existent D-Bus unit, fails silently with a `warn!()` and toast, and leaves the WireGuard interface running.

**Required Fix:**
```rust
ConnectionStatus::Connecting => {
    let (standalone, iface) = {
        let s = state.read().await;
        (s.standalone_mode, s.interface.clone())
    };
    if standalone {
        if let Err(e) = crate::state::standalone_disconnect(&iface).await {
            tracing::error!("cancel standalone: {}", e);
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
        toast.add_toast(adw::Toast::new(&format!("Cancel failed: {e:#}")));
    }
}
```

---

## 10. Low-Priority Issues (Non-blocking)

| # | Severity | Location | Issue |
|---|---|---|---|
| L1 | LOW | `src/state.rs:build_wg_config` | IPv6 AllowedIPs (`::/0`) absent; IPv6 traffic bypasses tunnel |
| L2 | LOW | `src/state.rs:build_wg_config` | DNS fallback hardcoded to `10.0.0.241`; not reachable in all regions |
| L3 | LOW | `src/state.rs` | No unit test for `build_wg_config` (it's a pure function, fully testable) |
| L4 | LOW | `src/ui.rs` | `Stale(_)` not included in the `Connected \| KillSwitchActive` disconnect branch (pre-existing, not introduced by this feature) |

---

## 11. Build Validation

All commands executed inside the Nix dev shell from `/home/nimda/Projects/vex-vpn`.

| Step | Command | Exit Code | Result |
|---|---|---|---|
| 1 — Clippy | `nix develop --command cargo clippy -- -D warnings` | 0 | ✅ PASS — zero warnings |
| 2 — Debug build | `nix develop --command cargo build` | 0 | ✅ PASS — compiled in 3.54 s |
| 3 — Test suite | `nix develop --command cargo test` | 0 | ✅ PASS — 28 tests passed (lib + main + helper + integration) |
| 4 — Release build | `nix develop --command cargo build --release` | 0 | ✅ PASS — compiled in 47.28 s (LTO enabled) |
| 5 — Nix build | `nix build` | 0 | ✅ PASS — Crane reproducible build successful |

**Test summary:** 28 total tests — 0 failures.  
Suites run: `vex_vpn` (lib), `vex_vpn` (main binary), `vex_vpn_helper`, `config_integration`, doc-tests.

---

## 12. Architecture & Thread Safety

| Check | Result |
|---|---|
| All `AppState` access via `Arc<RwLock<AppState>>` | ✅ |
| No GTK4 calls from Tokio threads | ✅ |
| `standalone_connect` / `disconnect` on GLib main loop only | ✅ |
| `poll_once` standalone branch uses only `tokio::process::Command` | ✅ |
| `write_wireguard_config` / `wg_quick_up` / `wg_quick_down` in `src/helper.rs` validate input before delegation | ✅ |

---

## 13. Score Table

| Category | Score | Grade |
|---|---|---|
| Specification Compliance | 88% | B+ |
| Best Practices | 90% | A- |
| Functionality | 82% | B |
| Code Quality | 91% | A- |
| Security | 98% | A+ |
| Performance | 92% | A- |
| Consistency | 90% | A- |
| Build Success | 100% | A+ |

**Overall Grade: B+ (91%)**

---

## 14. Summary

### What Works Well

- Complete security model: input validation, atomic writes, 0o600 permissions, CA pinning, SNI override — all correctly implemented.
- `add_key` API integration is correct: direct IP via `resolve()`, SNI via hostname, PIA CA cert, correct query params.
- `build_wg_config` produces a valid wg-quick config with AllowedIPs, DNS, and PersistentKeepalive.
- `poll_once` standalone branch correctly uses `wg show` and completely bypasses the D-Bus/systemd path.
- State cleanup on both explicit disconnect and natural interface-down is correct.
- All 5 build steps pass with zero warnings and 28/28 tests.

### Critical Issues Requiring Refinement

1. **MEDIUM-1** (`src/state.rs` `poll_loop`): Stale watchdog unconditionally calls `restart_vpn_unit()` in standalone mode — logs NoSuchUnit noise every 30 s when stale.
2. **MEDIUM-2** (`src/ui.rs` connect button `Connecting` arm): Cancel-while-connecting calls `disconnect_vpn()` (D-Bus) even in standalone mode — fails silently, leaves WireGuard interface running.

### Verdict

**NEEDS_REFINEMENT** — two MEDIUM functional bugs must be fixed before PASS.
