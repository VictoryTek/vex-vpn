# Bug 2 Fix Specification — Tray Runtime: Replace `current_thread` Runtime with Main Runtime Handle

**Feature Name:** `bug2_tray_runtime`
**Severity:** Critical
**Date:** 2026-05-08
**Files Affected:** `src/tray.rs`, `src/main.rs`

---

## 1. Current State Analysis

### 1.1 `src/tray.rs` — Full Inventory

#### Struct definition (lines 22–26)

```rust
struct PiaTray {
    state: Arc<RwLock<AppState>>,
    rt: tokio::runtime::Runtime,          // ← current_thread runtime
    tx: std::sync::mpsc::SyncSender<TrayMessage>,
}
```

#### `read_state` method (lines 28–30)

```rust
fn read_state(&self) -> AppState {
    self.rt.block_on(async { self.state.read().await.clone() })
}
```

Calls `block_on` on the `current_thread` runtime. This completes in microseconds (just an RwLock acquire). Correct on its own.

#### Menu `activate` callback — connect/disconnect (lines 86–98)

```rust
activate: Box::new(move |tray: &mut PiaTray| {
    if is_connected || is_connecting {
        tray.rt.spawn(async {                          // ← BUG HERE
            if let Err(e) = crate::dbus::disconnect_vpn().await {
                tracing::error!("disconnect failed: {}", e);
            }
        });
    } else {
        tray.rt.spawn(async {                          // ← BUG HERE
            if let Err(e) = crate::dbus::connect_vpn().await {
                tracing::error!("connect failed: {}", e);
            }
        });
    }
}),
```

Two `spawn` calls submit D-Bus futures to the `current_thread` runtime's task queue.

#### `run_tray` function (lines 111–131)

```rust
pub fn run_tray(state: Arc<RwLock<AppState>>, tx: std::sync::mpsc::SyncSender<TrayMessage>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("Failed to create tray runtime: {}", e);
            return;
        }
    };

    let tray = PiaTray { state, rt, tx };

    if let Err(e) = ksni::TrayService::new(tray).run() { ... }
}
```

Creates a `current_thread` Tokio runtime locally and stores it in `PiaTray`. After construction the runtime is handed to ksni — it is never driven again via `Runtime::block_on` or `Runtime::run()` after initial setup.

### 1.2 `src/main.rs` — Tray Call Site (lines 29, 46–49)

```rust
// Line 29 – main runtime creation
let rt = tokio::runtime::Runtime::new()?;    // multi-thread runtime

// Lines 46–49 – tray thread spawning
let state_for_tray = app_state.clone();
std::thread::spawn(move || {
    tray::run_tray(state_for_tray, tray_tx);  // ← no Handle passed
});
```

The main runtime (`Runtime::new()` → `multi_thread`) runs permanently. Its worker threads drive the IO and timer reactors continuously. Its `Handle` is accessible via `rt.handle().clone()`.

### 1.3 `src/dbus.rs` — D-Bus Function Signatures

```rust
pub async fn connect_vpn() -> Result<()>
pub async fn disconnect_vpn() -> Result<()>
```

Both functions call `Connection::system().await` (opens a D-Bus socket) and then perform async D-Bus method calls (`start_unit` / `stop_unit`). They require a live Tokio IO reactor to drive the socket I/O to completion.

### 1.4 `Cargo.toml` — Tokio Version

```toml
tokio = { version = "1", features = ["full"] }
```

Resolved version in the local Cargo registry: **tokio 1.50.0** (confirmed at `~/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.50.0/`).

---

## 2. Problem Definition

### 2.1 Root Cause

`tokio::runtime::Builder::new_current_thread()` creates a **single-threaded, cooperative runtime**. This runtime only makes progress (polls tasks, drives the IO reactor) when one of these is executing:

- `Runtime::block_on(future)` — runs to completion, then stops
- `Runtime::run()` — not called anywhere in the tray code

After `run_tray` constructs `PiaTray` and calls `ksni::TrayService::new(tray).run()`, ksni drives its own D-Bus event loop (via zbus, which uses a separate executor internally). The tray's `current_thread` Tokio runtime is **never polled again** after construction — ksni does not know about it.

The ONLY time the tray runtime's reactor is active is during the brief `block_on` call in `read_state`, which completes in microseconds.

### 2.2 Failure Sequence

