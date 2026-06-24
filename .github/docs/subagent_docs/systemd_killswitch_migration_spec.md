# Spec: Systemd Kill Switch Migration

**Feature:** `systemd_killswitch_migration`
**Date:** 2026-06-24

---

## 1. Current State Analysis

### `nix/module-gui.nix`
- Sets `networking.nftables.enable = mkIf cfg.killSwitch.enable true` — forces nftables backend ON when kill switch is enabled.
- Declares `networking.nftables.tables.vex_kill_switch` (inet family) with an OUTPUT+INPUT filter table that DROPs all traffic except loopback, established/related, the VPN interface, and any `allowedInterfaces`/`allowedAddresses`.
- `vpnInterface` default is `"wg0"` — wrong default for OpenVPN, which creates `tun0`.
- No `serviceName` option exists; the kill switch service name is not configurable.

### `nix/module-vpn.nix`
- Sets `networking.nftables.enable = mkDefault true` — enables nftables unconditionally for any host running the VPN backend module.

### `src/state.rs` — `check_kill_switch()`
- Spawns `nft list table inet vex_kill_switch` as a subprocess to detect kill switch state.
- Returns `true` if exit code is 0.

### `src/helper.rs` — `apply_kill_switch()` / `remove_kill_switch()`
- Delegates to the `vex-vpn-helper` binary via `pkexec`.
- The helper binary (`src/bin/helper.rs`) runs `nft -f -` with a hand-crafted ruleset to enable the kill switch, and `nft delete table inet vex_kill_switch` to remove it.

### `src/ui_profiles.rs`
- Calls `crate::helper::apply_kill_switch(&iface)` (takes interface name) / `crate::helper::remove_kill_switch()` on the kill-switch toggle in the profile detail page.

### `src/dbus.rs`
- Contains private `start_unit(name)` and `stop_unit(name)` functions that use `MethodFlags::AllowInteractiveAuth` — already polkit-auth-aware.
- Contains `get_service_status(service)` which queries `ActiveState` via D-Bus.

---

## 2. Problem Definition

NixOS is mutually exclusive between `networking.nftables.enable = true` and `networking.firewall.extraCommands` (iptables). Hosts on vexos-nix (or any NixOS configuration using the iptables firewall backend) break silently when `networking.nftables.enable` is set to true:

1. Both `module-gui.nix` and `module-vpn.nix` force nftables on.
2. The runtime kill switch management code in `src/bin/helper.rs` and `src/state.rs` directly invokes `nft`, which will not exist or will fail on iptables-backend hosts.
3. The `vpnInterface` default (`"wg0"`) is wrong for OpenVPN users; NetworkManager's OpenVPN plugin creates `tun0`.

---

## 3. Proposed Solution Architecture

Replace the nftables-based kill switch with a **systemd oneshot service** (`vex-vpn-killswitch.service`) that applies iptables rules via custom chains. The app then controls the kill switch by calling `StartUnit`/`StopUnit` on that service via the existing systemd D-Bus proxy in `dbus.rs`.

