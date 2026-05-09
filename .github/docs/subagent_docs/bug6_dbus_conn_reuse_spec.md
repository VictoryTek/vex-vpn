# BUG 6 — D-Bus Connection Reuse Specification

**Feature Name:** `bug6_dbus_conn_reuse`  
**Severity:** Medium  
**File:** `src/dbus.rs`  
**Date:** 2026-05-09  

---

## 1. Current State Analysis

### 1.1 The `system_conn()` Function (src/dbus.rs, line 34)

```rust
async fn system_conn() -> Result<Connection> {
    Connection::system().await.map_err(anyhow::Error::from)
}
```

This private async helper opens a **brand new** zbus `Connection` on every invocation.
`Connection::system()` performs a full Unix socket connect + D-Bus authentication
handshake each time it is called.

### 1.2 All Call Sites of `system_conn()`

There are **3 call sites**, all inside `src/dbus.rs`:

| Line | Caller function | How triggered |
|------|-----------------|---------------|
| 45   | `get_service_status(service)` | Called by poll loop AND UI |
| 82   | `start_unit(name)` → via `connect_vpn()` / `enable_port_forward()` | User action |
| 94   | `stop_unit(name)` → via `disconnect_vpn()` / `disable_port_forward()` | User action |

No other files (`src/app.rs`, `src/main.rs`, `src/state.rs`, `src/tray.rs`,
`src/ui.rs`) call `system_conn()` directly — they always go through the public
API functions (`get_service_status`, `connect_vpn`, etc.).

### 1.3 Connection Leak Rate (Poll Loop Analysis)

`src/state.rs` — `poll_once()` (called every 3 seconds from `poll_loop()`):

```rust
// Call 1 — pia-vpn.service status
let new_status = match crate::dbus::get_service_status("pia-vpn.service").await { ... };

// Call 2 — pia-vpn-portforward.service status
let pf_active = crate::dbus::get_service_status("pia-vpn-portforward.service")
    .await
    .map(|s| s == "active")
    .unwrap_or(false);
```

**Result:** 2 fresh D-Bus connections per poll cycle × 20 cycles/minute = **40 connections/minute** at steady state, just from status polling. Each connection involves a Unix socket open, D-Bus SASL authentication (`EXTERNAL` mechanism), and `Hello` method call before any real work can be done.

### 1.4 Async Context Verification

All three call sites are inside `async fn` bodies that run exclusively on the
**Tokio multi-thread runtime** (started in `src/main.rs` with `#[tokio::main]`).
There is no blocking context or separate thread runtime involved at the `dbus.rs`
layer. This confirms that an async-aware `OnceCell` is appropriate and safe.

---

## 2. Problem Definition

Opening a new D-Bus connection on every `system_conn()` call is wasteful:

- Each connection incurs a Unix socket open + D-Bus SASL authentication round-trip.
- At 40 connections/minute continuously, the overhead is measurable and unnecessary.
- D-Bus socket file descriptors are consumed and released rapidly; under pathological
  conditions (e.g. slow auth on a loaded system) this can cause latency spikes in
  the 3-second poll cycle.
- The pattern does not reflect the intended use of `zbus::Connection`, which is
  designed to be created once and shared.

---

## 3. Proposed Solution

### 3.1 Choice of Mechanism

| Option | Usable for async init? | New dep needed? | Verdict |
|--------|----------------------|-----------------|---------|
| `std::sync::OnceLock` | NO — cannot `.await` inside `get_or_init` | No | **Rejected** |
| `once_cell::sync::OnceCell` | NO — same limitation | Yes | **Rejected** |
| `tokio::sync::OnceCell` | YES — `get_or_try_init` is `async` | No (included in `tokio` "full") | **Selected** |

**Rationale:**  
`Connection::system()` is `async fn` in zbus 3.x. The only
correct lazy-init mechanism is one that supports `.await` inside the initializer.
`tokio::sync::OnceCell::get_or_try_init` is exactly this — it awaits the async
initializer and, crucially, if the initializer returns `Err`, the cell remains
unset so the next caller will retry.

`tokio` is already declared in `Cargo.toml` with `features = ["full"]`, which
includes the `sync` feature that gates `tokio::sync::OnceCell`. **No new
dependency is required.**

### 3.2 zbus `Connection` is `Clone`

`zbus::Connection` is cheaply `Clone` — internally it is an `Arc` wrapping the
shared socket state. Cloning yields a new handle to the same underlying
connection; no new socket or authentication handshake occurs. This makes it
safe to store a `Connection` in a `static OnceCell` and hand out `.clone()`s
to callers.

### 3.3 Exact Implementation

Replace the current `system_conn()` function in `src/dbus.rs` with:

```rust
use tokio::sync::OnceCell;

// ---------------------------------------------------------------------------
// Shared D-Bus system connection — initialised once, reused for all calls.
// ---------------------------------------------------------------------------

static SYSTEM_CONN: OnceCell<zbus::Connection> = OnceCell::const_new();

/// Returns the shared system-bus `Connection`, initialising it on first call.
///
/// Uses `tokio::sync::OnceCell::get_or_try_init` so that:
/// - the async `Connection::system()` call can be awaited,
/// - concurrent callers wait for a single initialisation attempt,
/// - a failed attempt leaves the cell empty so the next caller retries.
async fn system_conn() -> Result<zbus::Connection> {
    let conn = SYSTEM_CONN
        .get_or_try_init(|| async {
            Connection::system().await.map_err(anyhow::Error::from)
        })
        .await?;
    Ok(conn.clone())
}
```

