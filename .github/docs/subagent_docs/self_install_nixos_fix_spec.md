# Spec: Self-Install NixOS Path Fix

**Feature name:** `self_install_nixos_fix`
**Date:** 2026-05-10
**Status:** READY FOR IMPLEMENTATION

---

## 1. Problem Statement

The self-install helper (`src/bin/helper.rs`) writes VPN backend files to two
locations that are **read-only on NixOS**:

| Write attempt | Path | Error |
|---|---|---|
| Persistent scripts/config | `/etc/vex-vpn/` | Would succeed (NixOS does not manage this dir), but is FHS-incorrect for scripts |
| systemd unit file | `/etc/systemd/system/pia-vpn.service` | **EROFS — os error 30** (NixOS activation owns this directory) |
| polkit policy (next step) | `/etc/polkit-1/actions/org.vex-vpn.helper.policy` | Would also fail EROFS on NixOS |

The observable error is:

```
ERROR vex_vpn::ui: install_backend: helper error: write pia-vpn.service: Read-only file system (os error 30)
```

### Root cause

On NixOS, `/etc/systemd/system/` is populated exclusively by the NixOS
activation script (from the Nix store) and is made effectively immutable at
runtime. No process — even root — can write new files there outside of a
system rebuild. Similarly, `/etc/polkit-1/actions/` is NixOS-managed.

### Why `/run/systemd/system/` solves it

`/run/` is a tmpfs mounted by systemd-tmpfiles at boot. It is always writable
by root. `/run/systemd/system/` is explicitly monitored by systemd for runtime
unit additions — a `daemon-reload` after writing there is sufficient to
register the unit. This mechanism works on **both NixOS and standard FHS
Linux** (Ubuntu, Fedora, Arch, etc.).

The only trade-off: `/run/` is cleared on reboot, so the unit file must be
re-written when the machine reboots and the app starts again.

---

## 2. Research Summary

### 2.1 `/run/systemd/system/` on NixOS

- systemd scans `/run/systemd/system/` continuously (inotify). Units written
  there become visible immediately after `systemctl daemon-reload`.
- `daemon-reload` is sufficient — `daemon-reexec` is NOT needed for this.
- On NixOS, NixOS does NOT clear `/run/systemd/system/` between boots — the
  entire `/run/` tmpfs is re-created fresh at boot. This means the unit file
  disappears on every reboot.
