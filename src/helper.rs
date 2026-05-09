//! Kill switch management via the polkit-gated `vex-vpn-helper` binary.
//!
//! The helper runs as root via `pkexec`. This module serialises commands to
//! JSON, writes them to the helper's stdin, and reads the JSON response from
//! its stdout using `tokio::process`.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Path to the helper binary.
/// NixOS installs it via `environment.pathsToLink = ["/libexec"]`.
/// Dev builds fall back to searching PATH.
fn helper_path() -> &'static str {
    const NIXOS_PATH: &str = "/run/current-system/sw/libexec/vex-vpn-helper";
    if std::path::Path::new(NIXOS_PATH).exists() {
        NIXOS_PATH
    } else {
        "vex-vpn-helper"
    }
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
    let config = crate::config::Config::load().unwrap_or_else(|e| {
        tracing::warn!("Failed to load config: {e:#}");
        crate::config::Config::default()
    });
    let resp = call_helper(&HelperRequest {
        op: "enable_kill_switch",
        interface: Some(interface),
        allowed_interfaces: Some(&config.kill_switch_allowed_ifaces),
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
