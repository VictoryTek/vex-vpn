# BUG 1 — Wrong State Directory Path: Specification

> Generated: 2026-05-08  
> Severity: **Critical**  
> File: `src/state.rs`

---

## 1. Current State Analysis

### 1.1 The Wrong Path — Exact Location

**File:** `src/state.rs`  
**Line:** 177  

```rust
let state_dir = "/var/lib/private/pia-vpn"; // systemd StateDirectory with DynamicUser
```

This is the **only** occurrence of `/var/lib/private/pia-vpn` in any Rust source file. The string was confirmed by exhaustive `grep` across `src/**/*.rs`. It appears nowhere else in the codebase's Rust sources.

All four downstream read calls on lines 178–180 derive their path from this binding:

```rust
// src/state.rs lines 177–180
let state_dir = "/var/lib/private/pia-vpn"; // systemd StateDirectory with DynamicUser
let region = read_region(state_dir).await.ok();
let wg_info = read_wireguard(state_dir).await.ok();
let forwarded_port = read_port_forward(state_dir).await.unwrap_or(None);
```

Files attempted (all fail because the directory does not exist at this path):

| File | Purpose |
|------|---------|
| `/var/lib/private/pia-vpn/region.json` | Server region name and meta IP |
| `/var/lib/private/pia-vpn/wireguard.json` | WireGuard server IP and peer IP |
| `/var/lib/private/pia-vpn/portforward.json` | Forwarded port (base64-encoded payload) |

### 1.2 NixOS Module Confirmation — DynamicUser Is NOT Set

**File:** `nix/module-vpn.nix`  
**Lines 117–126 (pia-vpn.service serviceConfig block):**

```nix
serviceConfig = {
  Type = "oneshot";
  RemainAfterExit = true;
  Restart = "on-failure";
  EnvironmentFile = cfg.environmentFile;
  CacheDirectory = "pia-vpn";
  StateDirectory = "pia-vpn";
};
```

**Lines 268–274 (pia-vpn-portforward.service serviceConfig block):**

```nix
serviceConfig = {
  Type = "notify";
  Restart = "always";
  CacheDirectory = "pia-vpn";
  StateDirectory = "pia-vpn";
};
```

Neither service block sets `DynamicUser = true`. The `module-gui.nix` file contains no references to `DynamicUser` or the state directory path.

### 1.3 How systemd StateDirectory Behaves Without DynamicUser

