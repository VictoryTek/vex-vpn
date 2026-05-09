# Bug 7 — Latency Probe Timeout Blocks Poll Loop

**Spec file:** `.github/docs/subagent_docs/bug7_latency_timeout_spec.md`  
**Severity:** Medium  
**Affected file:** `src/state.rs`

---

## 1. Current State Analysis

### 1.1 Poll Loop Structure (`poll_loop`)

```rust
pub async fn poll_loop(state: Arc<RwLock<AppState>>) {
    loop {
        match poll_once(&state).await {
            Ok(()) => {}
            Err(e) => warn!("Poll error: {}", e),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;  // ← 3-second sleep
    }
}
```

The intended cycle time is: `poll_once duration + 3 s sleep`.  
The design assumes `poll_once` completes in a negligible or bounded time.

### 1.2 `poll_once` — Sequential Awaits (as written today)

Every `await` below runs **serially**, one after the other:

| Step | Call | Nature | Typical duration |
|------|------|---------|-----------------|
| 1 | `dbus::get_service_status("pia-vpn.service").await` | D-Bus IPC | ~1–5 ms |
| 2 | `read_region(state_dir).await` | File I/O | ~1 ms |
| 3 | `read_wireguard(state_dir).await` | File I/O | ~1 ms |
| 4 | `read_port_forward(state_dir).await` | File I/O | ~1 ms |
| 5 | `read_wg_stats(&interface).await` | Subprocess (`wg show`) | ~5–20 ms |
| 6 | `check_kill_switch().await` | Subprocess (`nft list`) | ~5–20 ms |
| 7 | `dbus::get_service_status("pia-vpn-portforward.service").await` | D-Bus IPC | ~1–5 ms |
| 8 | `measure_latency(ip).await` *(when connected)* | TCP connect (port 443) | 0–**5000 ms** |

Steps 1–7 are **all independent of each other's results**. They are currently serialised for no reason.  
Step 8 depends only on the result of step 2 (`region.meta_ip`).

### 1.3 Latency Probe — Exact Code

```rust
/// TCP-connect to port 443 of the given IP and return round-trip time in ms.
/// Returns `None` on timeout or connection failure.
async fn measure_latency(ip: &str) -> Option<u32> {
    let addr = format!("{}:443", ip);
    let start = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_millis(5000),          // ← exact timeout: 5 000 ms
        tokio::net::TcpStream::connect(addr.as_str()),
    )
    .await;
    match result {
        Ok(Ok(_)) => Some(start.elapsed().as_millis() as u32),
        _ => None,
    }
}
```

**Exact timeout value:** `Duration::from_millis(5000)` (5 seconds).

### 1.4 Cargo.toml — Tokio Version

```toml
tokio = { version = "1", features = ["full"] }
```

`tokio::join!` is available in Tokio 1.x core — no additional feature flag is required.

---

## 2. Problem Definition

When the VPN is connected and the PIA meta server is unreachable (network glitch,
firewall, server down), `measure_latency` blocks for the full 5-second timeout.

**Effective poll interval in the worst case:**

```
poll_once (sequential, probe times out) ≈ 5000 ms
+ tokio::time::sleep(3 s)              = 3000 ms
─────────────────────────────────────────────────
effective interval                     ≈ 8+ seconds
```

This is 2.7× slower than the intended ~3-second cycle, causing the UI to lag
noticeably when the meta server is unreachable while the VPN tunnel itself remains up.

Additionally, even in the happy path, running 7 independent I/O operations
serially wastes wall-clock time proportional to their sum, when they could all
complete in parallel (dominated by the slowest single one).

---

## 3. Proposed Solution

### 3.1 Strategy: `tokio::join!` + reduced timeout (recommended)

Apply **both** changes together:

1. **Run steps 1–7 concurrently** with `tokio::join!`. All seven futures are
   independent; none reads a result produced by another. This reduces the
   non-probe portion of `poll_once` from `Σ(durations)` to `max(durations)`.

2. **Reduce the latency probe timeout** from 5 000 ms to **2 000 ms**. After the
   join, the probe runs serially (it still depends on `region.meta_ip`), but is
   now guaranteed to finish well within the 3-second sleep window.

**Worst-case poll_once after fix:**
```
tokio::join! (dominated by slowest of 7 ops) ≈ ~20 ms
+ measure_latency (timeout)                  = 2000 ms
─────────────────────────────────────────────────────
poll_once worst case                         ≈ ~2020 ms
```

Effective interval worst case: `2020 ms + 3000 ms ≈ 5 s` — well within acceptable range.

### 3.2 Why Not the Simple One-Liner?

Reducing the timeout alone (5 s → 2 s) fixes the blocking issue but leaves
seven independent I/O operations running serially. This is a missed opportunity
for efficiency and is strictly worse than the combined fix. The `tokio::join!`
refactor carries zero risk (see §4) and is the idiomatic Tokio solution.

---

## 4. Exact Code Changes

### 4.1 Refactor `poll_once` — replace sequential awaits with `tokio::join!`

**Before** (the entire sequential block in `poll_once`, lines ~160–195 of `state.rs`):

