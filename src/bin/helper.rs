//! vex-vpn-helper — polkit-gated network helper binary.
//!
//! Runs as root via `pkexec`. Reads newline-delimited JSON commands from stdin,
//! executes nftables operations, and writes JSON responses to stdout.
//!
//! Protocol (one JSON object per line):
//!   stdin:  {"op": "enable_kill_switch", "interface": "wg0"}
//!           {"op": "disable_kill_switch"}
//!           {"op": "status"}
//!   stdout: {"ok": true}
//!           {"ok": false, "error": "..."}
//!           {"ok": true, "active": bool}
//!
//! No GTK, no Tokio, no reqwest — pure sync stdin/stdout.

use std::io::{self, BufRead, Write};
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Embedded file constants — written to /etc/ during InstallBackend
// ---------------------------------------------------------------------------

/// PIA RSA-4096 CA certificate — compiled into the helper binary.
const PIA_CA_CERT: &[u8] = include_bytes!("../../assets/ca.rsa.4096.crt");

const SERVICE_UNIT: &str = r#"[Unit]
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
"#;

const CONNECT_SCRIPT: &str = r#"#!/usr/bin/env bash
# pia-connect.sh — installed by vex-vpn self-install
# Adapted from https://github.com/tadfisher/flake (MIT licence)
set -euo pipefail

# Prefer NixOS system profile tools; fall back to standard FHS paths.
export PATH="/run/current-system/sw/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

CERT_FILE="/var/lib/vex-vpn/ca.rsa.4096.crt"
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

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Command {
    EnableKillSwitch {
        interface: String,
        #[serde(default)]
        allowed_interfaces: Vec<String>,
    },
    DisableKillSwitch,
    Status,
    InstallBackend {
        pia_user: String,
        pia_pass: String,
    },
    UninstallBackend,
    ReinstallUnit,
}

#[derive(Serialize)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    active: Option<bool>,
}

fn main() {
    // Security: verify we are running as root (euid 0).
    // pkexec sets this before exec-ing us.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        let resp = Response {
            ok: false,
            error: Some("must run as root via pkexec".into()),
            active: None,
        };
        println!("{}", serde_json::to_string(&resp).unwrap_or_default());
        std::process::exit(1);
    }

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) if l.trim().is_empty() => continue,
            Ok(l) => l,
            Err(_) => break,
        };
        let resp = match serde_json::from_str::<Command>(&line) {
            Ok(cmd) => handle_command(cmd),
            Err(e) => Response {
                ok: false,
                error: Some(format!("parse error: {}", e)),
                active: None,
            },
        };
        let json = match serde_json::to_string(&resp) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let _ = writeln!(out, "{}", json);
        let _ = out.flush();
    }
}

fn handle_command(cmd: Command) -> Response {
    match cmd {
        Command::EnableKillSwitch {
            interface,
            allowed_interfaces,
        } => {
            if !is_valid_interface(&interface) {
                return Response {
                    ok: false,
                    error: Some(format!("invalid interface name: {:?}", interface)),
                    active: None,
                };
            }
            // Validate any extra allowed interfaces too.
            for extra in &allowed_interfaces {
                if !is_valid_interface(extra) {
                    return Response {
                        ok: false,
                        error: Some(format!("invalid allowed_interface name: {:?}", extra)),
                        active: None,
                    };
                }
            }
            run_nft_enable(&interface, &allowed_interfaces)
        }
        Command::DisableKillSwitch => run_nft_disable(),
        Command::Status => check_status(),
        Command::InstallBackend { pia_user, pia_pass } => {
            handle_install_backend(pia_user, pia_pass)
        }
        Command::UninstallBackend => handle_uninstall_backend(),
        Command::ReinstallUnit => handle_reinstall_unit(),
    }
}

/// Validate a Linux network interface name (max 15 chars, safe for nft).
fn is_valid_interface(name: &str) -> bool {
    if name.is_empty() || name.len() > 15 {
        return false;
    }
    let b = name.as_bytes();
    if !b[0].is_ascii_lowercase() {
        return false;
    }
    b[1..]
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
}

