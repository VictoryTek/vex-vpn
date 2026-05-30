//! Kill switch management via the polkit-gated `vex-vpn-helper` binary.
//!
//! The helper runs as root via `pkexec`. This module serialises commands to
//! JSON, writes them to the helper's stdin, and reads the JSON response from
//! its stdout using `tokio::process`.
#![allow(dead_code)]

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Resolve the path to the helper binary.
fn helper_path() -> String {
    use std::path::Path;
    if Path::new("/run/current-system/sw/libexec/vex-vpn-helper").exists() {
        return "/run/current-system/sw/libexec/vex-vpn-helper".to_owned();
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = format!("{}/.nix-profile/libexec/vex-vpn-helper", home);
        if Path::new(&p).exists() {
            return p;
        }
    }
    if Path::new("/nix/var/nix/profiles/default/libexec/vex-vpn-helper").exists() {
        return "/nix/var/nix/profiles/default/libexec/vex-vpn-helper".to_owned();
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
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
    "vex-vpn-helper".to_owned()
}

#[derive(Serialize)]
struct HelperRequest<'a> {
    op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    interface: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_interfaces: Option<&'a [String]>,
}

#[derive(Deserialize)]
struct HelperResponse {
    ok: bool,
    error: Option<String>,
    #[allow(dead_code)]
    active: Option<bool>,
}

async fn call_helper(req: &HelperRequest<'_>) -> Result<HelperResponse> {
    let mut child = Command::new("pkexec")
        .arg(helper_path())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("spawn pkexec: {}", e))?;

    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow::anyhow!("helper stdin unavailable"))?;
    let line = serde_json::to_string(req).map_err(|e| anyhow::anyhow!("serialize: {}", e))? + "\n";
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| anyhow::anyhow!("write to helper: {}", e))?;
    drop(stdin);

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("helper stdout unavailable"))?;
    let mut reader = BufReader::new(stdout).lines();
    let response_line = reader
        .next_line()
        .await
        .map_err(|e| anyhow::anyhow!("read from helper: {}", e))?
        .unwrap_or_default();

    child
        .wait()
        .await
        .map_err(|e| anyhow::anyhow!("wait for helper: {}", e))?;

    let resp: HelperResponse = serde_json::from_str(&response_line)
        .map_err(|_| anyhow::anyhow!("helper returned invalid JSON: {:?}", response_line))?;

    Ok(resp)
}

/// Enable the nftables kill switch for the given WireGuard interface.
pub async fn apply_kill_switch(interface: &str) -> Result<()> {
    if !crate::config::validate_interface(interface) {
        bail!("invalid interface name: {:?}", interface);
    }
    let allowed: Vec<String> = vec!["lo".to_string()];
    let resp = call_helper(&HelperRequest {
        op: "enable_kill_switch",
        interface: Some(interface),
        allowed_interfaces: Some(&allowed),
    })
    .await?;
    if resp.ok {
        Ok(())
    } else {
        bail!("helper error: {}", resp.error.unwrap_or_default())
    }
}

/// Remove the nftables kill switch.
pub async fn remove_kill_switch() -> Result<()> {
    let resp = call_helper(&HelperRequest {
        op: "disable_kill_switch",
        interface: None,
        allowed_interfaces: None,
    })
    .await?;
    if resp.ok {
        Ok(())
    } else {
        bail!("helper error: {}", resp.error.unwrap_or_default())
    }
}