```rust
// Query systemd via D-Bus for the service active state.
let new_status = match crate::dbus::get_service_status("pia-vpn.service").await {
    Ok(s) if s == "active" => ConnectionStatus::Connected,
    Ok(s) if s == "activating" => ConnectionStatus::Connecting,
    Ok(s) if s == "failed" => ConnectionStatus::Error("Service failed".to_string()),
    Ok(_) => ConnectionStatus::Disconnected,
    Err(e) => {
        debug!("Could not query service status: {}", e);
        ConnectionStatus::Disconnected
    }
};

let state_dir = "/var/lib/pia-vpn"; // systemd StateDirectory (no DynamicUser)
let region = read_region(state_dir).await.ok();
let wg_info = read_wireguard(state_dir).await.ok();
let forwarded_port = read_port_forward(state_dir).await.unwrap_or(None);
let (rx_bytes, tx_bytes) = read_wg_stats(&interface).await.unwrap_or((0, 0));
let kill_switch_active = check_kill_switch().await.unwrap_or(false);

let pf_active = crate::dbus::get_service_status("pia-vpn-portforward.service")
    .await
    .map(|s| s == "active")
    .unwrap_or(false);
```

**After** (replace the entire block above with):

```rust
let state_dir = "/var/lib/pia-vpn"; // systemd StateDirectory (no DynamicUser)

// Run all independent I/O operations concurrently.
let (
    vpn_status_result,
    region,
    wg_info,
    forwarded_port,
    wg_stats_result,
    kill_switch_result,
    pf_status_result,
) = tokio::join!(
    crate::dbus::get_service_status("pia-vpn.service"),
    read_region(state_dir),
    read_wireguard(state_dir),
    read_port_forward(state_dir),
    read_wg_stats(&interface),
    check_kill_switch(),
    crate::dbus::get_service_status("pia-vpn-portforward.service"),
);

let new_status = match vpn_status_result {
    Ok(s) if s == "active" => ConnectionStatus::Connected,
    Ok(s) if s == "activating" => ConnectionStatus::Connecting,
    Ok(s) if s == "failed" => ConnectionStatus::Error("Service failed".to_string()),
    Ok(_) => ConnectionStatus::Disconnected,
    Err(e) => {
        debug!("Could not query service status: {}", e);
        ConnectionStatus::Disconnected
    }
};
let region = region.ok();
let wg_info = wg_info.ok();
let forwarded_port = forwarded_port.unwrap_or(None);
let (rx_bytes, tx_bytes) = wg_stats_result.unwrap_or((0, 0));
let kill_switch_active = kill_switch_result.unwrap_or(false);
let pf_active = pf_status_result.map(|s| s == "active").unwrap_or(false);
```

The latency probe block that follows is **unchanged** — it already correctly
reads from `region` (which is now resolved from the join result).

### 4.2 Reduce timeout in `measure_latency`

**Before:**
```rust
let result = tokio::time::timeout(
    Duration::from_millis(5000),
    tokio::net::TcpStream::connect(addr.as_str()),
)
.await;
```

**After:**
```rust
let result = tokio::time::timeout(
    Duration::from_millis(2000),
    tokio::net::TcpStream::connect(addr.as_str()),
)
.await;
```

---

## 5. Risks and Mitigations

| Risk | Severity | Mitigation |
|------|----------|------------|
| `tokio::join!` spawns tasks that borrow `state_dir` and `interface` simultaneously | Low | Both are local `String`/`&str` bindings; all futures created inside `poll_once` so lifetimes are fine. No `Arc` sharing required. |
| `check_kill_switch` or `read_wg_stats` can be slow under load | Low | These were already running serially; concurrent execution only improves latency. Any I/O error is already handled via `.unwrap_or`. |
| Reducing timeout from 5 s to 2 s may cause false `None` on high-latency networks | Low | PIA meta servers are always in the same region as the VPN endpoint. A real RTT > 2 s would be a broken connection anyway. |
| Two D-Bus connections opened concurrently | Low | `zbus 3.x` supports concurrent connections; each call opens its own system-bus connection internally. |
| The `tokio::join!` macro is from the `tokio` crate core | None | `tokio = { version = "1", features = ["full"] }` — already present; no additional dependency. |

---

## 6. Implementation Checklist

- [ ] Replace the sequential `await` block in `poll_once` with `tokio::join!` (§4.1)
- [ ] Change `Duration::from_millis(5000)` → `Duration::from_millis(2000)` in `measure_latency` (§4.2)
- [ ] Confirm the latency probe block (the `if new_status.is_connected()` chain) is unchanged
- [ ] Run `nix develop --command cargo clippy -- -D warnings` — must exit 0
- [ ] Run `nix develop --command cargo build` — must exit 0
- [ ] Run `nix develop --command cargo test` — all tests must pass
- [ ] Run `nix develop --command cargo build --release` — must exit 0
- [ ] Run `nix build` — must exit 0

---

## 7. Summary

| Item | Value |
|------|-------|
| Affected file | `src/state.rs` |
| Exact timeout (current) | `Duration::from_millis(5000)` |
| Proposed timeout (new) | `Duration::from_millis(2000)` |
| Concurrent change | `tokio::join!` over 7 independent poll operations |
| Poll sleep | `Duration::from_secs(3)` — **unchanged** |
| Tokio features needed | None (already `features = ["full"]`) |
| Recommended approach | Combined: `tokio::join!` + reduced timeout |