fn nft_binary() -> &'static str {
    // NixOS system profile (preferred).
    if std::path::Path::new("/run/current-system/sw/bin/nft").exists() {
        return "/run/current-system/sw/bin/nft";
    }
    // Debian/Ubuntu place nft in /usr/sbin; some distros use /usr/bin.
    if std::path::Path::new("/usr/bin/nft").exists() {
        return "/usr/bin/nft";
    }
    "/usr/sbin/nft"
}

fn run_nft_enable(iface: &str, extra_ifaces: &[String]) -> Response {
    // Build per-interface accept rules for output + input chains.
    let mut iface_rules = format!(
        "        oifname \"{iface}\" accept\n        iifname \"{iface}\" accept\n",
        iface = iface
    );
    for extra in extra_ifaces {
        iface_rules.push_str(&format!(
            "        oifname \"{extra}\" accept\n        iifname \"{extra}\" accept\n",
            extra = extra
        ));
    }

    let ruleset = format!(
        "table inet pia_kill_switch {{\n\
         chain output {{\n\
             type filter hook output priority 0; policy drop;\n\
             ct state established,related accept\n\
             {iface_rules}\
             oifname \"lo\" accept\n\
         }}\n\
         chain input {{\n\
             type filter hook input priority 0; policy drop;\n\
             ct state established,related accept\n\
             {iface_rules}\
             iifname \"lo\" accept\n\
         }}\n\
         }}",
        iface_rules = iface_rules
    );

    // Pipe the ruleset directly to `nft -f -` via stdin to avoid a
    // predictable /tmp path that could be raced (TOCTOU).
    let mut child = match std::process::Command::new(nft_binary())
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            return Response {
                ok: false,
                error: Some(format!("spawn nft: {}", e)),
                active: None,
            };
        }
    };

    if let Some(stdin) = child.stdin.as_mut() {
        if let Err(e) = stdin.write_all(ruleset.as_bytes()) {
            return Response {
                ok: false,
                error: Some(format!("write nft stdin: {}", e)),
                active: None,
            };
        }
    }
    // Close stdin to signal EOF to nft.
    drop(child.stdin.take());

    match child.wait_with_output() {
        Ok(o) if o.status.success() => Response {
            ok: true,
            error: None,
            active: None,
        },
        Ok(o) => Response {
            ok: false,
            error: Some(String::from_utf8_lossy(&o.stderr).trim().to_string()),
            active: None,
        },
        Err(e) => Response {
            ok: false,
            error: Some(format!("wait for nft: {}", e)),
            active: None,
        },
    }
}

