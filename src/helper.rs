//! Kill switch management via the polkit-gated `vex-vpn-helper` binary.
//!
//! The helper runs as root via `pkexec`. This module serialises commands to
//! JSON, writes them to the helper's stdin, and reads the JSON response from
//! its stdout using `tokio::process`.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

/// Resolve the path to the helper binary.
/// Checks several Nix profile and install locations before falling back to PATH.
fn helper_path() -> String {
    use std::path::Path;
    // 1. NixOS system profile (module-installed).
    if Path::new("/run/current-system/sw/libexec/vex-vpn-helper").exists() {
        return "/run/current-system/sw/libexec/vex-vpn-helper".to_owned();
    }
    // 2. User Nix profile (nix profile install).
    if let Ok(home) = std::env::var("HOME") {
        let p = format!("{}/.nix-profile/libexec/vex-vpn-helper", home);
        if Path::new(&p).exists() {
            return p;
        }
    }
    // 3. System-level Nix profile.
    if Path::new("/nix/var/nix/profiles/default/libexec/vex-vpn-helper").exists() {
        return "/nix/var/nix/profiles/default/libexec/vex-vpn-helper".to_owned();
    }
    // 4. Sibling libexec/ of current binary (covers `nix run` store path).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin_dir) = exe.parent() {
            // binary at $out/bin/vex-vpn → helper at $out/libexec/vex-vpn-helper
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
    // 5. PATH fallback (dev builds).
    "vex-vpn-helper".to_owned()
}

#[derive(Serialize)]
struct HelperRequest<'a> {
    op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    interface: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allowed_interfaces: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pia_user: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pia_pass: Option<&'a str>,
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
        pia_user: None,
        pia_pass: None,
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
        pia_user: None,
        pia_pass: None,
    })
    .await?;
    if resp.ok {
        Ok(())
    } else {
        bail!("helper error: {}", resp.error.unwrap_or_default())
    }
}

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
    if resp.ok {
        Ok(())
    } else {
        bail!("helper error: {}", resp.error.unwrap_or_default())
    }
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
    if resp.ok {
        Ok(())
    } else {
        bail!("helper error: {}", resp.error.unwrap_or_default())
    }
}
