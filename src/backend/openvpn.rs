//! OpenVPN backend — controls VPN connections via NetworkManager D-Bus,
//! with nmcli as a fallback for connections not yet registered in NM.

use super::{ConnectionInfo, VpnBackend};
use crate::profile::VpnProfile;
use crate::state::ConnectionStatus;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::path::Path;
use tracing::debug;

pub struct OpenVpnBackend;

/// Derive the NetworkManager connection name that nmcli assigns when importing
/// a .ovpn file: it uses the file stem (e.g. "vpn" for "vpn.ovpn").
fn nm_conn_name(profile: &VpnProfile) -> String {
    Path::new(&profile.config_file)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&profile.name)
        .to_string()
}

#[async_trait]
impl VpnBackend for OpenVpnBackend {
    async fn connect(&self, profile: &VpnProfile) -> Result<()> {
        // First try activating by the profile UUID (works when previously imported
        // via this code path, because we store the NM-assigned UUID in profile.id
        // only when the profile was registered with NM already).
        if crate::dbus::activate_nm_connection(&profile.id)
            .await
            .is_ok()
        {
            return Ok(());
        }

        // UUID not found in NM — import the .ovpn file via nmcli.
        let config_path = profile.config_path();
        let path_str = config_path
            .to_str()
            .ok_or_else(|| anyhow!("OpenVPN config path is not valid UTF-8"))?;

        let import_status = tokio::process::Command::new("nmcli")
            .args(["connection", "import", "type", "openvpn", "file", path_str])
            .status()
            .await
            .map_err(|e| anyhow!("nmcli not found or not executable: {}", e))?;

        if !import_status.success() {
            return Err(anyhow!(
                "Failed to import OpenVPN profile into NetworkManager (nmcli exited non-zero)"
            ));
        }

        // NM assigns the connection name equal to the file stem.
        let conn_name = nm_conn_name(profile);

        let up_status = tokio::process::Command::new("nmcli")
            .args(["connection", "up", &conn_name])
            .status()
            .await
            .map_err(|e| anyhow!("nmcli connection up failed: {}", e))?;

        if !up_status.success() {
            return Err(anyhow!(
                "Failed to activate OpenVPN connection '{}' via nmcli",
                conn_name
            ));
        }

        Ok(())
    }

    async fn disconnect(&self, profile: &VpnProfile) -> Result<()> {
        // Try D-Bus deactivation first (works when profile.id matches NM UUID).
        if crate::dbus::deactivate_nm_connection(&profile.id)
            .await
            .is_ok()
        {
            return Ok(());
        }

        // Fall back to nmcli using the connection name derived from the file stem.
        let conn_name = nm_conn_name(profile);

        let status = tokio::process::Command::new("nmcli")
            .args(["connection", "down", &conn_name])
            .status()
            .await
            .map_err(|e| anyhow!("nmcli connection down failed: {}", e))?;

        if !status.success() {
            return Err(anyhow!(
                "Failed to deactivate OpenVPN connection '{}' via nmcli",
                conn_name
            ));
        }

        Ok(())
    }

    async fn status(&self, profile: &VpnProfile) -> Result<ConnectionStatus> {
        // Check active NM connections by UUID first.
        match crate::dbus::get_nm_connection_state(&profile.id).await {
            Ok(Some(state)) => return Ok(state),
            Ok(None) => {}
            Err(e) => {
                debug!("NM status query failed for profile {}: {}", profile.id, e);
            }
        }

        // Fall back: check by connection name via nmcli.
        let conn_name = nm_conn_name(profile);
        let output = tokio::process::Command::new("nmcli")
            .args(["-t", "-f", "NAME,STATE", "connection", "show", "--active"])
            .output()
            .await;

        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                // nmcli terse format: "NAME:STATE"
                if let Some((name, state)) = line.split_once(':') {
                    if name == conn_name {
                        return Ok(match state.trim() {
                            "activated" => ConnectionStatus::Connected,
                            "activating" => ConnectionStatus::Connecting,
                            _ => ConnectionStatus::Disconnected,
                        });
                    }
                }
            }
        }

        Ok(ConnectionStatus::Disconnected)
    }

    async fn connection_info(&self, _profile: &VpnProfile) -> Result<Option<ConnectionInfo>> {
        // Traffic statistics for NM-managed connections are not yet implemented.
        // NM exposes RX/TX via org.freedesktop.NetworkManager.Device properties,
        // but correlating them with the VPN profile requires additional D-Bus calls.
        Ok(None)
    }
}