fn run_nft_disable() -> Response {
    let result = std::process::Command::new(nft_binary())
        .args(["delete", "table", "inet", "pia_kill_switch"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    // If the table doesn't exist nft exits non-zero, but that's OK for us.
    match result {
        Ok(_) => Response {
            ok: true,
            error: None,
            active: None,
        },
        Err(e) => Response {
            ok: false,
            error: Some(format!("spawn nft: {}", e)),
            active: None,
        },
    }
}

fn check_status() -> Response {
    let result = std::process::Command::new(nft_binary())
        .args(["list", "table", "inet", "pia_kill_switch"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    let active = matches!(result, Ok(o) if o.status.success());
    Response {
        ok: true,
        error: None,
        active: Some(active),
    }
}

// ---------------------------------------------------------------------------
// InstallBackend / UninstallBackend helpers
// ---------------------------------------------------------------------------

/// Write `content` to `path` atomically via a temp file + rename.
/// The temp file is created in the same directory with mode `0o600`,
/// then permissions are set to `mode` before the rename.
fn write_file_atomic(path: &std::path::Path, content: &[u8], mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let dir = path.parent().unwrap_or(std::path::Path::new("/"));
    let filename = path
        .file_name()
        .unwrap_or_else(|| std::ffi::OsStr::new("tmp"));
    let tmp = dir.join(format!(".{}.tmp", filename.to_string_lossy()));
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600) // restrictive while writing
            .open(&tmp)?;
        f.write_all(content)?;
    }
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(mode))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn build_polkit_policy(helper_path: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
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
"#,
        helper_path
    )
}

/// Try to write the polkit policy to one of the well-known locations.
/// Returns `true` on success, `false` if all attempts fail.
/// Attempts in order:
///   1. /etc/polkit-1/actions/  (standard FHS, read-only on NixOS)
///   2. /run/polkit-1/actions/  (polkit 0.120+ runtime scan directory)
fn try_write_polkit_policy(content: &[u8]) -> bool {
    let candidates = [
        "/etc/polkit-1/actions/org.vex-vpn.helper.policy",
        "/run/polkit-1/actions/org.vex-vpn.helper.policy",
    ];
    for candidate in &candidates {
        let path = std::path::Path::new(candidate);
        if let Some(dir) = path.parent() {
            if std::fs::create_dir_all(dir).is_err() {
                continue;
            }
        }
        if write_file_atomic(path, content, 0o644).is_ok() {
            return true;
        }
    }
    false
}

fn handle_install_backend(pia_user: String, pia_pass: String) -> Response {
    // Validate credentials: non-empty, ≤128 bytes, no ASCII control chars.
    if pia_user.is_empty() || pia_user.len() > 128 || pia_user.bytes().any(|b| b < 0x20) {
        return Response {
            ok: false,
            error: Some("invalid pia_user: must be non-empty, ≤128 bytes, no control chars".into()),
            active: None,
        };
    }
    if pia_pass.is_empty() || pia_pass.len() > 128 || pia_pass.bytes().any(|b| b < 0x20) {
        return Response {
            ok: false,
            error: Some("invalid pia_pass: must be non-empty, ≤128 bytes, no control chars".into()),
            active: None,
        };
    }

    // 1. Create /var/lib/vex-vpn/ directory.
    if let Err(e) = std::fs::create_dir_all("/var/lib/vex-vpn") {
        return Response {
            ok: false,
            error: Some(format!("create /var/lib/vex-vpn: {}", e)),
            active: None,
        };
    }
    if let Err(e) =
        std::fs::set_permissions("/var/lib/vex-vpn", std::fs::Permissions::from_mode(0o755))
    {
        return Response {
            ok: false,
            error: Some(format!("chmod /var/lib/vex-vpn: {}", e)),
            active: None,
        };
    }

    // 2. Write CA certificate (mode 0644).
    let ca_path = std::path::Path::new("/var/lib/vex-vpn/ca.rsa.4096.crt");
    if let Err(e) = write_file_atomic(ca_path, PIA_CA_CERT, 0o644) {
        return Response {
            ok: false,
            error: Some(format!("write ca cert: {}", e)),
            active: None,
        };
    }

    // 3. Write credentials.env atomically (mode 0600).
    let creds_content = format!("PIA_USER={}\nPIA_PASS={}\n", pia_user, pia_pass);
    let creds_path = std::path::Path::new("/var/lib/vex-vpn/credentials.env");
    if let Err(e) = write_file_atomic(creds_path, creds_content.as_bytes(), 0o600) {
        return Response {
            ok: false,
            error: Some(format!("write credentials.env: {}", e)),
            active: None,
        };
    }

    // 4. Write connect script (mode 0755).
    let connect_path = std::path::Path::new("/var/lib/vex-vpn/pia-connect.sh");
    if let Err(e) = write_file_atomic(connect_path, CONNECT_SCRIPT.as_bytes(), 0o755) {
        return Response {
            ok: false,
            error: Some(format!("write pia-connect.sh: {}", e)),
            active: None,
        };
    }

    // 5. Write disconnect script (mode 0755).
    let disconnect_path = std::path::Path::new("/var/lib/vex-vpn/pia-disconnect.sh");
    if let Err(e) = write_file_atomic(disconnect_path, DISCONNECT_SCRIPT.as_bytes(), 0o755) {
        return Response {
            ok: false,
            error: Some(format!("write pia-disconnect.sh: {}", e)),
            active: None,
        };
    }

    // 6. Write systemd unit file (mode 0644) to /run/systemd/system/ (writable on NixOS).
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
            error: Some(format!("write pia-vpn.service: {}", e)),
            active: None,
        };
    }

    // 7. Write polkit policy with the real helper binary path.
    // This is non-fatal on NixOS where /etc/polkit-1/actions/ is read-only —
    // the NixOS module installs the policy via Nix instead.
    let self_path = match std::fs::read_link("/proc/self/exe") {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(e) => {
            return Response {
                ok: false,
                error: Some(format!("read_link /proc/self/exe: {}", e)),
                active: None,
            };
        }
    };
    let policy_content = build_polkit_policy(&self_path);
    let policy_written = try_write_polkit_policy(policy_content.as_bytes());
    if !policy_written {
        // Check whether the NixOS module already registered the action.
        let nixos_policy = std::path::Path::new(
            "/run/current-system/sw/share/polkit-1/actions/org.vex-vpn.helper.policy",
        );
        if !nixos_policy.exists() {
            return Response {
                ok: false,
                error: Some(
                    "polkit policy could not be written and action not found; \
                     pkexec will not work without a polkit policy"
                        .into(),
                ),
                active: None,
            };
        }
        // NixOS module covers it — continue.
        eprintln!("polkit write skipped: action already registered by NixOS module");
    }

    // 8. Run systemctl daemon-reload.
    let status = std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => {
            return Response {
                ok: false,
                error: Some(format!(
                    "systemctl daemon-reload failed: exit code {:?}",
                    s.code()
                )),
                active: None,
            };
        }
        Err(e) => {
            return Response {
                ok: false,
                error: Some(format!("systemctl daemon-reload: {}", e)),
                active: None,
            };
        }
    }

    Response {
        ok: true,
        error: None,
        active: None,
    }
}

