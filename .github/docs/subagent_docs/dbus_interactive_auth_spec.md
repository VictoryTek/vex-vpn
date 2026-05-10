# Spec: D-Bus Interactive Authorization for systemd Unit Control

**Feature:** `dbus_interactive_auth`  
**Phase:** 1 — Research & Specification  
**Date:** 2025  
**Affected file:** `src/dbus.rs`

---

## 1. Problem Statement

When a user clicks Connect in `vex-vpn` (via `nix run github:victorytek/vex-vpn` or in standalone mode without the NixOS module installed), the application fails with:

```
ERROR vex_vpn::ui: connect: start_unit(pia-vpn.service) failed:
org.freedesktop.DBus.Error.InteractiveAuthorizationRequired:
Interactive authentication required.
```

The root cause is that `src/dbus.rs` calls `org.freedesktop.systemd1.Manager.StartUnit` and `StopUnit` over the D-Bus system bus without setting the `ALLOW_INTERACTIVE_AUTHORIZATION` message flag (bit 0x4). When this flag is absent, the D-Bus daemon silently refuses to let polkit prompt the user for credentials; it instead returns an immediate `InteractiveAuthorizationRequired` error.

---

## 2. Current State Analysis

### 2.1 File: `src/dbus.rs` (complete — 150 lines)

The file defines three `#[dbus_proxy]`-generated proxy types:
- `SystemdManagerProxy` — wraps `org.freedesktop.systemd1.Manager`
- `SystemdUnitProxy` — wraps `org.freedesktop.systemd1.Unit`
- `NetworkManagerProxy` — wraps `org.freedesktop.NetworkManager`

The private helper functions responsible for the bug are:

```rust
async fn start_unit(name: &str) -> Result<()> {
    let conn = system_conn().await?;
    let manager = SystemdManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;
    manager
        .start_unit(name, "replace")          // <-- macro-generated call; no flag
        .await
        .map_err(|e| anyhow::anyhow!("start_unit({}) failed: {}", name, e))?;
    Ok(())
}

async fn stop_unit(name: &str) -> Result<()> {
    let conn = system_conn().await?;
    let manager = SystemdManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;
    manager
        .stop_unit(name, "replace")           // <-- macro-generated call; no flag
        .await
        .map_err(|e| anyhow::anyhow!("stop_unit({}) failed: {}", name, e))?;
    Ok(())
}
```

These are the only entry points that write-mutate systemd unit state; they are called by:
- `connect_vpn()` / `disconnect_vpn()` — from the GTK4 button handler in `src/ui.rs`
- `enable_port_forward()` / `disable_port_forward()` — from `src/ui.rs`
- `restart_vpn_unit()` — from the watchdog in `src/state.rs`

### 2.2 File: `src/ui.rs` (error emission location)

```rust
if let Err(e) = crate::dbus::connect_vpn().await {
    tracing::error!("connect: {}", e);  // produces the observed error log
    toast.add_toast(adw::Toast::new(&format!("Connect failed: {e:#}")));
}
```

### 2.3 File: `nix/module-gui.nix` (partial mitigation — NixOS module only)

The NixOS module includes a polkit rule:

```nix
security.polkit.extraConfig = ''
  polkit.addRule(function(action, subject) {
    if (
      action.id == "org.freedesktop.systemd1.manage-units" &&
      action.lookup("unit") in ["pia-vpn.service", "pia-vpn-portforward.service"] &&
      subject.isInGroup("wheel")
    ) { return polkit.Result.YES; }
  });
'';
```

This grants silent `YES` (no password) for `wheel` group users **only when the NixOS module is installed** (`services.vex-vpn.enable = true`). In `nix run` standalone mode, no such rule is active and polkit falls back to `auth_admin` (requires interactive authorization).

### 2.4 Cargo.toml dependency

```toml
zbus = { version = "3", features = ["tokio"] }
```

This pins to the zbus 3.x series. The codebase uses the `#[dbus_proxy]` attribute macro (old zbus 3.x API, not the newer `#[proxy]`).

---

## 3. Research Sources

### Source 1 — D-Bus Specification (freedesktop.org)