As documented in [Lennart Poettering's _Dynamic Users with systemd_](https://0pointer.net/blog/dynamic-users-with-systemd.html) and the [systemd.exec(5) man page](https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html):

> "Let's say `StateDirectory=foobar` is set for a service that has `DynamicUser=` turned **off**. The instant the service is started, `/var/lib/foobar` is created as state directory, owned by the service's user and remains in existence when the service is stopped."

> "If the same service now is run with `DynamicUser=` turned **on**, the implementation is slightly altered. Instead of a directory `/var/lib/foobar` a symbolic link by the same path is created (owned by root), pointing to `/var/lib/private/foobar`."

Mapping to this project:

| Configuration | Resulting Path |
|---------------|----------------|
| `StateDirectory = "pia-vpn"` **without** `DynamicUser` | `/var/lib/pia-vpn` ← **actual path on NixOS** |
| `StateDirectory = "pia-vpn"` **with** `DynamicUser = true` | `/var/lib/private/pia-vpn` (real) + `/var/lib/pia-vpn` (symlink) |

The service scripts in `module-vpn.nix` use `$STATE_DIRECTORY` (the environment variable that systemd sets to the actual resolved path) for **writing** data. This means the systemd service writes to the correct location automatically. The Rust GUI process reads the same data using a **hardcoded path** that points to the DynamicUser location — which does not exist.

---

## 2. Problem Definition

The Rust GUI's poll loop (`poll_once` in `src/state.rs`) reads runtime state written by the `pia-vpn` systemd service. The service correctly writes to `/var/lib/pia-vpn` (via `$STATE_DIRECTORY`). The GUI reads from `/var/lib/private/pia-vpn`, which does not exist because `DynamicUser` is not enabled.

Every file read silently fails:

```rust
let region = read_region(state_dir).await.ok();          // always None
let wg_info = read_wireguard(state_dir).await.ok();      // always None
let forwarded_port = read_port_forward(state_dir)        // always None
    .await.unwrap_or(None);
```

The errors are suppressed by `.ok()` and `.unwrap_or(None)`, so the root cause is invisible in logs unless debug-level tracing is enabled. The application runs but displays stale/empty data permanently.

**Observed symptoms:**
- UI shows "Select a server" even when connected
- Server IP, peer IP, download, upload, latency all display "—" or zero
- Port forwarding port shows "—"
- The VPN may be fully connected at the systemd level, but the UI never reflects this

---

## 3. Proposed Fix

### 3.1 Search/Replace

**File:** `src/state.rs`  
**Change type:** Single-line string literal correction + comment update

**Before (line 177):**
```rust
    let state_dir = "/var/lib/private/pia-vpn"; // systemd StateDirectory with DynamicUser
```

**After:**
```rust
    let state_dir = "/var/lib/pia-vpn"; // systemd StateDirectory (no DynamicUser)
```

The comment is updated to remove the false claim about `DynamicUser` and document the actual configuration.

### 3.2 No Other Changes Required

- No changes are needed in `nix/module-vpn.nix` — it already uses `$STATE_DIRECTORY` correctly via the environment variable.
- No changes are needed in `nix/module-gui.nix`.
- No other Rust source files reference `var/lib`.
- No changes are needed to `Cargo.toml`, `flake.nix`, or any test files.

### 3.3 Verification Steps

After applying the fix, the following should be validated:

1. `nix develop --command cargo clippy -- -D warnings` — must produce zero warnings/errors.
2. `nix develop --command cargo build` — must compile successfully.
3. `nix develop --command cargo test` — all tests must pass (the fix does not affect test coverage since path reads are async I/O, but no test regressions should occur).
4. `nix develop --command cargo build --release` — release build must succeed.
5. `nix build` — Crane-based Nix package build must succeed.
6. Manual runtime test: start `pia-vpn.service`, then launch `pia-gui`; UI should display the region name, IP, and stats within the 3-second poll interval.

---

## 4. Risks and Mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| A future maintainer re-enables `DynamicUser` in `module-vpn.nix`, breaking the path again | Low | The updated comment explicitly documents the dependency; if `DynamicUser` is ever added, `/var/lib/pia-vpn` remains accessible as a symlink (systemd creates it automatically), so the path `/var/lib/pia-vpn` remains valid in both configurations. |
| The pia-gui process runs as a user that lacks read permission on `/var/lib/pia-vpn` | Low | The NixOS module does not restrict the directory to a specific user; the default mode for `StateDirectory` is 0700 owned by root if no `User=` is set, but since the pia-vpn service runs as root (no `User=` directive), files inside are readable by root. The GUI must run as root or the directory permissions must be relaxed. This is a pre-existing constraint, not introduced by this fix. |
| Path is duplicated elsewhere in the future | Low | If the state directory path is ever needed in more than one place, it should be extracted to a constant (`const STATE_DIR: &str = "/var/lib/pia-vpn";`) to prevent drift. The current single-occurrence fix is minimal and sufficient. |

---

## 5. References

1. Lennart Poettering, _Dynamic Users with systemd_ (2017): https://0pointer.net/blog/dynamic-users-with-systemd.html  
   — Authoritative explanation of how `StateDirectory` behaves with and without `DynamicUser`.

2. systemd.exec(5) — `StateDirectory=` documentation: https://www.freedesktop.org/software/systemd/man/latest/systemd.exec.html  
   — Specifies path resolution and `$STATE_DIRECTORY` environment variable.

3. NixOS `systemd.services.<name>.serviceConfig` options: https://nixos.org/manual/nixos/stable/options#opt-systemd.services._name_.serviceConfig  
   — NixOS pass-through of systemd unit `[Service]` fields.

4. `nix/module-vpn.nix` (this repo) — confirms `StateDirectory = "pia-vpn"` with no `DynamicUser`.

5. `src/state.rs` (this repo) — sole location of the incorrect hardcoded path.

6. systemd Users/Groups/UIDs documentation: https://systemd.io/UIDS-GIDS/  
   — Confirms dynamic UID range and the conditions under which `/var/lib/private/` is used.