1. User clicks "Connect" or "Disconnect" in the tray menu.
2. ksni calls the `activate` closure synchronously.
3. `tray.rt.spawn(async { crate::dbus::connect_vpn().await })` queues the task.
4. `rt.spawn` returns a `JoinHandle`. The task is now pending in the runtime's queue.
5. The `activate` closure returns. The ksni D-Bus event loop continues.
6. The Tokio reactor for `rt` is **never polled again** (no `block_on` or `run` is called).
7. `connect_vpn()` called `Connection::system().await` which opened a socket and suspended waiting for the D-Bus handshake response.
8. The IO reactor will never wake the task. The D-Bus call silently never completes.
9. The VPN does not connect or disconnect. No error is logged.

### 2.3 Why `rt.block_on` in `read_state` Does Not Help

Each `block_on` call:
- Starts the reactor for exactly one future's lifetime
- When that future resolves, the reactor **stops** — any other pending tasks are immediately suspended
- The spawned D-Bus task may have begun executing (e.g., started the D-Bus handshake socket open), but when `block_on` exits its future, the reactor halts and the D-Bus future is abandoned mid-flight

---

## 3. Proposed Solution Architecture

### 3.1 Strategy

Replace the locally-created `current_thread` runtime in `tray.rs` entirely. Instead, accept the main multi-thread runtime's `Handle` and use it for **both** `block_on` reads and `spawn` calls.

**Why `Handle` solves both problems:**

1. **For `spawn`:** `Handle::spawn(future)` submits the task to the main runtime's worker threads. Those worker threads run permanently and continuously drive the IO reactor. The D-Bus socket handshake will be polled to completion.

2. **For `block_on`:** `Handle::block_on(future)` exists in Tokio 1.x (confirmed in source: `tokio-1.50.0/src/runtime/handle.rs`, `pub fn block_on<F: Future>(&self, future: F) -> F::Output`). On a multi-thread runtime, the IO/timer drivers are already running on worker threads, so `Handle::block_on` can safely block the calling thread while the future is driven by the worker pool. `tokio::sync::RwLock` does not use IO or timer drivers — it uses Tokio's waker/parking mechanism — so the limitation "cannot drive IO drivers" does not affect a pure lock read.