- `StateDirectory=pia-vpn` in the unit tells systemd to create
  `/var/lib/pia-vpn/` for the service's own runtime state. This is **separate**
  from `/var/lib/vex-vpn/` (the installer's directory).

### 2.2 Persistent data: `/var/lib/vex-vpn/`

`/var/lib/` is the standard FHS location for service variable data. On NixOS it
is writable by root and survives reboots. It is the correct location for the
CA certificate, credentials, and connect/disconnect scripts that must outlive
reboots.

Note: the VPN service already uses `StateDirectory=pia-vpn` which maps to
`/var/lib/pia-vpn/` — that is systemd-managed state for the VPN daemon itself.
Our installer-owned directory must be separate: `/var/lib/vex-vpn/`.

### 2.3 Reboot re-registration strategy

After a reboot, `/run/systemd/system/pia-vpn.service` is gone but
`/var/lib/vex-vpn/pia-connect.sh` still exists. The app must detect this and
re-register the unit without requiring the user to re-enter credentials.

**Chosen strategy:** at `main.rs` startup, before spawning the poll loop,
check:

```
if /var/lib/vex-vpn/pia-connect.sh exists AND pia-vpn.service is not-found in systemd
    → call helper's new `reinstall_unit` op (pkexec, one-time auth)
```

This is clean, explicit, and keeps `dbus.rs` free of side-effectful file
operations. The pkexec prompt appears at most once per reboot, and only when
the VPN was previously installed.

### 2.4 Why not `StartTransientUnit` (D-Bus transient units)?

`org.freedesktop.systemd1.Manager.StartTransientUnit` can create a oneshot
service without writing any file, but it has significant limitations:

- `ExecStop` is not supported in transient units (systemd restriction).
- `EnvironmentFile=` is not supported in transient units.
- `ConditionFileNotEmpty=` is not supported.
- `RemainAfterExit=yes` is supported but the stop hook cannot run the disconnect
  script.

Verdict: not suitable for the pia-vpn service, which requires `ExecStop` for
clean WireGuard teardown.

### 2.5 Polkit policy path

On NixOS, `/etc/polkit-1/actions/` is read-only. The polkit write step in
`handle_install_backend()` would fail after the unit file fix is applied.

Resolution: Make the polkit step **non-fatal** with the following fallback order:

1. Try `/etc/polkit-1/actions/org.vex-vpn.helper.policy`
2. On EROFS/permission error, try `/run/polkit-1/actions/org.vex-vpn.helper.policy`
   (polkit 0.120+ on some distributions scans here at runtime)
3. If both fail AND `/run/current-system/sw/share/polkit-1/actions/` contains
   the file (NixOS module installed it), log a warning and continue — the
   action is already registered.
4. If both fail AND no existing policy found, return an error (pkexec will fail
   without a policy).

On NixOS with the NixOS module installed, the polkit policy is installed
automatically at `/run/current-system/sw/share/polkit-1/actions/` and no write
is needed. On non-NixOS, writing to `/etc/polkit-1/actions/` succeeds as today.

---

## 3. New Path Mapping

### 3.1 File-by-file mapping

| Purpose | Old path | New path | Persistence |
|---|---|---|---|
| Installer data directory | `/etc/vex-vpn/` | `/var/lib/vex-vpn/` | Persistent (survives reboot) |
| PIA CA certificate | `/etc/vex-vpn/ca.rsa.4096.crt` | `/var/lib/vex-vpn/ca.rsa.4096.crt` | Persistent |
| Credentials env file | `/etc/vex-vpn/credentials.env` | `/var/lib/vex-vpn/credentials.env` | Persistent |
| Connect script | `/etc/vex-vpn/pia-connect.sh` | `/var/lib/vex-vpn/pia-connect.sh` | Persistent |
| Disconnect script | `/etc/vex-vpn/pia-disconnect.sh` | `/var/lib/vex-vpn/pia-disconnect.sh` | Persistent |
| systemd unit file | `/etc/systemd/system/pia-vpn.service` | `/run/systemd/system/pia-vpn.service` | **Volatile** (cleared on reboot) |
| polkit policy | `/etc/polkit-1/actions/org.vex-vpn.helper.policy` | Try `/etc/polkit-1/actions/` then `/run/polkit-1/actions/`; skip if NixOS module already installed it | Context-dependent |

### 3.2 Existing VPN service state (unchanged)

`StateDirectory=pia-vpn` in the unit continues to resolve to `/var/lib/pia-vpn/`
(systemd-managed). This is untouched by this fix.

---

## 4. Updated Embedded Constants in `src/bin/helper.rs`

### 4.1 `SERVICE_UNIT`

Replace every `/etc/vex-vpn/` reference with `/var/lib/vex-vpn/`:

```ini
[Unit]
Description=Connect to Private Internet Access VPN (WireGuard)
Requires=network-online.target
After=network.target network-online.target
ConditionFileNotEmpty=/var/lib/vex-vpn/ca.rsa.4096.crt
ConditionFileNotEmpty=/var/lib/vex-vpn/credentials.env

[Service]
Type=oneshot
RemainAfterExit=yes
Restart=on-failure
EnvironmentFile=/var/lib/vex-vpn/credentials.env
StateDirectory=pia-vpn
CacheDirectory=pia-vpn
ExecStart=/var/lib/vex-vpn/pia-connect.sh
ExecStop=/var/lib/vex-vpn/pia-disconnect.sh

[Install]
WantedBy=multi-user.target
```

### 4.2 `CONNECT_SCRIPT`

Change the `CERT_FILE` assignment:

```bash
# Old
CERT_FILE="/etc/vex-vpn/ca.rsa.4096.crt"

# New
CERT_FILE="/var/lib/vex-vpn/ca.rsa.4096.crt"
```

No other path changes needed in `CONNECT_SCRIPT` or `DISCONNECT_SCRIPT`
(the other paths they write to — `/run/systemd/network/` — are already correct).

---

## 5. Implementation Steps

### 5.1 `src/bin/helper.rs` — `handle_install_backend()`

**Step 1 — Data directory:** Change `create_dir_all("/etc/vex-vpn")` →
`create_dir_all("/var/lib/vex-vpn")`.
Set permissions 0o755 on `/var/lib/vex-vpn/`.

**Steps 2–5 — Data files:** Change every path from `/etc/vex-vpn/X` →
`/var/lib/vex-vpn/X`. Permissions unchanged (CA cert: 0o644, credentials:
0o600, scripts: 0o755).

**Step 6 — Unit file:**
1. `create_dir_all("/run/systemd/system")` (may not exist on early-boot).
2. Change unit path from `/etc/systemd/system/pia-vpn.service` →
   `/run/systemd/system/pia-vpn.service`.
3. `write_file_atomic(unit_path, SERVICE_UNIT.as_bytes(), 0o644)`.

**Step 7 — Polkit policy:** Apply non-fatal fallback logic:

```rust
let policy_written = try_write_polkit_policy(&policy_content);
if !policy_written {
    // Check if NixOS module already registered the action.
    if !polkit_action_exists("org.vex-vpn.helper.run") {
        return Response {
            ok: false,
            error: Some("polkit policy could not be written and action not found".into()),
            active: None,
        };
    }
    // NixOS module covers it — log and continue.
    eprintln!("polkit write skipped: action already registered by NixOS module");
}
```

Where `try_write_polkit_policy()` tries `/etc/polkit-1/actions/` then
`/run/polkit-1/actions/`, returns `true` on success.

`polkit_action_exists()` checks that
`/run/current-system/sw/share/polkit-1/actions/org.vex-vpn.helper.policy`
exists (NixOS module path).

**Step 8 — daemon-reload:** Unchanged.

### 5.2 `src/bin/helper.rs` — `handle_uninstall_backend()`

Update all removed paths to match new locations. Add removal of the new files
that were not previously removed:

```rust
// Stop service (ignore errors).
let _ = systemctl(&["stop", "pia-vpn.service"]);

// Remove volatile unit file.
let _ = std::fs::remove_file("/run/systemd/system/pia-vpn.service");

// Remove persistent installer data.
let _ = std::fs::remove_file("/var/lib/vex-vpn/credentials.env");
let _ = std::fs::remove_file("/var/lib/vex-vpn/pia-connect.sh");
let _ = std::fs::remove_file("/var/lib/vex-vpn/pia-disconnect.sh");
let _ = std::fs::remove_file("/var/lib/vex-vpn/ca.rsa.4096.crt");
// Remove directory only if empty (leaves it if user added files).
let _ = std::fs::remove_dir("/var/lib/vex-vpn");

// Run daemon-reload.
// ...unchanged error handling...
```

Do NOT remove `/var/lib/pia-vpn/` — that is systemd-managed VPN state, not
the installer's data.

Also remove the old polkit policy if still present:

```rust
let _ = std::fs::remove_file(
    "/etc/polkit-1/actions/org.vex-vpn.helper.policy"
);
let _ = std::fs::remove_file(
    "/run/polkit-1/actions/org.vex-vpn.helper.policy"
);
```

### 5.3 `src/bin/helper.rs` — New `ReinstallUnit` command

Add to the `Command` enum:

```rust
ReinstallUnit,
```

Add to `handle_command()`:

```rust
Command::ReinstallUnit => handle_reinstall_unit(),
```

Implement:

```rust
fn handle_reinstall_unit() -> Response {
    // Only reinstall if the data dir is present (we previously installed).
    if !std::path::Path::new("/var/lib/vex-vpn/pia-connect.sh").exists() {
        return Response {
            ok: false,
            error: Some("no prior installation found at /var/lib/vex-vpn/".into()),
            active: None,
        };
    }

    if let Err(e) = std::fs::create_dir_all("/run/systemd/system") {
        return Response {
            ok: false,
            error: Some(format!("create /run/systemd/system: {}", e)),
            active: None,
        };
    }

    let unit_path = std::path::Path::new("/run/systemd/system/pia-vpn.service");
    if let Err(e) = write_file_atomic(unit_path, SERVICE_UNIT.as_bytes(), 0o644) {
        return Response {
            ok: false,
            error: Some(format!("reinstall unit file: {}", e)),
            active: None,
        };
    }

    let status = std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => Response { ok: true, error: None, active: None },
        Ok(s) => Response {
            ok: false,
            error: Some(format!("daemon-reload failed: exit {:?}", s.code())),
            active: None,
        },
        Err(e) => Response {
            ok: false,
            error: Some(format!("daemon-reload: {}", e)),
            active: None,
        },
    }
}
```

### 5.4 `src/helper.rs` — Add `reinstall_unit()`

Add a new public async function alongside `install_backend` and
`uninstall_backend`:

```rust
/// Re-register the pia-vpn.service unit to /run/systemd/system/ after a
/// reboot erased the volatile unit file. Only succeeds if
/// /var/lib/vex-vpn/pia-connect.sh exists (prior install detected).
pub async fn reinstall_unit() -> Result<()> {
    let resp = call_helper(&HelperRequest {
        op: "reinstall_unit",
        interface: None,
        allowed_interfaces: None,
        pia_user: None,
        pia_pass: None,
    })
    .await?;
    if resp.ok {
        Ok(())
    } else {
        bail!("helper error: {}", resp.error.unwrap_or_default())
    }
}
```

Also update the `install_backend` doc comment to reference `/var/lib/vex-vpn/`
instead of `/etc/vex-vpn/`.

### 5.5 `src/main.rs` — Startup re-registration

Immediately before the `rt.spawn` for the poll loop, add a blocking check and
conditional reinstall. This must run before the poll loop starts, so the unit
is registered by the time `is_service_unit_installed()` is first called.

```rust
// Re-register the volatile unit file if a prior install is detected and
// the unit is not currently loaded (typical after a system reboot).
let state_for_poll = app_state.clone();
let poll_tx = state_change_tx.clone();
rt.spawn(async move {
    let data_installed =
        std::path::Path::new("/var/lib/vex-vpn/pia-connect.sh").exists();
    if data_installed && !crate::dbus::is_service_unit_installed("pia-vpn.service").await {
        info!("Detected stale install (reboot): re-registering pia-vpn.service");
        if let Err(e) = crate::helper::reinstall_unit().await {
            warn!("Auto-reinstall unit failed: {e:#}");
        }
    }
    state::poll_loop(state_for_poll, poll_tx).await;
});
```

> **Architecture note:** This deliberately does NOT embed the reinstall call
> inside `is_service_unit_installed()`. That function is called on every poll
> cycle (every 3 seconds). Embedding a pkexec-spawning side effect there would
> cause repeated auth prompts on any sustained "not-found" state. The startup
> one-shot approach is correct.

---

## 6. Helper `HelperRequest` serialisation

The new `reinstall_unit` op uses all-`None` optional fields, exactly like
`uninstall_backend`. No changes to the `HelperRequest` struct are needed —
`op: "reinstall_unit"` is sufficient.

---

## 7. Files to Modify

| File | Change summary |
|---|---|
| `src/bin/helper.rs` | New paths in constants, `handle_install_backend`, `handle_uninstall_backend`; new `ReinstallUnit` variant and `handle_reinstall_unit` |
| `src/helper.rs` | Add `reinstall_unit()` async function; update doc comment on `install_backend` |
| `src/main.rs` | Add startup re-registration block before poll loop spawn |

`src/dbus.rs` — **no changes required** (`is_service_unit_installed` stays a
pure D-Bus query; re-registration is handled at startup in `main.rs`).

`nix/module-vpn.nix` — **no changes required** (the NixOS module manages its
own paths independently of the self-install helper).

---

## 8. Risks and Mitigations

### R1 — `/var/lib/` on a `noexec` filesystem

**Risk:** Some hardened systems mount `/var/` with `noexec`, preventing
`/var/lib/vex-vpn/pia-connect.sh` from being executed directly by systemd.

**Mitigation:** systemd executes `ExecStart=` scripts via `/bin/sh -c` when
the path does not end with a non-script binary. However, since the unit uses
`ExecStart=/var/lib/vex-vpn/pia-connect.sh` directly, it would fail on
`noexec` `/var`.

**Recommended workaround (future):** Change `ExecStart` to
`ExecStart=/bin/bash /var/lib/vex-vpn/pia-connect.sh` — the script is passed
as an argument to bash which is on `/bin/` (exec mount), so `noexec` on
`/var/` does not apply. This is a safe hardening improvement. Include in this
fix as a low-risk improvement (no behaviour change on standard systems).

### R2 — `/run/systemd/system/` does not exist at install time

**Risk:** On some embedded/minimal systemd configurations, `/run/systemd/system/`
may not exist until systemd creates it.

**Mitigation:** `create_dir_all("/run/systemd/system")` is called before writing
the unit file. On any systemd system this directory is created by systemd very
early (PID 1 startup), so in practice the `create_dir_all` is a no-op. Still
required defensively.

### R3 — polkit policy not found after non-fatal skip

**Risk:** If polkit write fails AND the NixOS module is not installed, `pkexec`
will fail for all future helper invocations.

**Mitigation:** The non-fatal polkit path explicitly checks for an existing
registered action before silently continuing. If neither fallback write path
works AND no registered action exists, the install fails with a clear error
message. This preserves the existing safety guarantee.

### R4 — Re-registration pkexec prompt at startup

**Risk:** After a reboot, when the app starts, a polkit authentication dialog
appears unexpectedly. Users might find this confusing.

**Mitigation:** The prompt appears once per reboot, only when VPN was previously
installed. The polkit message string ("Authentication is required to control the
VPN kill switch") should be updated to "Authentication is required to restore
the VPN backend after reboot" to clarify the intent. This requires updating the
polkit action `<message>` — out of scope for this fix, but noted.

### R5 — Stale credentials left in `/var/lib/vex-vpn/` after manual uninstall

**Risk:** If a user manually removes `/run/systemd/system/pia-vpn.service`
without going through the uninstall UI, credentials remain in
`/var/lib/vex-vpn/credentials.env`. On next app launch, re-registration runs
automatically, restoring the unit.

**Mitigation:** This is expected behaviour (the data dir IS the install state).
If the user wants to fully uninstall, they should use the UI. The uninstall
handler removes all data files including credentials. Document this behaviour
in the README.

---

## 9. Testing Approach

1. **Standard Linux (non-NixOS):** Install → confirm files at `/var/lib/vex-vpn/`
   and unit at `/run/systemd/system/pia-vpn.service`. `systemctl status pia-vpn`
   should show the unit loaded.

2. **NixOS (module not installed):** Same as above. No EROFS errors.

3. **NixOS (module installed):** Install should succeed. Both the module-installed
   unit and the self-install unit coexist without conflict (the module's unit
   lives in `/nix/store/.../systemd/system/`, not `/run/systemd/system/`).

4. **Reboot scenario:** After install + reboot, app startup should trigger
   auto-reinstall. Verify `pia-vpn.service` is registered post-launch.

5. **Uninstall:** Verify `/var/lib/vex-vpn/` is removed and
   `/run/systemd/system/pia-vpn.service` is removed. Re-registration on next
   launch must NOT occur (no `pia-connect.sh` present).

---

## 10. Out of Scope

- Changing the NixOS module (`nix/module-vpn.nix`) — it manages its own unit
  separately.
- Migrating existing installs from `/etc/vex-vpn/` to `/var/lib/vex-vpn/` —
  documented as a breaking change; users with existing installs should
  uninstall and reinstall.
- Port-forward service (`pia-vpn-portforward.service`) — not written by the
  self-installer; managed entirely by the NixOS module.

---

*Spec written by: Research Subagent — vex-vpn self-install NixOS fix*
