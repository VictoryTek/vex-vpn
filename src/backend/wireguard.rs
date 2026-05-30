//! WireGuard backend — controls wg-quick@<interface>.service via systemd D-Bus.

use super::{ConnectionInfo, VpnBackend};
use crate::profile::VpnProfile;
use crate::state::ConnectionStatus;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use tracing::debug;

pub struct WireGuardBackend;

#[async_trait]
impl VpnBackend for WireGuardBackend {
    async fn connect(&self, profile: &VpnProfile) -> Result<()> {
        let iface = profile.effective_interface();
        if !crate::config::validate_interface(iface) {
            return Err(anyhow!("invalid interface name: {:?}", iface));
        }
        let unit = format!("wg-quick@{}.service", iface);
        crate::dbus::start_wireguard_unit(iface)
            .await
            .map_err(|e| anyhow!("failed to start {}: {}", unit, e))
    }

    async fn disconnect(&self, profile: &VpnProfile) -> Result<()> {
        let iface = profile.effective_interface();
        crate::dbus::stop_wireguard_unit(iface)
            .await
            .map_err(|e| anyhow!("failed to stop wg-quick@{}.service: {}", iface, e))
    }

    async fn status(&self, profile: &VpnProfile) -> Result<ConnectionStatus> {
        let iface = profile.effective_interface();
        let unit = format!("wg-quick@{}.service", iface);
        match crate::dbus::get_service_status(&unit).await {
            Ok(s) if s == "active" => {
                // Check WireGuard handshake staleness.
                match read_wg_handshake(iface).await {
                    Some(elapsed) if elapsed > 180 => Ok(ConnectionStatus::Stale(elapsed)),
                    _ => Ok(ConnectionStatus::Connected),
                }
            }
            Ok(s) if s == "activating" => Ok(ConnectionStatus::Connecting),
            Ok(s) if s == "failed" => Ok(ConnectionStatus::Error("Service failed".to_string())),
            Ok(_) => Ok(ConnectionStatus::Disconnected),
            Err(e) => {
                debug!("Could not query {} status: {}", unit, e);
                Ok(ConnectionStatus::Disconnected)
            }
        }
    }

    async fn connection_info(&self, profile: &VpnProfile) -> Result<Option<ConnectionInfo>> {
        let iface = profile.effective_interface();
        let (rx, tx) = read_wg_stats(iface).await.unwrap_or((0, 0));

        let local_ip = read_wg_local_ip(iface).await.unwrap_or_default();
        let remote_endpoint = read_wg_endpoint(iface).await.unwrap_or_default();

        if rx == 0 && tx == 0 && local_ip.is_empty() {
            return Ok(None);
        }

        Ok(Some(ConnectionInfo {
            local_ip,
            remote_endpoint,
            rx_bytes: rx,
            tx_bytes: tx,
        }))
    }
}

/// Returns the path to the `wg` binary, preferring the NixOS capability wrapper.
fn wg_binary() -> &'static str {
    if std::path::Path::new("/run/wrappers/bin/wg").exists() {
        "/run/wrappers/bin/wg"
    } else {
        "wg"
    }
}

/// Parse `wg show <iface> transfer` to get RX/TX byte counts.
async fn read_wg_stats(iface: &str) -> Result<(u64, u64)> {
    let output = tokio::process::Command::new(wg_binary())
        .args(["show", iface, "transfer"])
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!("wg show transfer failed for {}", iface);
    }

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let rx = parts[1].parse::<u64>().unwrap_or(0);
            let tx = parts[2].parse::<u64>().unwrap_or(0);
            return Ok((rx, tx));
        }
    }
    Ok((0, 0))
}

/// Parse `wg show <iface> latest-handshakes` and return seconds since last handshake.
pub(crate) async fn read_wg_handshake(iface: &str) -> Option<u64> {
    let output = tokio::process::Command::new(wg_binary())
        .args(["show", iface, "latest-handshakes"])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut latest: Option<u64> = None;
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            if let Ok(ts) = parts[1].parse::<u64>() {
                if ts > 0 {
                    latest = Some(latest.map_or(ts, |prev| prev.max(ts)));
                }
            }
        }
    }

    let ts = latest?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(now.saturating_sub(ts))
}

/// Retrieve the local IP address assigned to the WireGuard interface via `ip address show`.
async fn read_wg_local_ip(iface: &str) -> Option<String> {
    let output = tokio::process::Command::new("ip")
        .args(["address", "show", iface])
        .output()
        .await
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("inet ") {
            if let Some(addr) = trimmed.split_whitespace().nth(1) {
                // Strip CIDR prefix: "10.0.0.2/24" → "10.0.0.2"
                return Some(addr.split('/').next().unwrap_or(addr).to_string());
            }
        }
    }
    None
}

/// Retrieve the remote endpoint for the first peer via `wg show <iface> endpoints`.
async fn read_wg_endpoint(iface: &str) -> Option<String> {
    let output = tokio::process::Command::new(wg_binary())
        .args(["show", iface, "endpoints"])
        .output()
        .await
        .ok()?;

    let text = String::from_utf8_lossy(&output.stdout);
    // Format: <pubkey>\t<endpoint>
    for line in text.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 && parts[1] != "(none)" {
            return Some(parts[1].to_string());
        }
    }
    None
}