URL: `https://dbus.freedesktop.org/doc/dbus-specification.html`

The D-Bus specification defines message flags in §Message Format:

> `ALLOW_INTERACTIVE_AUTHORIZATION | 0x4` — "This flag may be set on a method call message to inform the receiving side that the caller is prepared to wait for interactive authorization... it would be appropriate to query the user for passwords or confirmation via Polkit or a similar framework."

This flag must be set in the message header flags byte (the fourth byte of the fixed header) on the outgoing `METHOD_CALL` message. Without it, polkit returns `InteractiveAuthorizationRequired` immediately.

### Source 2 — zbus 3.15.2 `MethodFlags` enum (docs.rs)

URL: `https://docs.rs/zbus/3.15.2/zbus/enum.MethodFlags.html`

```rust
#[repr(u8)]
pub enum MethodFlags {
    NoReplyExpected = 1,
    NoAutoStart = 2,
    AllowInteractiveAuth = 4,  // ← this is the flag we need
}
```

Key trait: `impl From<MethodFlags> for MessageFlags` — the enum is used with `call_with_flags`.

### Source 3 — zbus 3.15.2 `Proxy::call_with_flags` (docs.rs)

URL: `https://docs.rs/zbus/3.15.2/zbus/struct.Proxy.html#method.call_with_flags`

Exact signature:

```rust
pub async fn call_with_flags<'m, M, B, R>(
    &self,
    method_name: M,
    flags: BitFlags<MethodFlags>,
    body: &B,
) -> Result<Option<R>>
where
    M: TryInto<MemberName<'m>>,
    M::Error: Into<Error>,
    B: Serialize + DynamicType,
    R: DeserializeOwned + Type,
```

Notes:
- `flags` is `BitFlags<MethodFlags>` from the `enumflags2` crate
- Returns `Result<Option<R>>` — `Some(R)` when `NoReplyExpected` is not set
- Use instead of `call()` when extra message flags are needed

### Source 4 — zbus 3.x `ProxyBuilder` source (github.com/z-galaxy/zbus)

URL: `https://github.com/z-galaxy/zbus/blob/main/zbus/src/proxy/builder.rs`

The `Builder` struct fields are:
```rust
conn: Connection,
destination: Option<BusName<'a>>,
path: Option<ObjectPath<'a>>,
interface: Option<InterfaceName<'a>>,
proxy_type: PhantomData<T>,
cache: CacheProperties,
uncached_properties: Vec<String>,
```

**Confirmed**: There is NO `allow_interactive_authorization` field on `ProxyBuilder` in zbus 3.x. The flag cannot be set at the proxy level — it must be set per-call using `call_with_flags`.

### Source 5 — systemd D-Bus API documentation (man7.org)

URL: `https://www.man7.org/linux/man-pages/man5/org.freedesktop.systemd1.5.html`

From the Security section of the Manager Object:

> "PID 1 uses polkit to allow access to privileged operations for unprivileged processes."
>
> "Operations which modify unit state (`StartUnit()`, `StopUnit()`, `KillUnit()`, `RestartUnit()` and similar, `SetProperty()`) require `org.freedesktop.systemd1.manage-units`."

This confirms that `StartUnit` and `StopUnit` are polkit-protected under `org.freedesktop.systemd1.manage-units`. Without the `ALLOW_INTERACTIVE_AUTHORIZATION` D-Bus flag, polkit cannot prompt interactively and returns `InteractiveAuthorizationRequired`.

### Source 6 — vex-vpn codebase analysis (internal)

Full read of:
- `src/dbus.rs` (150 lines, complete)
- `src/ui.rs` (630 lines)
- `src/state.rs` (350 lines)
- `src/bin/helper.rs` (80 lines)
- `nix/module-gui.nix` (200 lines)
- `nix/polkit-vex-vpn.policy` (complete)
- `Cargo.toml` (complete)

Confirms:
- Bug is isolated to `start_unit()` and `stop_unit()` in `src/dbus.rs`
- The `vex-vpn-helper` binary handles only kill switch (nftables) — not relevant
- `polkit-vex-vpn.policy` covers only `org.vex-vpn.helper.run` — not relevant
- All systemd unit operations flow through the two private helpers in `dbus.rs`

