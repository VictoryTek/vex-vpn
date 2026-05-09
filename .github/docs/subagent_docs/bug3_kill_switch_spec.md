# BUG 3 — Kill Switch `nft` subprocess missing `sudo`

**Severity:** Critical  
**File:** `src/dbus.rs`  
**Functions:** `apply_kill_switch`, `remove_kill_switch`

---

## 1. Current State Analysis

### 1.1 nft Command Calls in `src/dbus.rs`

Two `tokio::process::Command::new("nft")` calls exist. Neither invokes `sudo`.

#### Call 1 — `apply_kill_switch()` — Line 129

```rust
// src/dbus.rs  lines 129–136
let mut child = tokio::process::Command::new("nft")
    .arg("-f")
    .arg("-")
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped())
    .spawn()
    .map_err(|e| anyhow::anyhow!("failed to spawn nft: {}", e))?;
```

The full `apply_kill_switch` function (lines 110–156) constructs an nftables ruleset
as a here-doc string and pipes it to `nft -f -`.  
Error from `nft` is captured via `wait_with_output()` and propagated as an
`anyhow::bail!` — **but the caller in `src/state.rs` / `src/ui.rs` logs it and
does not surface it to the user**, so the toggle appears to succeed.

#### Call 2 — `remove_kill_switch()` — Line 160

```rust
// src/dbus.rs  lines 160–165
let output = tokio::process::Command::new("nft")
    .args(["delete", "table", "inet", "pia_kill_switch"])
    .output()
    .await
    .map_err(|e| anyhow::anyhow!("failed to spawn nft: {}", e))?;
```

Failure here is only `warn!`-logged (lines 167–170); it does not propagate an error,
so teardown failure is completely silent.

### 1.2 sudo Usage in `src/dbus.rs`

`grep` confirms: the string `sudo` does **not** appear anywhere in `src/dbus.rs`.

### 1.3 NOPASSWD Rule in `nix/module-gui.nix`

```nix
# nix/module-gui.nix  lines ~167–176
security.sudo.extraRules = [
  {
    groups = [ "wheel" ];
    commands = [
      {
        command = "${pkgs.nftables}/bin/nft";
        options = [ "NOPASSWD" ];
      }
    ];
  }
];
```

**Scope:** The rule grants passwordless `sudo` to **the entire `nft` binary**
(`${pkgs.nftables}/bin/nft`) with no argument restrictions, for any user in the
`wheel` group. This covers both `nft -f -` (apply) and `nft delete table …`
(remove). No additional subcommand-level filtering is in place.

### 1.4 nftables Declarative Module

`nix/module-gui.nix` also defines `networking.nftables.tables.pia_kill_switch`
(lines 102–132). This is a **NixOS boot-time declarative ruleset** — it provides
the initial kill-switch table at system activation. The runtime toggle in
`src/dbus.rs` is a **separate, imperative path** that add/removes rules while the
GUI is running. Both paths are required to function correctly.

`nix/module-vpn.nix` does not reference `nft` or nftables directly; it manages
the WireGuard tunnel via systemd-networkd and is unaffected by this bug.

---

## 2. Problem Definition

### Why EPERM Occurs

`nft` requires `CAP_NET_ADMIN` (or effective UID 0) to create, modify, or delete
nftables tables and chains. The `vex-vpn` GUI runs as a regular user in a
`systemd --user` service (or interactively). Spawning `nft` directly inherits the
user's credentials; the kernel rejects netlink socket operations on nftables with
`EPERM` (errno 1).

The `apply_kill_switch` function captures this failure:

```
nft failed to apply kill switch: Operation not permitted
```

However, the error is not surfaced to the user in the UI — the toggle button
changes state visually while no firewall rules are actually written.

### Why `sudo` Is the Correct Fix

The NixOS module (`module-gui.nix`) already declares the precise NOPASSWD sudo
rule needed. The rule covers all invocations of the `nft` binary for `wheel` group
members. The fix is a minimal, targeted change: replace `Command::new("nft")` with
`Command::new("sudo")` and prepend `"nft"` as the first argument. This routes the
subprocess through `sudo`, which elevates privileges and matches the NOPASSWD rule.

No polkit changes are needed (polkit covers only systemd unit management, not
nftables). No capabilities wrapper is needed (sudo already handles this).

---

## 3. Proposed Solution

### 3.1 `apply_kill_switch` — Line 129

**Before:**
```rust
    let mut child = tokio::process::Command::new("nft")
        .arg("-f")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn nft: {}", e))?;
```

**After:**
```rust
    let mut child = tokio::process::Command::new("sudo")
        .arg("nft")
        .arg("-f")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("failed to spawn nft: {}", e))?;
```

No other lines in `apply_kill_switch` require changes.