### Benefits
- Works on both nftables and iptables firewall backends — the service is independent of NixOS networking backend.
- Eliminates the privileged `vex-vpn-helper` binary path for kill switch operations (polkit is still used — now via systemd's own polkit gate on `manage-units`).
- Kill switch service name is configurable via `services.vex-vpn.killSwitch.serviceName`; vexos-nix can point at their pre-existing `vpn-kill-switch.service`.

---

## 4. Implementation Steps

### 4.1 `nix/module-gui.nix`

**Remove:**
- `networking.nftables.enable = mkIf cfg.killSwitch.enable true;`
- `networking.nftables.tables.vex_kill_switch = mkIf cfg.killSwitch.enable { … };`

**Change:**
- `vpnInterface` default: `"wg0"` → `"tun0"`

**Add option:**
```nix
killSwitch.serviceName = mkOption {
  type = types.str;
  default = "vex-vpn-killswitch";
  description = ''
    Name of the systemd service used to manage the kill switch.
    Override to "vpn-kill-switch" on vexos-nix to use the
    system-provided service instead of the vex-vpn-managed one.
  '';
};
```

**Add systemd service** (only when `cfg.killSwitch.enable = true`):
```nix
systemd.services.vex-vpn-killswitch = mkIf cfg.killSwitch.enable {
  description = "vex-vpn network kill switch (iptables)";
  after = [ "network.target" ];
  wantedBy = [];  # starts stopped; app toggles at runtime
  serviceConfig = {
    Type = "oneshot";
    RemainAfterExit = true;
    ExecStart = <start-script>;
    ExecStop  = <stop-script>;
  };
};
```

**Start script** (iptables custom chains `VEX_KS_OUT` / `VEX_KS_IN`):
1. Create chains (flush if already exist).
2. Populate OUTPUT chain: allow lo, ESTABLISHED/RELATED, DHCP (UDP 67), VPN bootstrap (UDP 1194, TCP 443, UDP 51820), tunnel interfaces via prefix (`tun+`, `wg+`), named interfaces (`nordlynx`, `tailscale0`), then DROP.
3. Populate INPUT chain: allow lo, ESTABLISHED/RELATED, DHCP (UDP 68), tunnel prefixes, named interfaces, then DROP.
4. Insert jump rules at top of built-in OUTPUT/INPUT chains.
5. Mirror all rules for ip6tables.

**Stop script:**
1. Remove jump rules from OUTPUT/INPUT.
2. Flush and delete custom chains.
3. Mirror for ip6tables.

**Add polkit rule** (allowing wheel group / active local user to toggle kill switch service without password):
```js
polkit.addRule(function(action, subject) {
  if (
    action.id === "org.freedesktop.systemd1.manage-units" &&
    action.lookup("unit") === "${cfg.killSwitch.serviceName}.service" &&
    (subject.isInGroup("wheel") || (subject.local && subject.active))
  ) {
    return polkit.Result.YES;
  }
});
```

### 4.2 `nix/module-vpn.nix`

**Remove:**
```nix
networking.nftables.enable = mkDefault true;
```

### 4.3 `src/config.rs`

Add `kill_switch_service` field:
```rust
#[serde(default = "default_kill_switch_service")]
pub kill_switch_service: String,

fn default_kill_switch_service() -> String {
    "vex-vpn-killswitch".to_string()
}
```

Update `Config::default()` to include it.

### 4.4 `src/state.rs`

**`AppState`** — add field:
```rust
pub kill_switch_service_name: String,
```

Initialize in `new()` to `"vex-vpn-killswitch"` and in `new_with_config(cfg)` to `cfg.kill_switch_service.clone()`.

**`check_kill_switch(service_name: &str)` replacement:**
```rust
async fn check_kill_switch(service_name: &str) -> Result<bool> {
    let unit = format!("{}.service", service_name);
    match crate::dbus::get_service_status(&unit).await {
        Ok(s) => Ok(s == "active"),
        Err(_) => Ok(false),
    }
}
```

Update `poll_once` to read `service_name` from state and pass it to `check_kill_switch`.

### 4.5 `src/helper.rs`

Replace the entire pkexec/helper-based implementation:

```rust
/// Enable the kill switch by starting the systemd service.
pub async fn apply_kill_switch() -> Result<()> {
    let cfg = crate::config::Config::load().unwrap_or_default();
    let unit = format!("{}.service", cfg.kill_switch_service);
    crate::dbus::start_kill_switch_unit(&unit).await
}

/// Disable the kill switch by stopping the systemd service.
pub async fn remove_kill_switch() -> Result<()> {
    let cfg = crate::config::Config::load().unwrap_or_default();
    let unit = format!("{}.service", cfg.kill_switch_service);
    crate::dbus::stop_kill_switch_unit(&unit).await
}
```

Remove all pkexec/IPC/nft code.

### 4.6 `src/dbus.rs`

Expose public wrappers (or make `start_unit`/`stop_unit` pub(crate)):

```rust
pub async fn start_kill_switch_unit(name: &str) -> Result<()> {
    start_unit(name).await
}

pub async fn stop_kill_switch_unit(name: &str) -> Result<()> {
    stop_unit(name).await
}
```

### 4.7 `src/ui_profiles.rs`

Update kill switch toggle:
```rust
// Before:
crate::helper::apply_kill_switch(&iface).await
// After:
crate::helper::apply_kill_switch().await
```

(The `iface` variable is no longer needed by `apply_kill_switch`; remove `profile_iface` clone if no longer used.)

### 4.8 `src/ui.rs` — Startup kill switch detection

In `build_ui`, after the window is built, spawn a deferred async task that:
1. Reads the kill switch service name from `AppState`.
2. Calls `dbus::get_service_status(unit)`.
3. If the result is `"active"`, show a toast via `toast_overlay.add_toast(...)` saying:
   > "System kill switch is already active — vex-vpn is deferring to it."

This gives users on vexos-nix (where `vpn-kill-switch.service` may already be running) a clear, non-blocking notification.

---

## 5. Dependencies

All required D-Bus and async infrastructure is already present:
- `zbus` proxy for `org.freedesktop.systemd1.Manager` (existing, `dbus.rs`)
- `tokio::process` is no longer needed by `helper.rs` after this change
- `iptables` package reference in the Nix service scripts: `pkgs.iptables`

No new crate dependencies are introduced.

---

## 6. Configuration Changes

| File | Change |
|------|--------|
| `nix/module-gui.nix` | Remove nftables block; add `serviceName` option; change `vpnInterface` default; add systemd service + polkit rule |
| `nix/module-vpn.nix` | Remove `networking.nftables.enable = mkDefault true` |
| `src/config.rs` | Add `kill_switch_service: String` field |
| `src/state.rs` | Add `kill_switch_service_name` to AppState; replace `nft` subprocess check |
| `src/helper.rs` | Replace pkexec/nft with D-Bus systemd calls |
| `src/dbus.rs` | Add public wrappers for kill switch unit control |
| `src/ui_profiles.rs` | Remove `iface` argument from `apply_kill_switch` call |
| `src/ui.rs` | Add startup kill switch detection toast |

---

## 7. Risks and Mitigations

| Risk | Mitigation |
|------|------------|
| Host does not have `iptables` | The Nix module pulls in `pkgs.iptables` via ExecStart script path; hosts without the module won't have the service either |
| Prefix `+` matching (`tun+`, `wg+`) not supported in all iptables versions | Prefix matching with `+` is standard in `iptables` since 1.x; `||true` guards on named interfaces that may not exist (`nordlynx`, `tailscale0`) |
| ip6tables not available | Wrap ip6tables calls in `|| true`; failure is non-fatal for IPv4-only hosts |
| `vex-vpn-killswitch.service` name differs from `serviceName` on vexos-nix | The new `serviceName` option lets the NixOS config set `"vpn-kill-switch"` to match the system service |
| Helper binary `src/bin/helper.rs` becomes partially dead code | Helper binary's nft commands become dead code but the binary itself is still referenced in `Cargo.toml`; leave binary in place, mark as `#[allow(dead_code)]` where needed — the binary is now a no-op but removal is a separate task |

---

## 8. Build Commands

All via `nix develop --command`:
- `cargo fmt --check`
- `cargo clippy -- -D warnings`
- `cargo build`
- `cargo test`
