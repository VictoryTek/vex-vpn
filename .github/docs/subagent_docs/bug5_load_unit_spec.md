# BUG 5 Specification: Replace `get_unit` with `load_unit` in `src/dbus.rs`

**Severity:** Medium  
**File:** `src/dbus.rs`  
**Status:** Specification — Ready for Implementation

---

## 1. Current State Analysis

### 1.1 Full `src/dbus.rs` Proxy Trait (lines 9–19)

```rust
#[dbus_proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    fn get_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}
```

zbus 3.x automatically maps Rust `snake_case` method names to D-Bus `CamelCase` method names:
- `start_unit` → `StartUnit`
- `stop_unit` → `StopUnit`
- `get_unit` → `GetUnit`

### 1.2 All `get_unit` Call Sites

**Total call sites: 1**

| Location | Function | Line | Context |
|----------|----------|------|---------|
| `src/dbus.rs` | `get_service_status` | 51–53 | Getting unit object path to read `ActiveState` |

Full context (lines 45–57):

```rust
pub async fn get_service_status(service: &str) -> Result<String> {
    let conn = system_conn().await?;
    let manager = SystemdManagerProxy::new(&conn)
        .await
        .map_err(anyhow::Error::from)?;

    let unit_path = manager
        .get_unit(service)                                                  // <-- LINE 51
        .await
        .map_err(|e| anyhow::anyhow!("get_unit({}) failed: {}", service, e))?; // <-- LINE 53

    let unit = SystemdUnitProxy::builder(&conn)
        .path(unit_path.as_ref())
        ...
```

`get_unit` is **not** called anywhere else. It is not used by `connect_vpn`, `disconnect_vpn`, `enable_port_forward`, or `disable_port_forward` — those call `start_unit` / `stop_unit` directly (which systemd handles by loading the unit itself internally).

---

## 2. Problem Definition

`GetUnit` on the `org.freedesktop.systemd1.Manager` D-Bus interface only succeeds if the named unit has already been loaded into systemd's in-memory state. From the official systemd man page (`org.freedesktop.systemd1(5)`):

> **GetUnit()** may be used to get the unit object path for a unit name. It takes the unit name and returns the object path. **If a unit has not been loaded yet by this name this method will fail.**

This means that on fresh installs, dev environments, or systems where the WireGuard/PIA services have not yet been activated in the current boot session, `get_service_status("pia-vpn.service")` will fail with a D-Bus error of the form:

```
org.freedesktop.systemd1.NoSuchUnit: Unit pia-vpn.service not found.
```

This causes the VPN status polling in `state.rs` to surface an error instead of returning `"inactive"`, which is the semantically correct result.

---

## 3. D-Bus API Research

### 3.1 Official Method Signatures

From `org.freedesktop.systemd1(5)` (man7.org):

```
GetUnit(in  s name,
        out o unit);

LoadUnit(in  s name,
         out o unit);
```

- **`GetUnit`**: Returns unit object path. **Fails** if the unit has never been loaded into memory.  
- **`LoadUnit`**: Similar to `GetUnit`, but **loads the unit from disk first** if it is not yet in memory.

Both methods have identical signatures:
- **Input**: one string (`s`) — the unit name (e.g., `"pia-vpn.service"`)
- **Output**: one object path (`o`) — the D-Bus object path for the unit

### 3.2 zbus 3.x `#[dbus_proxy]` Macro Syntax

In zbus 3.x, methods declared inside a `#[dbus_proxy]` trait are automatically:
1. Mapped to D-Bus method calls using CamelCase conversion (`load_unit` → `LoadUnit`)
2. Made `async` in the generated `*Proxy` struct
3. The return type `zbus::Result<T>` corresponds to `T` being the output type

Correct declaration for `load_unit`:

```rust
fn load_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
```

This matches the existing pattern for `get_unit` exactly. No additional attributes or annotations are required.

---

## 4. Proposed Solution

### 4.1 Change 1 — Replace `get_unit` declaration with `load_unit` in the proxy trait

**File:** `src/dbus.rs`  
**Lines:** 9–19

**Before:**
```rust
#[dbus_proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    fn get_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}
```