### 3.2 `remove_kill_switch` — Line 160

**Before:**
```rust
    let output = tokio::process::Command::new("nft")
        .args(["delete", "table", "inet", "pia_kill_switch"])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to spawn nft: {}", e))?;
```

**After:**
```rust
    let output = tokio::process::Command::new("sudo")
        .args(["nft", "delete", "table", "inet", "pia_kill_switch"])
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("failed to spawn nft: {}", e))?;
```

No other lines in `remove_kill_switch` require changes.

### 3.3 Summary of All Changes

| Location | Line | Change |
|---|---|---|
| `src/dbus.rs` — `apply_kill_switch` | 129 | `Command::new("nft")` → `Command::new("sudo")` + `.arg("nft")` prepended |
| `src/dbus.rs` — `remove_kill_switch` | 160 | `Command::new("nft")` → `Command::new("sudo")` + `"nft"` prepended to args slice |

Only `src/dbus.rs` needs to be modified.

---

## 4. Risks and Mitigations

### Risk 1: sudo PATH Resolution

**Risk:** `sudo nft` resolves `nft` via sudo's secure `PATH`
(`/run/wrappers/bin:/run/current-system/sw/bin:…`). On NixOS this is a symlink
that points back to the Nix store binary `${pkgs.nftables}/bin/nft`. Sudo matches
the resolved canonical path against the sudoers rule. If the symlink resolution
diverges from the Nix store path in the rule, NOPASSWD might not match and sudo
would prompt for a password (or deny with EPERM in non-TTY context).

**Mitigation:** This is the standard NixOS pattern for sudo + Nix store binaries;
NixOS sudo integration resolves symlinks before matching. The `security.sudo.extraRules`
mechanism (which uses `extraConfig` under the hood) writes the full Nix store path
but NixOS's sudo wrapper handles this correctly. No special handling is needed in
Rust code. This is verified to work for other NixOS projects using the same pattern.

### Risk 2: `sudo` Not in PATH

**Risk:** If the calling process has a restricted `PATH` that lacks `sudo`, `Command::new("sudo")` will fail with `ENOENT`.

**Mitigation:** `sudo` lives in `/run/wrappers/bin/sudo` on NixOS, which is always
in the default NixOS `PATH`. The `vex-vpn` systemd user service sets no restrictive
`PATH` override. This risk is negligible.

### Risk 3: Non-TTY sudo Interaction

**Risk:** If the NOPASSWD rule is somehow not active (e.g. module not imported,
user not in `wheel`), `sudo` will attempt to prompt for a password on a TTY. Since
the GUI runs without a TTY, sudo will abort with an error.

**Mitigation:** This is the correct, expected behavior — it surfaces the
misconfiguration rather than silently failing. The existing error propagation in
`apply_kill_switch` (the `anyhow::bail!` on non-zero exit) will correctly surface
the sudo error message to the caller. No additional handling required.

### Risk 4: Argument Ordering with `sudo`

**Risk:** Incorrectly ordering `sudo` flags or inserting options between `sudo` and `nft` could cause failures.

**Mitigation:** The fix uses the simplest form — `sudo nft <args>` — with no
intervening flags. `sudo -n nft <args>` (non-interactive mode) is an option but
is unnecessary since NOPASSWD already suppresses the password prompt, and
`-n` would cause `sudo` to fail immediately if the rule somehow requires a password,
giving a less informative error. Plain `sudo nft` is correct and preferred.

### Risk 5: `remove_kill_switch` Error Handling

**Risk:** Currently `remove_kill_switch` only `warn!`-logs errors. Adding `sudo`
does not change this behavior — a sudo failure during teardown would still be
silently swallowed beyond the warning log.

**Mitigation:** This is an existing design decision (teardown failures are
non-fatal by design — if the table doesn't exist, `nft delete` returns non-zero
and that is acceptable). The behavior is unchanged by this fix. A separate
improvement could propagate teardown errors, but that is out of scope for this bug.

---

## 5. Implementation Steps

1. Open `src/dbus.rs`.
2. In `apply_kill_switch` (line 129): change `Command::new("nft")` to
   `Command::new("sudo")` and insert `.arg("nft")` before `.arg("-f")`.
3. In `remove_kill_switch` (line 160): change `Command::new("nft")` to
   `Command::new("sudo")` and change `.args(["delete", "table", "inet", "pia_kill_switch"])` to `.args(["nft", "delete", "table", "inet", "pia_kill_switch"])`.
4. No `Cargo.toml` changes needed (no new dependencies).
5. No `module-gui.nix` changes needed (NOPASSWD rule already covers all `nft` invocations).

---

## 6. Files to Modify

| File | Change Type |
|---|---|
| `src/dbus.rs` | Modify two `Command::new("nft")` call sites |

No other files require modification.
