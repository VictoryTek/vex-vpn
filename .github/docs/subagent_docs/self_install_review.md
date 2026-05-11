# Self-Install Flow — Review & QA

**Feature**: `self_install`
**Phase**: 3 — Review & Quality Assurance
**Date**: 2026-05-10
**Reviewer**: QA Subagent

---

## Build Validation Results

| Step | Command | Exit Code | Result |
|------|---------|-----------|--------|
| Clippy (zero-warning gate) | `nix develop --command cargo clippy -- -D warnings` | 0 | ✅ PASS |
| Debug build | `nix develop --command cargo build` | 0 | ✅ PASS |
| Test suite | `nix develop --command cargo test` | 101 | ❌ FAIL |
| Release build | `nix develop --command cargo build --release` | 0 | ✅ PASS |
| Nix package build | `nix build` | 0 | ✅ PASS |

---

## CRITICAL Issues

### CRITICAL-1 — Test suite fails due to parallel environment variable race condition

**Location**: `src/history.rs` — `test_load_recent_empty` and `test_history_path_respects_xdg_state_home`

**Symptom**: `cargo test` exits 101. One test fails:
```
---- history::tests::test_history_path_respects_xdg_state_home stdout ----
assertion `left == right` failed
  left: "/tmp/nix-shell.dXUE6j/vex_vpn_test_history_empty/vex-vpn/history.jsonl"
 right: "/tmp/test_state/vex-vpn/history.jsonl"
```

**Root cause**: Both history tests mutate the process-global `XDG_STATE_HOME` environment variable without synchronisation. When run in parallel (the Rust default), `test_load_recent_empty` sets `XDG_STATE_HOME` to a Nix-shell temp path; `test_history_path_respects_xdg_state_home` then reads a stale value before it finishes resetting the variable. Running serially (`--test-threads=1`) makes all 19 tests pass.

**Note**: This is a pre-existing bug in `src/history.rs` — that file was **not modified** by the self-install implementation. Nevertheless, per review policy any test failure is CRITICAL and blocks PASS.

**Fix required**: Serialize the two env-mutating tests with a shared `std::sync::Mutex`:

```rust
// In src/history.rs — tests module
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn test_load_recent_empty() {
    let _lock = ENV_LOCK.lock().unwrap();
    // ... existing body unchanged ...
}

#[test]
fn test_history_path_respects_xdg_state_home() {
    let _lock = ENV_LOCK.lock().unwrap();
    std::env::set_var("XDG_STATE_HOME", "/tmp/test_state");
    let path = history_path();
    std::env::remove_var("XDG_STATE_HOME");
    assert_eq!(
        path,
        std::path::PathBuf::from("/tmp/test_state/vex-vpn/history.jsonl")
    );
}
```

No new crate dependency required.

---

## Security Findings

### SEC-1 — Credentials validation is correct and sufficient ✅

`handle_install_backend` validates both `pia_user` and `pia_pass`:
- Non-empty, ≤ 128 bytes, no ASCII control characters (< 0x20, blocking `\n`/`\r`/`\t`)
- Newline-injection into the `credentials.env` env file is therefore impossible
- Systemd `EnvironmentFile=` parsing is not shell-based, so characters like `$`, `#`, backticks are safe in values

### SEC-2 — All written paths are hardcoded ✅

Every path written by `handle_install_backend` and `handle_uninstall_backend` is a compile-time string constant:
- `/etc/vex-vpn/ca.rsa.4096.crt`
- `/etc/vex-vpn/credentials.env`
- `/etc/vex-vpn/pia-connect.sh`
- `/etc/vex-vpn/pia-disconnect.sh`
- `/etc/systemd/system/pia-vpn.service`
- `/etc/polkit-1/actions/org.vex-vpn.helper.policy`

No path component is derived from user input.

### SEC-3 — Credentials written with mode 0o600 via atomic rename ✅

`write_file_atomic` creates the temp file with `mode(0o600)`, writes all bytes, calls `set_permissions(mode)` via `chmod(2)` (umask-independent), then `rename(2)`. The rename is atomic on Linux. Temp file lives in the same directory as the target (`/etc/vex-vpn/`) which we create and own (running as root), so symlink-squatting is not a realistic threat.

### SEC-4 — PIA CA cert embedded in binary ✅

`include_bytes!("../../assets/ca.rsa.4096.crt")` embeds the cert at compile time. No runtime download of the certificate; no TOCTOU. Written with mode 0o644.

### SEC-5 — UninstallBackend stops service before removing files ✅