**After:**
```rust
#[dbus_proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait SystemdManager {
    fn start_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    fn stop_unit(&self, name: &str, mode: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
    fn load_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}
```

**Rationale:** `get_unit` is no longer needed anywhere. Replacing it avoids leaving unused dead code in the proxy trait.

---

### 4.2 Change 2 — Update the call site in `get_service_status`

**File:** `src/dbus.rs`  
**Lines:** 51–53

**Before:**
```rust
    let unit_path = manager
        .get_unit(service)
        .await
        .map_err(|e| anyhow::anyhow!("get_unit({}) failed: {}", service, e))?;
```

**After:**
```rust
    let unit_path = manager
        .load_unit(service)
        .await
        .map_err(|e| anyhow::anyhow!("load_unit({}) failed: {}", service, e))?;
```

**Rationale:** `load_unit` loads the unit from disk if needed, so this call will succeed even when the unit has never been activated in the current boot session. The error message string is updated to match the new method name.

---

## 5. Should All `get_unit` Calls Become `load_unit`?

Yes — **all** uses of `get_unit` (there is exactly 1) should become `load_unit`:

- The sole use is in `get_service_status`, which is a read-only status query.
- `LoadUnit` is strictly a superset of `GetUnit`: it succeeds in all cases where `GetUnit` succeeds, plus it handles the "not yet loaded" case gracefully.
- There is no semantic difference for units that are already loaded — `LoadUnit` returns the same object path as `GetUnit` when the unit is already in memory.
- `start_unit` and `stop_unit` do not use `get_unit` at all; systemd internally loads units as needed when starting or stopping them.

---

## 6. Implementation Steps

1. Open `src/dbus.rs`.
2. In the `SystemdManager` trait (lines 15–19), replace `fn get_unit(...)` with `fn load_unit(...)` — same signature, same return type.
3. In `get_service_status` (lines 51–53), replace `.get_unit(service)` with `.load_unit(service)` and update the error message string.
4. No other files require changes.

---

## 7. Dependencies and Compatibility

- **zbus 3.x**: Already a project dependency. The `load_unit` → `LoadUnit` name mapping follows the same automatic CamelCase conversion zbus 3.x applies to all proxy methods. No version bump or new crate required.
- **systemd**: `LoadUnit` has been part of the `org.freedesktop.systemd1.Manager` interface since early systemd versions (well before systemd 220). It is universally available on any NixOS system that this project targets.
- **Existing behavior**: For units that are already loaded in memory, `LoadUnit` returns the same object path as `GetUnit` — no behavior change for the running case. The fix only changes the error case (unit not yet loaded → now succeeds instead of failing).

---

## 8. Risks and Mitigations

| Risk | Severity | Mitigation |
|------|----------|-----------|
| `LoadUnit` may have slightly higher overhead than `GetUnit` when unit is already loaded | Low | Overhead is negligible (single D-Bus round trip either way); the polling loop interval in `state.rs` already has a delay |
| `LoadUnit` triggering unexpected unit activation | None | `LoadUnit` only **loads** the unit file into memory (parses it); it does **not** start the unit. Unit activation requires an explicit `StartUnit` call. |
| Removing `get_unit` from proxy breaks other code | None | Confirmed by grep: `get_unit` appears in exactly 3 lines of `dbus.rs` — the declaration (line 18) and the single call site (lines 51, 53). No other files reference it. |
| zbus CamelCase mapping incorrect | Low | zbus 3.x converts `load_unit` → `LoadUnit` using the same automatic snake_case→CamelCase conversion applied to all other proxy methods (`start_unit`→`StartUnit`, `stop_unit`→`StopUnit`). No custom `name` attribute is needed. |

---

## 9. Summary

- **`get_unit` call sites:** 1 (in `get_service_status`, line 51)
- **All should become `load_unit`:** Yes — there are no cases where the old `GetUnit` behavior is preferable
- **Files to modify:** `src/dbus.rs` only
- **Lines to modify:** Lines 18, 51, 53
- **New dependencies:** None
- **Spec file:** `.github/docs/subagent_docs/bug5_load_unit_spec.md`