---

## 4. Root Cause

`start_unit()` and `stop_unit()` use the `#[dbus_proxy]`-generated `manager.start_unit(...)` / `manager.stop_unit(...)` methods. These macro-generated methods call `Proxy::call()` internally, which sends the D-Bus `METHOD_CALL` message with no flags (flags byte = 0x00). Without `ALLOW_INTERACTIVE_AUTHORIZATION` (0x04) set in the flags byte, the D-Bus daemon tells polkit the client is NOT willing to wait for interactive authorization, so polkit returns the error immediately.

---

## 5. Proposed Solution

### 5.1 Approach

Replace the macro-generated method calls in `start_unit()` and `stop_unit()` with `Proxy::call_with_flags()` using `MethodFlags::AllowInteractiveAuth`. This sets the `ALLOW_INTERACTIVE_AUTHORIZATION` flag (0x04) on the D-Bus message, enabling polkit to either:
- Silently allow (when a polkit rule grants `YES`, as in the NixOS module scenario)
- Present a password prompt (in standalone/nix-run mode for users without a silent grant)

### 5.2 API Access Pattern

The `#[dbus_proxy]` macro in zbus 3.x generates a struct that wraps `Proxy<'a>` and exposes an `inner(&self) -> &Proxy<'_>` method. We use `manager.inner()` to access the underlying `Proxy` directly and call `call_with_flags`.

The `MethodFlags::AllowInteractiveAuth` single variant is converted to `BitFlags<MethodFlags>` via `.into()`, using the `From<MethodFlags> for BitFlags<MethodFlags>` impl provided by `enumflags2`.

### 5.3 Required Import Change

Add to `src/dbus.rs`:
```rust
use zbus::MethodFlags;
```

### 5.4 Implementation

**File:** `src/dbus.rs`

**Change 1 — imports**: Add `MethodFlags` import.

Before:
```rust
use zbus::dbus_proxy;
use zbus::Connection;
```

After:
```rust
use zbus::dbus_proxy;
use zbus::MethodFlags;
use zbus::Connection;
```

**Change 2 — `start_unit` function**: Replace generated method call with `call_with_flags`.

Before:
```rust
async fn start_unit(name: &str) -> Result<()> {
    let conn = system_conn().await?;
    let manager = SystemdManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;
    manager
        .start_unit(name, "replace")
        .await
        .map_err(|e| anyhow::anyhow!("start_unit({}) failed: {}", name, e))?;
    Ok(())
}
```

After:
```rust
async fn start_unit(name: &str) -> Result<()> {
    let conn = system_conn().await?;
    let manager = SystemdManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;
    manager
        .inner()
        .call_with_flags::<_, _, zbus::zvariant::OwnedObjectPath>(
            "StartUnit",
            MethodFlags::AllowInteractiveAuth.into(),
            &(name, "replace"),
        )
        .await
        .map_err(|e| anyhow::anyhow!("start_unit({}) failed: {}", name, e))?;
    Ok(())
}
```

**Change 3 — `stop_unit` function**: Replace generated method call with `call_with_flags`.

Before:
```rust
async fn stop_unit(name: &str) -> Result<()> {
    let conn = system_conn().await?;
    let manager = SystemdManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;
    manager
        .stop_unit(name, "replace")
        .await
        .map_err(|e| anyhow::anyhow!("stop_unit({}) failed: {}", name, e))?;
    Ok(())
}
```

After:
```rust
async fn stop_unit(name: &str) -> Result<()> {
    let conn = system_conn().await?;
    let manager = SystemdManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;
    manager
        .inner()
        .call_with_flags::<_, _, zbus::zvariant::OwnedObjectPath>(
            "StopUnit",
            MethodFlags::AllowInteractiveAuth.into(),
            &(name, "replace"),
        )
        .await
        .map_err(|e| anyhow::anyhow!("stop_unit({}) failed: {}", name, e))?;
    Ok(())
}
```

---

## 6. Implementation Steps