`handle_uninstall_backend` calls `systemctl stop pia-vpn.service` first (ignoring errors in case the service was already stopped), then removes the unit file and credentials, then runs `daemon-reload`. Correct order.

### SEC-6 — MEDIUM: Polkit policy XML does not escape helper path (low practical risk)

`build_polkit_policy(helper_path)` formats the helper path read from `/proc/self/exe` directly into XML without escaping:

```rust
format!(r#"... <annotate key="org.freedesktop.policykit.exec.path">{}</annotate> ..."#, helper_path)
```

If the path contained `<`, `>`, `&`, `"`, or `'` it would produce malformed or injected XML. In practice, Nix store paths consist only of `/`, alphanumerics, `-`, `.`, and `_`; XML injection is not exploitable. However, the code is technically unsafe. **Recommended fix**: XML-escape the path using a minimal substitution or use `quick-xml` / manual escaping.

### SEC-7 — MEDIUM: Incomplete cleanup on uninstall

`handle_uninstall_backend` removes only:
- `/etc/systemd/system/pia-vpn.service`
- `/etc/vex-vpn/credentials.env`

It leaves on disk:
- `/etc/vex-vpn/pia-connect.sh`
- `/etc/vex-vpn/pia-disconnect.sh`
- `/etc/vex-vpn/ca.rsa.4096.crt`
- `/etc/polkit-1/actions/org.vex-vpn.helper.policy`

The CA cert and scripts are not sensitive. Leaving the polkit policy is mildly concerning (it grants pkexec execution of the helper binary path that was recorded at install time; if that path no longer exists, the policy is inert). But it is not a security vulnerability. **Recommended fix**: Remove all files under `/etc/vex-vpn/` and the polkit policy during uninstall for clean system hygiene.

### SEC-8 — `systemctl` called without absolute path (low risk)

`Command::new("systemctl")` in both install and uninstall functions relies on the PATH provided by pkexec. pkexec sanitises the environment, so this is not exploitable. On NixOS, systemctl is on PATH via `/run/current-system/sw/bin`. On Debian/Ubuntu it is in `/usr/bin`. **Recommended**: use `/usr/bin/systemctl` as a fallback or probe like `nft_binary()` does.

---

## Correctness Findings

### COR-1 — `is_service_unit_installed()` is correct ✅

Uses `LoadUnit` (not `GetUnit`) then reads `load_state`. `LoadUnit` always succeeds; `LoadState == "not-found"` iff no unit file is on disk. The NixOS module case (`LoadState == "loaded"`) is correctly detected as installed.

### COR-2 — Connect script faithfully matches `module-vpn.nix` ✅

Verified by line-by-line comparison against `nix/module-vpn.nix`:

| Feature | module-vpn.nix | self-install script |
|---------|---------------|---------------------|
| PATH setup | via `path = with pkgs; [...]` | `export PATH="..."` at top |
| MAX_LATENCY | compile-time `${toString cfg.maxLatency}` | env var `VEX_VPN_MAX_LATENCY` default 0.1; exported |
| CERT_FILE | `${cfg.certificateFile}` | `/etc/vex-vpn/ca.rsa.4096.crt` |
| Token auth | `curl -u "$PIA_USER:$PIA_PASS"` | identical |
| WireGuard key gen | `wg genkey` / `wg pubkey` | identical |
| netdev/network files | 640, root:systemd-network | identical |
| `networkctl reload && networkctl up` | ✅ | ✅ |
| `systemd-networkd-wait-online` | hard-coded Nix store path | probed at 3 standard locations |
| `ip route add` | `${pkgs.iproute2}/bin/ip` | `ip` via PATH |
| STATE_DIRECTORY | set by systemd from `StateDirectory=` | set by systemd from `StateDirectory=pia-vpn` |

The `MAX_LATENCY` variable is correctly exported for the `xargs` subshell via `export MAX_LATENCY`.

The Nix module's `preUp`/`postUp`/`preDown`/`postDown` hooks are omitted from the embedded scripts — correct, since the self-install flow does not support user-defined hooks.

### COR-3 — EnvironmentFile path is consistent ✅

Unit: `EnvironmentFile=/etc/vex-vpn/credentials.env`
Helper writes to: `/etc/vex-vpn/credentials.env` ✅

### COR-4 — `daemon-reload` runs after writing unit file ✅

Step 8 of `handle_install_backend` calls `systemctl daemon-reload` and blocks on its exit code. Any failure aborts with an error response.

---

## UI Flow Findings

### UI-1 — Startup check is non-blocking ✅

