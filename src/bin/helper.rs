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
use std::process::Stdio;

use serde::{Deserialize, Serialize};

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
    // NixOS places nft here; /usr/sbin/nft is the FHS fallback.
    if std::path::Path::new("/run/current-system/sw/bin/nft").exists() {
        "/run/current-system/sw/bin/nft"
    } else {
        "/usr/sbin/nft"
    }
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