1. Open `src/dbus.rs`
2. Add `use zbus::MethodFlags;` to the imports section (after `use zbus::dbus_proxy;`)
3. Replace the body of `start_unit()` as specified in §5.4 Change 2
4. Replace the body of `stop_unit()` as specified in §5.4 Change 3
5. Verify compilation: `nix develop --command cargo build`
6. Verify clippy clean: `nix develop --command cargo clippy -- -D warnings`

---

## 7. Scope

### What Changes
- **Only** `src/dbus.rs`: two private functions `start_unit` and `stop_unit`, plus one import line

### What Does NOT Change
- `src/ui.rs` — no changes; errors from dbus are already displayed as toast notifications
- `src/state.rs` — no changes; `restart_vpn_unit()` already calls through the private `start_unit`/`stop_unit`
- `src/bin/helper.rs` — unrelated (kill switch only)
- `nix/module-gui.nix` — the existing polkit rule remains correct and beneficial
- `Cargo.toml` — no new dependencies; `MethodFlags` is already in zbus 3.x
- `flake.nix`, `module.nix`, `nix/module-vpn.nix` — no changes

---

## 8. Correctness Notes

### Return Type Handling
`call_with_flags` returns `Result<Option<R>>`. Since `NoReplyExpected` is NOT set, the return is always `Some(OwnedObjectPath)` on success (the systemd job path). We discard this `Option` value — the callers of `start_unit`/`stop_unit` only check for error/success, they do not need the job path.

### Behavioral Difference in `nix run` Mode
After this change, when a user without a silent polkit grant (e.g., `nix run` without NixOS module) clicks Connect:
- polkit will display a password prompt (or use a fingerprint reader, etc.)
- Upon successful authentication, the unit start/stop proceeds normally
- Upon cancellation/failure, the D-Bus call returns an error which becomes the toast notification

### No Impact on NixOS Module Users
When the NixOS module is installed, the existing polkit rule grants silent `YES` for wheel group members. The `AllowInteractiveAuth` flag is harmless in this case — it merely signals willingness to wait; if polkit grants `YES` immediately, no prompt appears.

### Thread Safety
These functions are `async` and called from Tokio tasks. `MethodFlags` is `Copy + Send + Sync`. No thread-safety concerns.

### zbus 3.x Compatibility
`MethodFlags` and `call_with_flags` are stable in zbus 3.x (present since at least 3.14). No version change required.

---

## 9. Risks and Mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `inner()` method name not generated by `#[dbus_proxy]` | Low | The zbus 3.x macro generates `inner()` for all proxy types; also works via `Deref` if needed |
| `MethodFlags::AllowInteractiveAuth.into()` type inference failure | Low | Type is fully constrained by the function parameter type `BitFlags<MethodFlags>` |
| `call_with_flags` not accepting `(&str, &str)` tuple as body | Low | zbus serializes tuples via serde; this is the standard pattern for multi-arg D-Bus calls |
| Polkit prompt appearing unexpectedly for NixOS module users | None | The `YES` rule from the module takes precedence; no prompt appears |
| Watchdog restarts during prompt (user takes too long) | Low | The watchdog calls `restart_vpn_unit()` which would also prompt; acceptable UX trade-off for non-module users |

---

## 10. Files Requiring Modification

| File | Change Type | Description |
|------|------------|-------------|
| `src/dbus.rs` | Edit | Add `MethodFlags` import; replace `start_unit` and `stop_unit` bodies |

No other files require modification.

---

## 11. Summary

**Root cause:** `start_unit()` and `stop_unit()` in `src/dbus.rs` invoke the `#[dbus_proxy]`-generated methods without setting the D-Bus `ALLOW_INTERACTIVE_AUTHORIZATION` (0x04) flag. systemd's polkit integration requires this flag to permit interactive authentication.

**Fix:** Replace the two macro-generated method calls with `Proxy::call_with_flags()` using `MethodFlags::AllowInteractiveAuth`, accessed via `manager.inner()`. This is a 3-line change per function (import + two bodies). No new dependencies, no architectural changes, no impact on callers.

**Spec file path:** `.github/docs/subagent_docs/dbus_interactive_auth_spec.md`