fn handle_uninstall_backend() -> Response {
    // Stop any running instance (ignore errors — may already be stopped).
    let _ = std::process::Command::new("systemctl")
        .args(["stop", "pia-vpn.service"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    // Remove volatile unit file from /run/ (ignore ENOENT).
    let _ = std::fs::remove_file("/run/systemd/system/pia-vpn.service");

    // Remove persistent installer data.
    let _ = std::fs::remove_file("/var/lib/vex-vpn/credentials.env");
    let _ = std::fs::remove_file("/var/lib/vex-vpn/pia-connect.sh");
    let _ = std::fs::remove_file("/var/lib/vex-vpn/pia-disconnect.sh");
    let _ = std::fs::remove_file("/var/lib/vex-vpn/ca.rsa.4096.crt");
    // Remove directory only if empty (don't fail if user added files).
    let _ = std::fs::remove_dir("/var/lib/vex-vpn");

    // Remove polkit policy from either location (ignore errors).
    let _ = std::fs::remove_file("/etc/polkit-1/actions/org.vex-vpn.helper.policy");
    let _ = std::fs::remove_file("/run/polkit-1/actions/org.vex-vpn.helper.policy");

    // Run daemon-reload.
    let status = std::process::Command::new("systemctl")
        .arg("daemon-reload")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    if let Err(e) = status {
        return Response {
            ok: false,
            error: Some(format!("systemctl daemon-reload: {}", e)),
            active: None,
        };
    }

    Response {
        ok: true,
        error: None,
        active: None,
    }
}

/// Re-register the pia-vpn.service unit to /run/systemd/system/ after a
/// reboot erased the volatile unit file. Only proceeds if the persistent
/// installer data at /var/lib/vex-vpn/ is present.
fn handle_reinstall_unit() -> Response {
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
        Ok(s) if s.success() => Response {
            ok: true,
            error: None,
            active: None,
        },
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