3. **`Handle` is `Clone + Send + Sync`:** It is designed to be cloned and shared across threads. Cloning is a reference-count increment (`Arc<Inner>`). Safe to store in `PiaTray` (which is `Send` to be stored in ksni's `TrayService`).

### 3.2 Exact Changes

#### Change 1: `src/tray.rs` — Remove `use` import for `tokio::runtime::Runtime` (if any explicit import)

No explicit `use tokio::runtime::Runtime` import is present. No change needed here.

#### Change 2: `src/tray.rs` — `PiaTray` struct field

**Before:**
```rust
struct PiaTray {
    state: Arc<RwLock<AppState>>,
    rt: tokio::runtime::Runtime,
    tx: std::sync::mpsc::SyncSender<TrayMessage>,
}
```

**After:**
```rust
struct PiaTray {
    state: Arc<RwLock<AppState>>,
    handle: tokio::runtime::Handle,
    tx: std::sync::mpsc::SyncSender<TrayMessage>,
}
```

#### Change 3: `src/tray.rs` — `read_state` method

**Before:**
```rust
fn read_state(&self) -> AppState {
    self.rt.block_on(async { self.state.read().await.clone() })
}
```

**After:**
```rust
fn read_state(&self) -> AppState {
    self.handle.block_on(async { self.state.read().await.clone() })
}
```

`Handle::block_on` blocks the current (ksni event-loop) thread until the RwLock is acquired and state is cloned. The main runtime's worker threads drive the future. This is semantically equivalent to the previous call for a simple lock read.

#### Change 4: `src/tray.rs` — `menu()` activate callback: Connect/Disconnect spawns

**Before:**
```rust
activate: Box::new(move |tray: &mut PiaTray| {
    if is_connected || is_connecting {
        tray.rt.spawn(async {
            if let Err(e) = crate::dbus::disconnect_vpn().await {
                tracing::error!("disconnect failed: {}", e);
            }
        });
    } else {
        tray.rt.spawn(async {
            if let Err(e) = crate::dbus::connect_vpn().await {
                tracing::error!("connect failed: {}", e);
            }
        });
    }
}),
```

**After:**
```rust
activate: Box::new(move |tray: &mut PiaTray| {
    if is_connected || is_connecting {
        tray.handle.spawn(async {
            if let Err(e) = crate::dbus::disconnect_vpn().await {
                tracing::error!("disconnect failed: {}", e);
            }
        });
    } else {
        tray.handle.spawn(async {
            if let Err(e) = crate::dbus::connect_vpn().await {
                tracing::error!("connect failed: {}", e);
            }
        });
    }
}),
```

The task is now submitted to the main multi-thread runtime, which permanently polls all tasks to completion.

#### Change 5: `src/tray.rs` — `run_tray` function

**Before:**
```rust
pub fn run_tray(state: Arc<RwLock<AppState>>, tx: std::sync::mpsc::SyncSender<TrayMessage>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("Failed to create tray runtime: {}", e);
            return;
        }
    };

    let tray = PiaTray { state, rt, tx };

    if let Err(e) = ksni::TrayService::new(tray).run() {
        tracing::warn!(
            "System tray unavailable (may not be supported on this desktop): {}",
            e
        );
    }
}
```

**After:**
```rust
pub fn run_tray(
    state: Arc<RwLock<AppState>>,
    tx: std::sync::mpsc::SyncSender<TrayMessage>,
    handle: tokio::runtime::Handle,
) {
    let tray = PiaTray { state, handle, tx };

    if let Err(e) = ksni::TrayService::new(tray).run() {
        tracing::warn!(
            "System tray unavailable (may not be supported on this desktop): {}",
            e
        );
    }
}
```

The local runtime creation and error-handling block are removed entirely. The `handle` parameter is stored directly in `PiaTray`.

#### Change 6: `src/main.rs` — Obtain handle and pass to tray thread

**Before (lines 46–49):**
```rust
let state_for_tray = app_state.clone();
std::thread::spawn(move || {
    tray::run_tray(state_for_tray, tray_tx);
});
```

**After:**
```rust
let state_for_tray = app_state.clone();
let tray_handle = rt.handle().clone();
std::thread::spawn(move || {
    tray::run_tray(state_for_tray, tray_tx, tray_handle);
});
```

`rt.handle().clone()` is a cheap `Arc` clone. `tray_handle` is `Send`, so moving it into the thread closure is safe.

---

## 4. Complete Replacement Code Blocks

### 4.1 Complete new `src/tray.rs`

```rust
use crate::state::{AppState, ConnectionStatus};
use ksni::Tray;
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Messages sent from the tray thread to the GTK main thread.
// ---------------------------------------------------------------------------

pub enum TrayMessage {
    ShowWindow,
    #[allow(dead_code)]
    Quit,
}

// ---------------------------------------------------------------------------
// The tray runs on its own OS thread. It holds a Handle to the main Tokio
// runtime so that spawned D-Bus tasks are driven by the main runtime's worker
// threads rather than a stranded single-threaded runtime.
// ---------------------------------------------------------------------------

struct PiaTray {
    state: Arc<RwLock<AppState>>,
    handle: tokio::runtime::Handle,
    tx: std::sync::mpsc::SyncSender<TrayMessage>,
}

impl PiaTray {
    fn read_state(&self) -> AppState {
        self.handle.block_on(async { self.state.read().await.clone() })
    }
}

impl Tray for PiaTray {
    fn id(&self) -> String {
        "pia-gui".to_string()
    }

    fn title(&self) -> String {
        let s = self.read_state();
        match &s.status {
            ConnectionStatus::Connected => s
                .region
                .as_ref()
                .map(|r| format!("PIA — {}", r.name))
                .unwrap_or_else(|| "PIA — Connected".to_string()),
            other => format!("PIA — {}", other.label()),
        }
    }

    fn icon_name(&self) -> String {
        let s = self.read_state();
        match s.status {
            ConnectionStatus::Connected => "network-vpn-symbolic",
            ConnectionStatus::Connecting => "network-vpn-acquiring-symbolic",
            ConnectionStatus::KillSwitchActive => "network-vpn-no-route-symbolic",
            _ => "network-vpn-disabled-symbolic",
        }
        .to_string()
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let s = self.read_state();
        let is_connected = s.status.is_connected();
        let is_connecting = matches!(s.status, ConnectionStatus::Connecting);

        vec![
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Open PIA".to_string(),
                activate: Box::new(|tray: &mut PiaTray| {
                    let _ = tray.tx.send(TrayMessage::ShowWindow);
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: if is_connected || is_connecting {
                    "Disconnect".to_string()
                } else {
                    "Connect".to_string()
                },
                activate: Box::new(move |tray: &mut PiaTray| {
                    if is_connected || is_connecting {
                        tray.handle.spawn(async {
                            if let Err(e) = crate::dbus::disconnect_vpn().await {
                                tracing::error!("disconnect failed: {}", e);
                            }
                        });
                    } else {
                        tray.handle.spawn(async {
                            if let Err(e) = crate::dbus::connect_vpn().await {
                                tracing::error!("connect failed: {}", e);
                            }
                        });
                    }
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Quit".to_string(),
                activate: Box::new(|_| {
                    std::process::exit(0);
                }),
                ..Default::default()
            }),
        ]
    }
}

pub fn run_tray(
    state: Arc<RwLock<AppState>>,
    tx: std::sync::mpsc::SyncSender<TrayMessage>,
    handle: tokio::runtime::Handle,
) {
    let tray = PiaTray { state, handle, tx };

    if let Err(e) = ksni::TrayService::new(tray).run() {
        tracing::warn!(
            "System tray unavailable (may not be supported on this desktop): {}",
            e
        );
    }
}
```

### 4.2 Exact diff in `src/main.rs`

Only two lines change in `main.rs`. The existing block:

```rust
    // Spawn system tray on its own thread with its own single-threaded runtime.
    let state_for_tray = app_state.clone();
    std::thread::spawn(move || {
        tray::run_tray(state_for_tray, tray_tx);
    });
```

Becomes:

```rust
    // Spawn system tray on its own thread; pass the main runtime handle so
    // D-Bus tasks are driven by the main worker-thread pool.
    let state_for_tray = app_state.clone();
    let tray_handle = rt.handle().clone();
    std::thread::spawn(move || {
        tray::run_tray(state_for_tray, tray_tx, tray_handle);
    });
```

---

## 5. Dependencies

No new dependencies. `tokio::runtime::Handle` is already available via `tokio = { version = "1", features = ["full"] }`.

**Verified API (tokio 1.50.0 source — confirmed locally):**
- `Runtime::handle(&self) -> &Handle` — returns a reference to the runtime's handle
- `Handle::clone()` — cheap `Arc` clone; `Handle: Clone + Send + Sync`
- `Handle::spawn<F>(future: F) -> JoinHandle<F::Output>` — submits task to runtime's worker pool
- `Handle::block_on<F>(future: F) -> F::Output` — blocks the calling thread, drives future via the runtime's worker pool. **Caveat:** cannot drive IO/timer drivers on a `current_thread` runtime, but on the main `multi_thread` runtime the drivers are already running. `tokio::sync::RwLock` reads do not use IO or timer drivers.

---

## 6. Risks and Mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| `Handle::block_on` panics if called from within a Tokio async context | Low | ksni's `Tray` trait methods (`title`, `icon_name`, `menu`, `activate`) are called synchronously by ksni's D-Bus event loop, not from within an async context. The tray OS thread has no Tokio context active. |
| `Handle::block_on` limitation — cannot drive IO on `current_thread` runtime | N/A | The main runtime is `multi_thread`. IO drivers run on background worker threads. `RwLock` reads don't use IO. Not applicable. |
| Main runtime shutdown before tray shuts down | Low | The main runtime is kept alive in `main.rs` via `let rt = ...` which is not dropped until after `std::process::exit(exit_code.into())` is called. The tray's `Quit` item calls `std::process::exit(0)` directly. Normal app close goes through GTK which calls `std::process::exit`. The handle will not outlive the runtime in normal operation. |
| `PiaTray` must remain `Send` for ksni | Low | `tokio::runtime::Handle` is `Send + Sync`. All existing fields are already `Send`. No regression. |
| D-Bus tasks accumulate if menu is clicked rapidly | Very Low | Each task is independent; `start_unit`/`stop_unit` are idempotent. Existing behaviour, not introduced by this fix. |

---

## 7. Implementation Steps

1. Edit `src/tray.rs`:
   - Replace `rt: tokio::runtime::Runtime` with `handle: tokio::runtime::Handle` in `PiaTray`
   - Update `read_state` to use `self.handle.block_on`
   - Update both `tray.rt.spawn` calls to `tray.handle.spawn`
   - Update `run_tray` signature to remove local runtime creation and accept `handle: tokio::runtime::Handle`
   - Update `PiaTray` constructor to use `handle` field

2. Edit `src/main.rs`:
   - Add `let tray_handle = rt.handle().clone();` before the thread spawn
   - Update the comment on the spawn block
   - Pass `tray_handle` as third argument to `tray::run_tray`

3. Build & validate:
   ```
   nix develop --command cargo clippy -- -D warnings
   nix develop --command cargo build
   nix develop --command cargo test
   nix develop --command cargo build --release
   nix build
   ```