**No other changes are required.** The three call sites (`get_service_status`,
`start_unit`, `stop_unit`) already call `system_conn().await?` and receive a
`zbus::Connection` by value — they continue to work unchanged because the
returned value is now a cheap `Clone` of the shared connection rather than a
freshly-opened one.

### 3.4 Required Import Change

The existing imports in `src/dbus.rs`:

```rust
use anyhow::Result;
use tracing::warn;
use zbus::dbus_proxy;
use zbus::Connection;
```

Must be updated to add the `OnceCell` import and remove the now-redundant
direct `Connection` import (since the static uses the fully-qualified path
`zbus::Connection`):

```rust
use anyhow::Result;
use tokio::sync::OnceCell;
use tracing::warn;
use zbus::dbus_proxy;
use zbus::Connection;
```

(`use zbus::Connection` can remain — it is still used by the function signature
and by callers inside the same file.)

### 3.5 Cargo.toml Changes

**None required.** `tokio::sync::OnceCell` is gated behind the `sync` feature
flag, which is already enabled via `features = ["full"]` in `Cargo.toml`:

```toml
tokio = { version = "1", features = ["full"] }
```

`once_cell` is **not** present in `Cargo.toml` and does not need to be added.

---

## 4. Implementation Steps

1. Open `src/dbus.rs`.
2. Add `use tokio::sync::OnceCell;` to the import block.
3. Remove the module-level comment block:
   ```
   // ---------------------------------------------------------------------------
   // Connection helper — per-call for simplicity
   // ---------------------------------------------------------------------------
   ```
4. Replace the old `system_conn` function (lines 34–36) with the new static +
   function shown in §3.3, updating the section comment to:
   ```
   // ---------------------------------------------------------------------------
   // Shared D-Bus system connection
   // ---------------------------------------------------------------------------
   ```
5. No changes to any other file.

---

## 5. Risks and Mitigations

### Risk 1: Stale connection after D-Bus daemon restart

**Scenario:** The `dbus-daemon` (system bus) is restarted while the application
is running. The cached `zbus::Connection` will no longer be valid; subsequent
D-Bus calls will return errors.

**Likelihood:** Very low on NixOS in production — `dbus.service` is a
system-critical unit that is almost never restarted while the desktop session is
active.

**Mitigation:**  
- D-Bus call errors are already propagated naturally (`Result<_>` returns) and
  handled gracefully in `poll_once` via `unwrap_or` / `Err` match arms.
- The `poll_loop` continues on error; the UI shows `ConnectionStatus::Error` or
  `Disconnected`, which is correct behaviour.
- If future resilience is required, the `OnceCell` can be replaced with a
  `RwLock<Option<Connection>>` that resets on error — but this is out of scope
  for this bug fix.

### Risk 2: `OnceCell` does not reset on `get_or_try_init` failure

**Scenario:** `Connection::system()` fails on first call (e.g. D-Bus not yet
started at application startup). The cell remains empty and the next call retries.

**Impact:** This is the **correct** behaviour. The per-call version would also
fail; the new version fails identically but with zero extra connections opened
for subsequent retries once the connection is established.

### Risk 3: Concurrent initialisation

`tokio::sync::OnceCell::get_or_try_init` serialises concurrent callers — if two
async tasks call `system_conn()` simultaneously before the cell is set, only one
runs the initialiser; the other waits. This is safe and correct.

### Risk 4: `!Unpin` / `!Freeze` on `OnceCell`

`tokio::sync::OnceCell<T>` is `!Freeze` (it uses internal mutability). A
`static` of type `OnceCell` is valid because Rust allows interior-mutable
statics provided the type is `Sync`, which `OnceCell<T: Send + Sync>` satisfies.
`zbus::Connection` is `Send + Sync`, so `OnceCell<zbus::Connection>` is `Sync`.
The `static` is therefore sound.

---

## 6. Dependencies

| Crate | Version | Change |
|-------|---------|--------|
| `tokio` | `1` (already in Cargo.toml) | No change — `OnceCell` already available via `features = ["full"]` |
| `zbus` | `3` (already in Cargo.toml) | No change |
| `once_cell` | — | **Not added** — unnecessary |

---

## 7. Testing Notes

- The existing `cargo test` suite (`src/state.rs` unit tests for `decode_port_payload`,
  `format_bytes`) does not cover D-Bus integration and will not be affected.
- Manual validation: run `nix develop --command cargo clippy -- -D warnings` and
  `nix develop --command cargo build`; confirm zero warnings and successful
  compilation.
- Runtime validation: launch the application, observe that the D-Bus system
  connection is opened once (tracing log or `ss -x` showing a single socket
  to `/run/dbus/system_bus_socket`), and that VPN connect/disconnect and status
  polling all continue to function correctly.
