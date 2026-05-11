# Self-Install Flow Specification — vex-vpn

**Feature**: `self_install`
**Phase**: 1 — Research & Specification
**Date**: 2026-05-10

---

## Table of Contents

1. [Current State Analysis](#1-current-state-analysis)
2. [Problem Definition](#2-problem-definition)
3. [Research — Six Sources](#3-research--six-sources)
4. [What pia-vpn.service Actually Does](#4-what-pia-vpnservice-actually-does)
5. [Exact Embedded File Content](#5-exact-embedded-file-content)
6. [HelperRequest / Command Changes](#6-helperrequest--command-changes)
7. [UI Flow](#7-ui-flow)
8. [Uninstall Flow](#8-uninstall-flow)
9. [Files to Modify](#9-files-to-modify)
10. [Risks and Mitigations](#10-risks-and-mitigations)

---

## 1. Current State Analysis

### 1.1 NixOS module install path (normal)

`nix/module-vpn.nix` declares a `systemd.services.pia-vpn` unit.
When a user adds `nixosModules.default` to their `flake.nix` imports and sets
`services.pia-vpn.enable = true`, NixOS generates and activates the unit with
all tools pinned to Nix store paths.

`nix/module-gui.nix` handles:
- Installing the GUI binary and the helper binary into the system profile
- Linking the polkit `.policy` file via `environment.etc`
- Setting up an optional autostart user service
- Wiring DNS provider into `services.pia-vpn.dnsServers`

### 1.2 What is missing without the module

Running `nix run github:victorytek/vex-vpn` or installing via `nix profile install`
delivers only the compiled binaries. The following are absent:

| Missing | Impact |
|---------|--------|
| `/etc/systemd/system/pia-vpn.service` | Connect button D-Bus call fails immediately |
| `/etc/vex-vpn/ca.rsa.4096.crt` | CA cert for PIA API calls from the service |
| `/etc/vex-vpn/credentials.env` | PIA_USER / PIA_PASS env file for the service |
| `/etc/vex-vpn/pia-connect.sh` | Script run by ExecStart |
| `/etc/vex-vpn/pia-disconnect.sh` | Script run by ExecStop |
| `/etc/polkit-1/actions/org.vex-vpn.helper.policy` | pkexec action file |

### 1.3 Current "missing service" handling in ui.rs

In `src/ui.rs` inside the connect button click handler (around line 598–618),
when `crate::dbus::connect_vpn()` returns an error whose message contains
`"NoSuchUnit"` or `"No such unit"`, the code shows a static
`adw::MessageDialog` that says:

> "The pia-vpn.service systemd unit was not found. Enable the vex-vpn
> NixOS module… Then run: sudo nixos-rebuild switch"

This is what the self-install flow replaces.

### 1.4 Helper binary protocol (current)

`src/bin/helper.rs` reads newline-delimited JSON from stdin and writes
JSON responses to stdout. Executed via `pkexec`. Current ops:

| op | purpose |
|----|---------|
| `enable_kill_switch` | Writes an nft ruleset via stdin |
| `disable_kill_switch` | Deletes the `pia_kill_switch` table |
| `status` | Reports whether the kill-switch table exists |

Response format: `{"ok": bool, "error"?: "...", "active"?: bool}`

### 1.5 dbus.rs

`src/dbus.rs` defines:
- `SystemdManagerProxy` with `start_unit`, `stop_unit`, `load_unit`
- `SystemdUnitProxy` with `active_state` (D-Bus property)
- `get_service_status(service)` — calls `load_unit` then reads `active_state`

`LoadUnit` always returns a unit object path even when no file exists on disk;
the `LoadState` property on the unit object will be `"not-found"` in that case.

### 1.6 Key facts about helper_path() in src/helper.rs

```rust
fn helper_path() -> &'static str {
    const NIXOS_PATH: &str = "/run/current-system/sw/libexec/vex-vpn-helper";
    if std::path::Path::new(NIXOS_PATH).exists() {
        NIXOS_PATH
    } else {
        "vex-vpn-helper"   // falls back to PATH search
    }
}
```

With `nix run` or `nix profile install`, the helper is NOT at the NixOS system
profile path and it is NOT in `$PATH`. This function must be extended.

---

## 2. Problem Definition

`nix run github:victorytek/vex-vpn` and `nix profile install` give users the
compiled GUI, but connecting fails immediately because `pia-vpn.service` does
not exist. The current error dialog gives no actionable path for users who
aren't configuring a NixOS system module.

**Goal**: When `pia-vpn.service` is absent, the app proactively detects this at
startup, offers a single "Install" click that writes all required files via the
privileged `vex-vpn-helper` binary (invoked through `pkexec`), runs
`systemctl daemon-reload`, and then enables the Connect button.

---

## 3. Research — Six Sources

### Source 1 — systemd service type: oneshot + RemainAfterExit

`Type=oneshot` with `RemainAfterExit=yes` is the canonical pattern for a
"connect-and-stay-active" VPN service: the unit's `ActiveState` becomes
`"active"` once the `ExecStart` script exits 0, and stays active until
`ExecStop` runs (or the unit is explicitly stopped). The existing poll loop in
`state.rs` already maps `"active"` → `ConnectionStatus::Connected`. No changes
needed to the poll loop for the self-installed service.

**Reference**: systemctl(1) man page, Arch Linux, 2026-05-10.
Confirmed: `daemon-reload` (not `daemon-reexec`) is the correct command
after writing a new unit file to `/etc/systemd/system/`. `daemon-reload` causes
systemd to re-scan all unit files and rebuild the dependency graph.
`daemon-reexec` re-executes PID 1 itself and is only for systemd upgrades.

### Source 2 — D-Bus GetUnit vs LoadUnit

From `org.freedesktop.systemd1` D-Bus interface documentation:

- `GetUnit(name)` — returns only units **already in memory**. Returns
  `org.freedesktop.systemd1.NoSuchUnit` error if not loaded.
- `LoadUnit(name)` — **always** returns an object path. If the unit file
  doesn't exist, it loads a transient stub with `LoadState = "not-found"`.

**Check strategy**: Call `load_unit(service)` (already in `dbus.rs`), then
read the `LoadState` property of the returned unit object. If `LoadState ==
"not-found"` → unit file absent → show install dialog.

This is safer than checking `GetUnit` (which only sees in-memory units) and
avoids a filesystem check (which could race with systemd).

### Source 3 — polkit auth_admin_keep and bootstrap without a .policy file

polkit `auth_admin_keep` caches the admin authentication token for the session
(typically 5 minutes), allowing subsequent pkexec calls without re-prompting.
This requires the `.policy` file to be present at `/etc/polkit-1/actions/` or
`/usr/share/polkit-1/actions/` before pkexec is invoked.

**Bootstrap problem**: The first `InstallBackend` pkexec call happens before
the `.policy` file is written. Without a matching `.policy` file, pkexec falls
back to the built-in `org.freedesktop.policykit.exec` action, which defaults to
`auth_admin` (prompts the user, no caching). This is acceptable for the one-time
install. After install writes the `.policy` file, all future pkexec invocations
(for kill switch) benefit from `auth_admin_keep`.

The `.policy` file needs the **actual path** to the helper binary embedded in
the `<annotate key="org.freedesktop.policykit.exec.path">` element. During
`InstallBackend`, the helper reads its own real path from `/proc/self/exe` and
substitutes it into the policy XML before writing.

**Reference**: polkit(8), freedesktop.org specification.

### Source 4 — /etc/systemd/system/ vs /usr/lib/systemd/system/

`/usr/lib/systemd/system/` — for units installed by **packages** (managed by a
package manager). This directory may be overwritten by package upgrades.

`/etc/systemd/system/` — for **administrator-customised** or manually-installed
units. Takes precedence over `/usr/lib/systemd/system/`. Survives package
upgrades. This is the correct location for the self-install flow.

**Reference**: systemd.unit(5), Arch Linux man pages.

### Source 5 — How Docker / NetworkManager handle self-install of system services

Docker's post-install documentation (`docs.docker.com/engine/install/linux-postinstall/`)
demonstrates the canonical pattern: after writing or enabling a service, run
`systemctl daemon-reload` followed by `systemctl enable docker.service`. The
service is written to `/etc/systemd/system/`. NetworkManager uses the same
approach for drop-in files.

Key lesson: **write the unit file first**, then `daemon-reload`, then optionally
`enable`/`start`. The self-install flow follows this order precisely.

### Source 6 — ExecSearchPath= in systemd service units

`ExecSearchPath=` (systemd 243+) lets a unit declare which directories to search
for binaries named in `Exec*` directives. This replaces the need to hard-code
Nix store paths in the unit file.

However, `ExecSearchPath=` applies only to **Exec directives** in the unit file,
not to the PATH seen by shell scripts run by those directives. Shell scripts
must set their own `PATH` at the top of the script.

**Decision**: Do not use `ExecSearchPath=` in the self-installed unit. Instead,
use **absolute paths** in ExecStart/ExecStop (pointing to our installed scripts)
and have the scripts set PATH explicitly at their first line.

---

## 4. What pia-vpn.service Actually Does

> Extracted verbatim from `nix/module-vpn.nix` (MIT licence, upstream: tadfisher/flake).

### 4.1 Service configuration

| Parameter | Value |
|-----------|-------|
| Type | oneshot |
| RemainAfterExit | yes |
| Restart | on-failure |
| EnvironmentFile | `cfg.environmentFile` (→ self-install: `/etc/vex-vpn/credentials.env`) |
| CacheDirectory | `pia-vpn` (→ `/var/cache/pia-vpn`) |
| StateDirectory | `pia-vpn` (→ `/var/lib/pia-vpn`) |
| ConditionFileNotEmpty | certificate file AND environment file |
| Requires + After | `network-online.target` |
| After | `network.target network-online.target` |
| WantedBy | `multi-user.target` |

### 4.2 Runtime tool dependencies

| Tool | NixOS source | Self-install path |
|------|-------------|-------------------|
| `bash` | `pkgs.bash` | via PATH |
| `curl` | `pkgs.curl` | via PATH |
| `gawk` | `pkgs.gawk` | via PATH |
| `jq` | `pkgs.jq` | via PATH |
| `wg` | `pkgs.wireguard-tools` | via PATH |
| `systemd-networkd-wait-online` | `${pkgs.systemd}/lib/systemd/` | searched via explicit probe |
| `ip` | `${pkgs.iproute2}/bin/ip` | via PATH |
| `networkctl` | part of systemd | via PATH |

### 4.3 Connect script logic (ExecStart)

1. `printServerLatency()` — curls port 443 of each PIA meta server with
   `--connect-timeout $maxLatency`; exports elapsed time, region ID, IP
2. Parallel `xargs -I{} bash -c 'printServerLatency {}'` against all regions
3. Sorts by latency; picks the lowest-latency region ID
4. Fetches full region JSON for the winning region
5. Calls `https://$meta_hostname/authv3/generateToken` with `PIA_USER:PIA_PASS`
   and `--cacert $certificateFile`; saves `$STATE_DIRECTORY/token.json`
6. `wg genkey` + `wg pubkey` to generate ephemeral WireGuard keypair
7. Calls `https://$wg_hostname:1337/addKey` to register the public key;
   saves `$STATE_DIRECTORY/wireguard.json`
8. Writes `/run/systemd/network/60-wg0.netdev` (WireGuard peer config) and
   `/run/systemd/network/60-wg0.network` (address, DNS, routing policy rule)
9. `networkctl reload && networkctl up wg0`
10. `systemd-networkd-wait-online -i wg0` (blocks until interface is up)
11. `ip route add default dev wg0 table 42`
12. Runs `cfg.preUp` and `cfg.postUp` hooks (both empty in defaults)

### 4.4 Disconnect script logic (ExecStop / preStop)

1. `rm /run/systemd/network/60-wg0.{netdev,network} || true`
2. `networkctl down wg0`
3. `networkctl reload`
4. Runs `cfg.preDown` and `cfg.postDown` hooks (both empty in defaults)

### 4.5 State files written by the service

| Path | Content |
|------|---------|
| `/var/lib/pia-vpn/region.json` | Winning PIA region JSON |
| `/var/lib/pia-vpn/token.json` | PIA auth token response |
| `/var/lib/pia-vpn/wireguard.json` | WireGuard addKey API response |

The existing `state.rs` `poll_once()` reads these files at `/var/lib/pia-vpn`
(already hardcoded as `state_dir` constant). No change needed.

---

## 5. Exact Embedded File Content

All four string/byte constants below live in `src/bin/helper.rs`.

### 5.1 PIA CA certificate (bytes)

```rust
/// PIA RSA-4096 CA certificate — compiled into the helper binary.
/// Written to /etc/vex-vpn/ca.rsa.4096.crt during InstallBackend.
const PIA_CA_CERT: &[u8] = include_bytes!("../../assets/ca.rsa.4096.crt");
```

> `assets/ca.rsa.4096.crt` already exists in the workspace (used by `pia.rs`).
> The build filter in `flake.nix` already allows `*.crt` files.

### 5.2 Service unit file

Written to `/etc/systemd/system/pia-vpn.service`.

```rust
const SERVICE_UNIT: &str = r#"[Unit]
Description=Connect to Private Internet Access VPN (WireGuard)
Requires=network-online.target
After=network.target network-online.target
ConditionFileNotEmpty=/etc/vex-vpn/ca.rsa.4096.crt
ConditionFileNotEmpty=/etc/vex-vpn/credentials.env

[Service]
Type=oneshot
RemainAfterExit=yes
Restart=on-failure
EnvironmentFile=/etc/vex-vpn/credentials.env
StateDirectory=pia-vpn
CacheDirectory=pia-vpn
ExecStart=/etc/vex-vpn/pia-connect.sh
ExecStop=/etc/vex-vpn/pia-disconnect.sh

[Install]
WantedBy=multi-user.target
"#;
```

> **Note on `WantedBy`**: The `[Install]` section is present for conventional
> completeness but `systemctl enable` is NOT run during install. The user
> controls connection exclusively through the GUI Connect button (and the
> optional auto-connect feature in Preferences). Boot-autostart can be enabled
> later by running `systemctl enable pia-vpn.service` manually.

### 5.3 Connect script

Written to `/etc/vex-vpn/pia-connect.sh` with mode `0755`.

```rust
const CONNECT_SCRIPT: &str = r#"#!/usr/bin/env bash
# pia-connect.sh — installed by vex-vpn self-install
# Adapted from https://github.com/tadfisher/flake (MIT licence)
set -euo pipefail

# Prefer NixOS system profile tools; fall back to standard FHS paths.
export PATH="/run/current-system/sw/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

CERT_FILE="/etc/vex-vpn/ca.rsa.4096.crt"
IFACE="${VEX_VPN_IFACE:-wg0}"
MAX_LATENCY="${VEX_VPN_MAX_LATENCY:-0.1}"
DNS1="${VEX_VPN_DNS1:-10.0.0.241}"
DNS2="${VEX_VPN_DNS2:-10.0.0.242}"

# Locate systemd-networkd-wait-online (private systemd binary, not in standard PATH).
NETWORKD_WAIT_ONLINE=""
for _p in \
    /run/current-system/sw/lib/systemd/systemd-networkd-wait-online \
    /usr/lib/systemd/systemd-networkd-wait-online \
    /lib/systemd/systemd-networkd-wait-online; do
  [[ -x "$_p" ]] && { NETWORKD_WAIT_ONLINE="$_p"; break; }
done
if [[ -z "$NETWORKD_WAIT_ONLINE" ]]; then
  echo "ERROR: systemd-networkd-wait-online not found in standard locations." >&2
  echo "Ensure systemd is installed and systemd-networkd is available." >&2
  exit 1
fi

printServerLatency() {
  serverIP="$1"
  regionID="$2"
  regionName="$(echo ${@:3} | sed 's/ false//' | sed 's/true/(geo)/')"
  time=$(LC_NUMERIC=en_US.utf8 curl -o /dev/null -s \
    --connect-timeout "${MAX_LATENCY}" \
    --write-out "%{time_connect}" \
    http://$serverIP:443)
  if [ $? -eq 0 ]; then
    >&2 echo Got latency ${time}s for region: $regionName
    echo $time $regionID $serverIP
  fi
}
export -f printServerLatency
export MAX_LATENCY

echo Determining region...
serverlist='https://serverlist.piaservers.net/vpninfo/servers/v4'
allregions=$(curl -s "$serverlist" | head -1)
filtered="$(echo $allregions | jq -r '.regions[]
           | .servers.meta[0].ip+" "+.id+" "+.name+" "+(.geo|tostring)')"
best="$(echo "$filtered" | xargs -I{} bash -c 'printServerLatency {}' |
        sort | head -1 | awk '{ print $2 }')"
if [ -z "$best" ]; then
  >&2 echo "No region found with latency under ${MAX_LATENCY} s. Stopping."
  exit 1
fi
region="$(echo $allregions | jq --arg REGION_ID "$best" -r '.regions[] | select(.id==$REGION_ID)')"
meta_ip="$(echo $region | jq -r '.servers.meta[0].ip')"
meta_hostname="$(echo $region | jq -r '.servers.meta[0].cn')"
wg_ip="$(echo $region | jq -r '.servers.wg[0].ip')"
wg_hostname="$(echo $region | jq -r '.servers.wg[0].cn')"
echo "$region" > "$STATE_DIRECTORY/region.json"

echo Generating token...
tokenResponse="$(curl -s -u "$PIA_USER:$PIA_PASS" \
  --connect-to "$meta_hostname::$meta_ip:" \
  --cacert "$CERT_FILE" \
  "https://$meta_hostname/authv3/generateToken")"
if [ "$(echo "$tokenResponse" | jq -r '.status')" != "OK" ]; then
  >&2 echo "Failed to generate token. Stopping."
  exit 1
fi
echo "$tokenResponse" > "$STATE_DIRECTORY/token.json"
token="$(echo "$tokenResponse" | jq -r '.token')"

echo Connecting to the PIA WireGuard API on $wg_ip...
privateKey="$(wg genkey)"
publicKey="$(echo "$privateKey" | wg pubkey)"
json="$(curl -s -G \
  --connect-to "$wg_hostname::$wg_ip:" \
  --cacert "$CERT_FILE" \
  --data-urlencode "pt=${token}" \
  --data-urlencode "pubkey=$publicKey" \
  "https://${wg_hostname}:1337/addKey")"
status="$(echo "$json" | jq -r '.status')"
if [ "$status" != "OK" ]; then
  >&2 echo "Server did not return OK. Stopping."
  >&2 echo "$json"
  exit 1
fi

echo Creating network interface ${IFACE}.
echo "$json" > "$STATE_DIRECTORY/wireguard.json"

gateway="$(echo "$json" | jq -r '.server_ip')"
servervip="$(echo "$json" | jq -r '.server_vip')"
peerip=$(echo "$json" | jq -r '.peer_ip')

mkdir -p /run/systemd/network/
touch /run/systemd/network/60-${IFACE}.{netdev,network}
chown root:systemd-network /run/systemd/network/60-${IFACE}.{netdev,network}
chmod 640 /run/systemd/network/60-${IFACE}.{netdev,network}

cat > /run/systemd/network/60-${IFACE}.netdev <<NETDEV
[NetDev]
Description = WireGuard PIA network device
Name = ${IFACE}
Kind = wireguard

[WireGuard]
PrivateKey = $privateKey

[WireGuardPeer]
PublicKey = $(echo "$json" | jq -r '.server_key')
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint = ${wg_ip}:$(echo "$json" | jq -r '.server_port')
PersistentKeepalive = 25
NETDEV

cat > /run/systemd/network/60-${IFACE}.network <<NETCFG
[Match]
Name = ${IFACE}

[Network]
Description = WireGuard PIA network interface
Address = ${peerip}/32
DNS = ${DNS1}
DNS = ${DNS2}
IPForward = ipv4

[RoutingPolicyRule]
From = ${peerip}
Table = 42
NETCFG

echo Bringing up network interface ${IFACE}.

networkctl reload
networkctl up ${IFACE}

"$NETWORKD_WAIT_ONLINE" -i "${IFACE}"

ip route add default dev "${IFACE}" table 42
"#;
```

### 5.4 Disconnect script

Written to `/etc/vex-vpn/pia-disconnect.sh` with mode `0755`.

```rust
const DISCONNECT_SCRIPT: &str = r#"#!/usr/bin/env bash
# pia-disconnect.sh — installed by vex-vpn self-install
set -euo pipefail

export PATH="/run/current-system/sw/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

IFACE="${VEX_VPN_IFACE:-wg0}"

echo Removing network interface ${IFACE}.
rm /run/systemd/network/60-${IFACE}.{netdev,network} || true

echo Bringing down network interface ${IFACE}.
networkctl down ${IFACE}
networkctl reload
"#;
```

### 5.5 Polkit policy (runtime-generated, not a const)

The polkit policy cannot be a `const &str` because it must embed the **actual
path** to the running helper binary (read from `/proc/self/exe`). It is
generated at runtime inside `handle_install_backend()`:

```rust
fn build_polkit_policy(helper_path: &str) -> String {
    format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE policyconfig PUBLIC
  "-//freedesktop//DTD PolicyKit Policy Configuration 1.0//EN"
  "http://www.freedesktop.org/standards/PolicyKit/1/policyconfig.dtd">
<policyconfig>
  <vendor>vex-vpn</vendor>
  <vendor_url>https://github.com/victorytek/vex-vpn</vendor_url>

  <action id="org.vex-vpn.helper.run">
    <description>Manage VPN kill switch via nftables</description>
    <message>Authentication is required to control the VPN kill switch</message>
    <icon_name>network-vpn-symbolic</icon_name>
    <defaults>
      <allow_any>auth_admin</allow_any>
      <allow_inactive>auth_admin</allow_inactive>
      <allow_active>auth_admin_keep</allow_active>
    </defaults>
    <annotate key="org.freedesktop.policykit.exec.path">{}</annotate>
    <annotate key="org.freedesktop.policykit.exec.allow_gui">true</annotate>
  </action>
</policyconfig>
"#, helper_path)
}
```

> `helper_path` is obtained via `std::fs::read_link("/proc/self/exe")`
> (gives the canonical, symlink-resolved path of the running binary).

---

## 6. HelperRequest / Command Changes

### 6.1 src/bin/helper.rs — Command enum

Add two new variants to the `Command` enum:

```rust
#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Command {
    EnableKillSwitch { /* existing */ },
    DisableKillSwitch,
    Status,
    // NEW:
    InstallBackend {
        pia_user: String,
        pia_pass: String,
    },
    UninstallBackend,
}
```

`interface` and `dns_servers` are **not** parameters — they are baked into the
embedded scripts with defaults (wg0, 10.0.0.241, 10.0.0.242). This keeps the
install flow simple. Future improvements can add an `interface` parameter
without breaking existing install databases.

### 6.2 src/bin/helper.rs — handle_command additions

```rust
Command::InstallBackend { pia_user, pia_pass } => {
    handle_install_backend(pia_user, pia_pass)
}
Command::UninstallBackend => handle_uninstall_backend(),
```

#### `handle_install_backend(pia_user, pia_pass) -> Response`

Validation (fail fast, return error response):
- `pia_user`: non-empty, ≤ 128 bytes, no ASCII control chars (`\n`, `\r`, NUL)
- `pia_pass`: non-empty, ≤ 128 bytes, no ASCII control chars

Write sequence (any failure → return error Response):

1. Create `/etc/vex-vpn/` directory (mode 0755, `std::fs::create_dir_all`)
2. Write `/etc/vex-vpn/ca.rsa.4096.crt` — `PIA_CA_CERT` bytes, mode 0644
3. Write `/etc/vex-vpn/credentials.env` atomically:
   - Content: `"PIA_USER={pia_user}\nPIA_PASS={pia_pass}\n"` (safe because we
     validated no newlines in either value; the env file format is key=value
     per line, not shell evaluated)
   - Mode: **0600**, owner root:root
   - Use temp-file-then-rename pattern for atomicity (write to
     `/etc/vex-vpn/.credentials.env.tmp`, then `rename`)
4. Write `/etc/vex-vpn/pia-connect.sh` — `CONNECT_SCRIPT` bytes, mode 0755
5. Write `/etc/vex-vpn/pia-disconnect.sh` — `DISCONNECT_SCRIPT` bytes, mode 0755
6. Write `/etc/systemd/system/pia-vpn.service` — `SERVICE_UNIT` bytes, mode 0644
7. Read own real path: `std::fs::read_link("/proc/self/exe")`
8. Write `/etc/polkit-1/actions/org.vex-vpn.helper.policy` — generated policy,
   mode 0644. (Overwrite if present — the path may differ between versions.)
9. Run `systemctl daemon-reload` (blocking `std::process::Command`)
10. Return `Response { ok: true, error: None, active: None }`

#### `handle_uninstall_backend() -> Response`

1. Stop any running instance: `systemctl stop pia-vpn.service` (ignore error —
   may already be stopped)
2. Remove `/etc/systemd/system/pia-vpn.service` (ignore ENOENT)
3. Remove `/etc/vex-vpn/credentials.env` (ignore ENOENT — credentials are
   sensitive; removed on uninstall)
4. Run `systemctl daemon-reload`
5. Return `Response { ok: true, … }`

> **Note**: `/etc/vex-vpn/ca.rsa.4096.crt`, `pia-connect.sh`, and
> `pia-disconnect.sh` are intentionally left on disk so a re-install only needs
> to re-write the unit file and credentials. The polkit policy is also kept.
> The `/etc/vex-vpn/` directory itself is not removed.

### 6.3 src/helper.rs — HelperRequest struct extension

Add two nullable fields to the existing struct:

```rust
#[derive(Serialize)]
struct HelperRequest<'a> {
    op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    interface: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_interfaces: Option<&'a [String]>,
    // NEW:
    #[serde(skip_serializing_if = "Option::is_none")]
    pia_user: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pia_pass: Option<&'a str>,
}
```

### 6.4 src/helper.rs — new public async functions

```rust
/// Install the pia-vpn systemd backend service via the privileged helper.
/// Writes all required files under /etc/vex-vpn/ and /etc/systemd/system/.
pub async fn install_backend(pia_user: &str, pia_pass: &str) -> Result<()> {
    let resp = call_helper(&HelperRequest {
        op: "install_backend",
        interface: None,
        allowed_interfaces: None,
        pia_user: Some(pia_user),
        pia_pass: Some(pia_pass),
    })
    .await?;
    if resp.ok { Ok(()) } else { bail!("helper error: {}", resp.error.unwrap_or_default()) }
}

/// Remove the pia-vpn systemd backend service.
pub async fn uninstall_backend() -> Result<()> {
    let resp = call_helper(&HelperRequest {
        op: "uninstall_backend",
        interface: None,
        allowed_interfaces: None,
        pia_user: None,
        pia_pass: None,
    })
    .await?;
    if resp.ok { Ok(()) } else { bail!("helper error: {}", resp.error.unwrap_or_default()) }
}
```

### 6.5 src/helper.rs — helper_path() extension

Replace the current `fn helper_path() -> &'static str` with a version that
returns `String` and checks additional locations:

```rust
fn helper_path() -> String {
    use std::path::Path;
    // 1. NixOS system profile (module-installed)
    if Path::new("/run/current-system/sw/libexec/vex-vpn-helper").exists() {
        return "/run/current-system/sw/libexec/vex-vpn-helper".to_owned();
    }
    // 2. User Nix profile (nix profile install)
    if let Ok(home) = std::env::var("HOME") {
        let p = format!("{}/.nix-profile/libexec/vex-vpn-helper", home);
        if Path::new(&p).exists() { return p; }
    }
    // 3. System-level Nix profile
    if Path::new("/nix/var/nix/profiles/default/libexec/vex-vpn-helper").exists() {
        return "/nix/var/nix/profiles/default/libexec/vex-vpn-helper".to_owned();
    }
    // 4. Sibling libexec/ of current binary (covers nix run store path)
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            // binary is at $out/bin/vex-vpn; helper at $out/libexec/vex-vpn-helper
            let candidate = bin_dir
                .parent()
                .map(|p| p.join("libexec").join("vex-vpn-helper"));
            if let Some(p) = candidate {
                if p.exists() {
                    return p.to_string_lossy().into_owned();
                }
            }
        }
    }
    // 5. PATH fallback (dev builds)
    "vex-vpn-helper".to_owned()
}
```

`call_helper()` must be updated to accept `&str` instead of `&'static str`
from `helper_path()`.

---

## 7. UI Flow

### 7.1 Startup install check (src/ui.rs — build_ui)

After `window.present()` at the end of `build_ui()`, spawn a one-shot async
task:

```rust
// --- Startup: check if pia-vpn.service is installed ---
{
    let window_ref = window.clone();
    let toasts_ref = toast_overlay.clone();
    let connect_btn_ref = live.connect_btn.clone();
    glib::spawn_future_local(async move {
        if !crate::dbus::is_service_unit_installed("pia-vpn.service").await {
            // Disable Connect immediately so the user can't try to connect
            // before the service exists.
            connect_btn_ref.set_sensitive(false);
            show_service_install_dialog(&window_ref, &toasts_ref, &connect_btn_ref).await;
        }
    });
}
```

`live` must be made available to this block. To avoid borrow issues, clone the
`connect_btn` handle from `live` before the existing glib timeout closure. The
`LiveWidgets` struct can have `connect_btn` cloned at `build_ui` call time.

### 7.2 show_service_install_dialog (new function in src/ui.rs)

```rust
async fn show_service_install_dialog(
    parent: &adw::ApplicationWindow,
    toasts: &adw::ToastOverlay,
    connect_btn: &gtk4::Button,
) {
    let dialog = adw::MessageDialog::new(
        Some(parent),
        Some("Install VPN backend?"),
        Some(
            "The pia-vpn system service is not installed. Installing it \
             requires administrator access and writes files to /etc/systemd/system/.\
             \n\nIt can be removed later from Preferences → Advanced.",
        ),
    );
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("install", "Install");
    dialog.set_response_appearance("install", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("install"));
    dialog.set_close_response("cancel");

    let response = dialog.choose_future().await;
    if response.as_str() != "install" {
        return; // Connect button remains disabled; user must restart or open Prefs
    }

    // Read credentials from the keyring (set during onboarding).
    let creds = match crate::secrets::load_sync_hint() {
        Ok(Some(c)) => c,
        _ => {
            toasts.add_toast(adw::Toast::new(
                "Cannot install: credentials not found. Please sign in first.",
            ));
            return;
        }
    };

    // Show a spinner toast while installing.
    toasts.add_toast(adw::Toast::new("Installing VPN backend…"));

    match crate::helper::install_backend(&creds.username, &creds.password).await {
        Ok(()) => {
            connect_btn.set_sensitive(true);
            toasts.add_toast(adw::Toast::new("VPN backend installed successfully."));
        }
        Err(e) => {
            tracing::error!("install_backend: {}", e);
            toasts.add_toast(adw::Toast::new(&format!("Install failed: {e:#}")));
            // connect_btn stays disabled
        }
    }
}
```

**Note on `choose_future()`**: `adw::MessageDialog::choose_future()` is
available in libadwaita 0.5.x (it's a GTK-rs async helper on top of the
`choose()` signal). This is the idiomatic async pattern used in adw-rs.

### 7.3 Connect button — NoSuchUnit handler (src/ui.rs)

Replace the existing inline dialog block (~lines 602–616) with a call to the
new async function:

```rust
if msg.contains("NoSuchUnit") || msg.contains("No such unit") {
    // Service not installed — offer self-install.
    let w = /* window handle cloned into the closure */;
    let t = toast.clone();
    let btn = btn_c.clone();
    glib::spawn_future_local(async move {
        show_service_install_dialog(&w, &t, &btn).await;
    });
} else {
    tracing::error!("connect: {}", e);
    toast.add_toast(adw::Toast::new(&format!("Connect failed: {e:#}")));
}
```

The window handle needs to be cloned into the connect button's click closure
before this point. The `build_main_page` function signature needs the window
reference passed in, or `build_ui` needs to pass it down. The simplest approach:
since `build_ui` returns `adw::ApplicationWindow`, and `build_main_page` is
called inside `build_ui`, the window is available there. Pass `window.clone()`
to `build_main_page` or create a `Rc<RefCell<Option<adw::ApplicationWindow>>>`
that is filled just after `window` is built.

**Simplest approach**: Change `build_main_page` to accept `adw::ApplicationWindow`
(or `adw::Window`) as a parameter so the connect handler can use it:

```rust
fn build_main_page(
    state: Arc<RwLock<AppState>>,
    window: adw::ApplicationWindow,   // NEW
    initial_auto_connect: bool,
    initial_kill_switch: bool,
    toasts: adw::ToastOverlay,
) -> (gtk4::Box, LiveWidgets)
```

### 7.4 dbus.rs additions

#### New property on SystemdUnit

```rust
#[dbus_proxy(
    interface = "org.freedesktop.systemd1.Unit",
    default_service = "org.freedesktop.systemd1"
)]
trait SystemdUnit {
    #[dbus_proxy(property)]
    fn active_state(&self) -> zbus::Result<String>;

    // NEW:
    #[dbus_proxy(property)]
    fn load_state(&self) -> zbus::Result<String>;
}
```

#### New public function

```rust
/// Returns true if the unit file for `service` is present on disk
/// (LoadState != "not-found").
///
/// Uses `LoadUnit` (not `GetUnit`) so it works even if the unit is inactive
/// and not yet in systemd's memory. Returns false on any D-Bus error.
pub async fn is_service_unit_installed(service: &str) -> bool {
    let Ok(conn) = system_conn().await else { return false };
    let Ok(manager) = SystemdManagerProxy::new(&conn).await else { return false };
    let Ok(unit_path) = manager.load_unit(service).await else { return false };
    let path_ref = unit_path.as_ref();
    let unit = match SystemdUnitProxy::builder(&conn)
        .path(path_ref)
        .map_err(|_| ())
    {
        Ok(b) => match b.build().await { Ok(u) => u, Err(_) => return false },
        Err(_) => return false,
    };
    unit.load_state()
        .await
        .map(|s| s != "not-found")
        .unwrap_or(false)
}
```

Also add a `GetUnit` method to `SystemdManagerProxy` for any future uses:

```rust
fn get_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
```

---

## 8. Uninstall Flow

### 8.1 Preferences page (src/ui_prefs.rs — build_advanced_page)

Add a new `adw::PreferencesGroup` at the END of `build_advanced_page()`,
after the existing `group`:

```rust
// ── VPN Backend Service ──────────────────────────────────────────────
let backend_group = adw::PreferencesGroup::builder()
    .title("VPN Backend Service")
    .description("The pia-vpn system service manages the WireGuard tunnel.")
    .build();

let backend_status_row = adw::ActionRow::builder()
    .title("Service status")
    .subtitle("Checking…")
    .build();
backend_group.add(&backend_status_row);

// Check install status asynchronously and update the row.
{
    let row = backend_status_row.clone();
    glib::spawn_future_local(async move {
        let installed = crate::dbus::is_service_unit_installed("pia-vpn.service").await;
        row.set_subtitle(if installed { "Installed" } else { "Not installed" });
    });
}

// Uninstall button — shown always; disabled if not installed.
let uninstall_btn = gtk4::Button::builder()
    .label("Remove VPN backend service")
    .css_classes(["destructive-action"])
    .margin_top(6)
    .margin_bottom(6)
    .halign(gtk4::Align::End)
    .build();

{
    let status_row = backend_status_row.clone();
    uninstall_btn.connect_clicked(move |btn| {
        btn.set_sensitive(false);
        let row = status_row.clone();
        glib::spawn_future_local(async move {
            match crate::helper::uninstall_backend().await {
                Ok(()) => {
                    row.set_subtitle("Not installed");
                }
                Err(e) => {
                    tracing::error!("uninstall_backend: {}", e);
                    row.set_subtitle(&format!("Error: {e:#}"));
                }
            }
        });
    });
}

backend_group.add(&uninstall_btn);
page.add(&backend_group);
```

> **Note on `adw::PreferencesGroup::add` with a button**: Starting with
> libadwaita 1.4, `add()` accepts any `gtk::Widget`, not just rows. The button
> will be laid out below the row inside the group's box. If this looks wrong
> in testing, wrap it in a plain `adw::ActionRow` with a suffix widget instead.

---

## 9. Files to Modify

| File | Change |
|------|--------|
| `src/bin/helper.rs` | Add `PIA_CA_CERT`, `SERVICE_UNIT`, `CONNECT_SCRIPT`, `DISCONNECT_SCRIPT` constants; add `InstallBackend`, `UninstallBackend` enum variants; add `handle_install_backend()`, `handle_uninstall_backend()`, `build_polkit_policy()` functions; add `write_file_atomic()` helper |
| `src/helper.rs` | Add `pia_user`, `pia_pass` fields to `HelperRequest`; add `install_backend()`, `uninstall_backend()` public async fns; replace `helper_path() -> &'static str` with `helper_path() -> String`; update `call_helper()` to use `String` path |
| `src/dbus.rs` | Add `load_state` property to `SystemdUnitProxy`; add `get_unit` to `SystemdManagerProxy`; add `pub async fn is_service_unit_installed()` |
| `src/ui.rs` | Add `window: adw::ApplicationWindow` param to `build_main_page`; add async startup install check in `build_ui`; add `show_service_install_dialog()` async function; replace inline NoSuchUnit dialog with `show_service_install_dialog` call |
| `src/ui_prefs.rs` | Add VPN Backend `adw::PreferencesGroup` with status row and uninstall button at the end of `build_advanced_page()` |

---

## 10. Risks and Mitigations

| Risk | Mitigation |
|------|-----------|
| `/run/current-system/sw/bin` absent on non-NixOS | Connect script sets PATH with FHS fallback directories |
| `systemd-networkd-wait-online` not in standard PATH | Script probes 3 known locations; exits with clear error |
| `wg` (wireguard-tools) not installed | Script fails with a standard bash error; GUI shows "Connect failed" toast |
| systemd-networkd not enabled | `networkctl up` fails; GUI shows error toast; user must enable systemd-networkd |
| PIA CA cert embedded in binary may become stale | Cert is the PIA RSA-4096 CA (long-lived); update with each release; same cert already in `assets/ca.rsa.4096.crt` used by the pia.rs client |
| Credentials stored in `/etc/vex-vpn/credentials.env` (mode 0600) | Root-only file; same security model as SSH host keys and `/etc/shadow`; adequate for VPN credentials |
| No polkit policy on first install → pkexec uses fallback auth | Acceptable: user sees a standard auth dialog. `auth_admin_keep` activates after first install |
| pkexec not available on the system | `call_helper` returns `spawn pkexec: No such file or directory`; shown as error toast |
| Helper path resolution fails for unusual Nix layouts | Fallback to `"vex-vpn-helper"` (PATH search); dev build `nix develop` puts it in target/debug which is in PATH for tests |
| Uninstall while VPN is active | `handle_uninstall_backend` calls `systemctl stop pia-vpn.service` first; idempotent |
| `choose_future()` not available in adw 0.5.x bindings | If unavailable, use `connect_response` callback pattern instead (sync style); verify against actual libadwaita-rs 0.5.x API before implementation |
| build_main_page signature change breaks callers | Only called once in `build_ui`; the window is already available there — low risk |

---

## Summary

### What pia-vpn.service does

`pia-vpn.service` is a `Type=oneshot, RemainAfterExit=yes` systemd unit that
implements a full WireGuard VPN connection lifecycle for Private Internet Access:
it selects the lowest-latency PIA server, authenticates, registers a WireGuard
keypair via PIA's REST API, writes `systemd-networkd` configuration to
`/run/systemd/network/`, brings up the tunnel, and adds a policy routing rule.
Its `ExecStop` tears down the interface cleanly.

### Exact service file content

See §5.2 (`SERVICE_UNIT` const), §5.3 (`CONNECT_SCRIPT` const), §5.4
(`DISCONNECT_SCRIPT` const), and §5.5 (`build_polkit_policy()` function).

### All files needing modification

`src/bin/helper.rs`, `src/helper.rs`, `src/dbus.rs`, `src/ui.rs`, `src/ui_prefs.rs`

### Spec file path

`/home/nimda/Projects/vex-vpn/.github/docs/subagent_docs/self_install_spec.md`