Wrapped in `glib::spawn_future_local` which schedules the async D-Bus call on the GLib main loop without blocking the main thread.

### UI-2 — Connect button correctly disabled before install ✅

`connect_btn_ref.set_sensitive(false)` is called immediately (before the dialog is shown) when `is_service_unit_installed` returns false.

### UI-3 — Connect button re-enabled on successful install ✅

The `Ok(())` arm of the `install_backend` call sets `connect_btn.set_sensitive(true)`. On error, the button stays insensitive (correct — cannot connect without service).

### UI-4 — "Remove" button styled destructively ✅

```rust
.css_classes(["destructive-action"])
```
This applies AdwaiTA's red destructive button styling. ✅

### UI-5 — All async errors surfaced to user ✅

Install failures: `toasts.add_toast(adw::Toast::new(&format!("Install failed: {e:#}")))` ✅  
Uninstall errors: `row.set_subtitle(&format!("Error: {e:#}"))` — surfaced in the prefs row subtitle.

### UI-6 — BUG: Uninstall button not re-enabled on failure

In `ui_prefs.rs` `build_advanced_page`:
```rust
uninstall_btn.connect_clicked(move |btn| {
    btn.set_sensitive(false);  // disabled on click
    glib::spawn_future_local(async move {
        match crate::helper::uninstall_backend().await {
            Ok(()) => { row.set_subtitle("Not installed"); }
            Err(e) => {
                tracing::error!(...);
                row.set_subtitle(&format!("Error: {e:#}"));
                // btn.set_sensitive(true) is MISSING
            }
        }
    });
});
```

After a failed uninstall, the button stays permanently disabled. The user must close and reopen Preferences to retry. **Recommended fix**: call `btn.set_sensitive(true)` in the `Err` arm.

Note: `btn` is moved into the closure but the closure is a `Fn` closure (not `FnOnce`). The button reference needs to be cloned before moving into the async block. Pattern:

```rust
uninstall_btn.connect_clicked(move |btn| {
    btn.set_sensitive(false);
    let row = status_row.clone();
    let btn_ref = btn.clone();
    glib::spawn_future_local(async move {
        match crate::helper::uninstall_backend().await {
            Ok(()) => { row.set_subtitle("Not installed"); }
            Err(e) => {
                tracing::error!("uninstall_backend: {}", e);
                row.set_subtitle(&format!("Error: {e:#}"));
                btn_ref.set_sensitive(true);
            }
        }
    });
});
```

---

## Architecture Compliance

| Check | Result |
|-------|--------|
| No GTK4 calls outside main thread | ✅ All GTK calls inside `glib::spawn_future_local` or synchronous UI code on main thread |
| `Arc<RwLock<AppState>>` used correctly | ✅ No `Mutex` substitution introduced |
| No new production dependencies in Cargo.toml | ✅ `libc` was already present; no new crates added |
| zbus 3.x API only | ✅ Uses `dbus_proxy` macro and `Connection::system().await` |
| Config persistence path unchanged | ✅ `~/.config/vex-vpn/config.toml` via `config_path()` |
| Binary name unchanged | ✅ `[[bin]] name = "vex-vpn"` unchanged |

---

## Score Table

| Category | Score | Grade |
|----------|-------|-------|
| Specification Compliance | 97% | A |
| Best Practices | 82% | B |
| Functionality | 90% | A- |
| Code Quality | 85% | B+ |
| Security | 88% | B+ |
| Performance | 96% | A |
| Consistency | 95% | A |
| Build Success | 60% | D |

**Overall Grade: B (87%) — conditional on test failure being resolved**

> Build Success is scored low solely because `cargo test` exits non-zero. All other build steps pass. The test failure is a pre-existing race condition in `src/history.rs` (not introduced by this feature). Fixing it will raise Build Success to 100% / A and the overall grade to A (93%).

---

## Verdict: NEEDS_REFINEMENT

**Blocking issue**:

1. **CRITICAL-1**: `cargo test` exits 101 — `test_history_path_respects_xdg_state_home` fails intermittently under parallel execution due to unsynchronised `XDG_STATE_HOME` mutation in `src/history.rs`. Fix: add a `static ENV_LOCK: Mutex<()>` and lock it in both env-mutating history tests.

**Recommended (non-blocking) improvements**:

2. **UI-6**: Re-enable uninstall button on error path in `src/ui_prefs.rs`.
3. **SEC-7**: Remove all `/etc/vex-vpn/` files and polkit policy during uninstall.
4. **SEC-6**: XML-escape the helper path in `build_polkit_policy`.
