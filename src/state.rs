use anyhow::Result;
use base64::{engine::general_purpose, Engine};
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, warn};
use crate::config::Config;
use crate::pia;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default)]
pub enum ConnectionStatus {
    #[default]
    Disconnected,
    Connecting,
    Connected,
    #[allow(dead_code)]
    KillSwitchActive,
    Error(String),
}

impl ConnectionStatus {
    pub fn label(&self) -> &str {
        match self {
            Self::Disconnected => "Disconnected",
            Self::Connecting => "Connecting...",
            Self::Connected => "Connected",
            Self::KillSwitchActive => "Kill switch active",
            Self::Error(_) => "Error",
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(self, Self::Connected | Self::KillSwitchActive)
    }
}

#[derive(Debug, Clone, Default)]
pub struct RegionInfo {
    #[allow(dead_code)]
    pub id: String,
    pub name: String,
    #[allow(dead_code)]
    pub country: String,
    #[allow(dead_code)]
    pub port_forward: bool,
    pub meta_ip: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionInfo {
    pub server_ip: String,
    pub peer_ip: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub status: ConnectionStatus,
    pub region: Option<RegionInfo>,
    pub connection: Option<ConnectionInfo>,
    pub kill_switch_enabled: bool,
    pub port_forward_enabled: bool,
    pub forwarded_port: Option<u16>,
    #[allow(dead_code)]
    pub auto_connect: bool,
    pub interface: String,
    pub latency_ms: Option<u32>,
    /// PIA authentication token (memory-only, never persisted).
    pub auth_token: Option<pia::AuthToken>,
    /// Full PIA server list from the v6 API.
    pub regions: Vec<pia::Region>,
    /// User-selected region ID (persisted via Config).
    pub selected_region_id: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            status: ConnectionStatus::Disconnected,
            region: None,
            connection: None,
            kill_switch_enabled: false,
            port_forward_enabled: false,
            forwarded_port: None,
            auto_connect: false,
            interface: "wg0".to_string(),
            latency_ms: None,
            auth_token: None,
            regions: Vec::new(),
            selected_region_id: None,
        }
    }

    pub fn new_with_config(config: &Config) -> Self {
        Self {
            auto_connect: config.auto_connect,
            interface: config.interface.clone(),
            selected_region_id: config.selected_region_id.clone(),
            ..Self::new()
        }
    }
}

// ---------------------------------------------------------------------------
// PIA JSON schemas (written by tadfisher's systemd service)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Debug)]
struct PiaRegionJson {
    id: String,
    name: String,
    country: String,
    port_forward: Option<bool>,
    servers: Option<PiaRegionServers>,
}

#[derive(Deserialize, Debug)]
struct PiaRegionServers {
    meta: Option<Vec<PiaServerEntry>>,
}

#[derive(Deserialize, Debug)]
struct PiaServerEntry {
    ip: String,
}

#[derive(Deserialize, Debug)]
struct PiaWireguardJson {
    server_ip: String,
    peer_ip: String,
}

#[derive(Deserialize, Debug)]
struct PiaPortForwardJson {
    payload: String,
}

#[derive(Deserialize, Debug)]
struct PiaPortPayload {
    port: u16,
}

// ---------------------------------------------------------------------------
// Poll loop
// ---------------------------------------------------------------------------

pub async fn poll_loop(state: Arc<RwLock<AppState>>) {
    let mut prev_status = ConnectionStatus::Disconnected;
    loop {
        match poll_once(&state).await {
            Ok(()) => {}
            Err(e) => warn!("Poll error: {}", e),
        }
        let new_status = state.read().await.status.clone();
        if new_status != prev_status {
            let old = prev_status.clone();
            let new = new_status.clone();
            let region = state
                .read()
                .await
                .region
                .as_ref()
                .map(|r| r.name.clone());
            // Fire-and-forget: spawn_blocking so D-Bus notification call
            // does not stall the poll loop.
            tokio::task::spawn_blocking(move || notify_status_change(&old, &new, region.as_deref()));
            prev_status = new_status;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Send a desktop notification when the VPN connection status changes.
fn notify_status_change(
    old: &ConnectionStatus,
    new: &ConnectionStatus,
    region: Option<&str>,
) {
    use notify_rust::{Notification, Urgency};
    let result = match new {
        ConnectionStatus::Connected => {
            let body = region
                .map(|r| format!("Connected to {}", r))
                .unwrap_or_else(|| "Connected".to_string());
            Notification::new()
                .summary("vex-vpn")
                .body(&body)
                .icon("network-vpn-symbolic")
                .show()
        }
        ConnectionStatus::Disconnected
            if matches!(old, ConnectionStatus::Connected | ConnectionStatus::KillSwitchActive) =>
        {
            Notification::new()
                .summary("vex-vpn")
                .body("Disconnected")
                .icon("network-vpn-disabled-symbolic")
                .show()
        }
        ConnectionStatus::Error(msg) => Notification::new()
            .summary("vex-vpn — Connection Error")
            .body(msg)
            .icon("network-vpn-disabled-symbolic")
            .urgency(Urgency::Critical)
            .show(),
        _ => return,
    };
    if let Err(e) = result {
        warn!("Failed to send desktop notification: {}", e);
    }
}

async fn poll_once(state: &Arc<RwLock<AppState>>) -> Result<()> {
    let interface = {
        let s = state.read().await;
        s.interface.clone()
    };

    let state_dir = "/var/lib/pia-vpn"; // systemd StateDirectory (no DynamicUser)

    // Run all 7 independent I/O operations concurrently.
    let (
        vpn_status_raw,
        region_raw,
        wg_info_raw,
        forwarded_port_raw,
        wg_stats_raw,
        kill_switch_raw,
        pf_status_raw,
    ) = tokio::join!(
        crate::dbus::get_service_status("pia-vpn.service"),
        read_region(state_dir),
        read_wireguard(state_dir),
        read_port_forward(state_dir),
        read_wg_stats(&interface),
        check_kill_switch(),
        crate::dbus::get_service_status("pia-vpn-portforward.service"),
    );

    let new_status = match vpn_status_raw {
        Ok(s) if s == "active" => ConnectionStatus::Connected,
        Ok(s) if s == "activating" => ConnectionStatus::Connecting,
        Ok(s) if s == "failed" => ConnectionStatus::Error("Service failed".to_string()),
        Ok(_) => ConnectionStatus::Disconnected,
        Err(e) => {
            debug!("Could not query service status: {}", e);
            ConnectionStatus::Disconnected
        }
    };

    let region = region_raw.ok();
    let wg_info = wg_info_raw.ok();
    let forwarded_port = forwarded_port_raw.unwrap_or(None);
    let (rx_bytes, tx_bytes) = wg_stats_raw.unwrap_or((0, 0));
    let kill_switch_active = kill_switch_raw.unwrap_or(false);
    let pf_active = pf_status_raw.map(|s| s == "active").unwrap_or(false);

    // Measure latency to the PIA meta server when connected.
    let latency_ms = if new_status.is_connected() {
        if let Some(ref reg) = region {
            if let Some(ref ip) = reg.meta_ip {
                measure_latency(ip).await
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    let mut s = state.write().await;
    s.status = new_status;
    s.region = region;
    s.forwarded_port = forwarded_port;
    s.kill_switch_enabled = kill_switch_active;
    s.port_forward_enabled = pf_active;
    s.latency_ms = latency_ms;

    if let Some(wg) = wg_info {
        let conn = s.connection.get_or_insert_with(ConnectionInfo::default);
        conn.server_ip = wg.server_ip;
        conn.peer_ip = wg.peer_ip;
        conn.rx_bytes = rx_bytes;
        conn.tx_bytes = tx_bytes;
    } else if !s.status.is_connected() {
        // Clear stale connection info when disconnected.
        s.connection = None;
    }

    debug!("State poll: {:?}", s.status);
    Ok(())
}

// ---------------------------------------------------------------------------
// File readers
// ---------------------------------------------------------------------------

async fn read_region(dir: &str) -> Result<RegionInfo> {
    let content = tokio::fs::read_to_string(format!("{}/region.json", dir)).await?;
    let r: PiaRegionJson = serde_json::from_str(&content)?;
    let meta_ip = r
        .servers
        .as_ref()
        .and_then(|s| s.meta.as_ref())
        .and_then(|m| m.first())
        .map(|e| e.ip.clone());
    Ok(RegionInfo {
        id: r.id,
        name: r.name,
        country: r.country,
        port_forward: r.port_forward.unwrap_or(false),
        meta_ip,
    })
}

async fn read_wireguard(dir: &str) -> Result<PiaWireguardJson> {
    let content = tokio::fs::read_to_string(format!("{}/wireguard.json", dir)).await?;
    Ok(serde_json::from_str(&content)?)
}

async fn read_port_forward(dir: &str) -> Result<Option<u16>> {
    let path = format!("{}/portforward.json", dir);
    match tokio::fs::read_to_string(&path).await {
        Err(_) => Ok(None),
        Ok(content) => {
            let pf: PiaPortForwardJson = serde_json::from_str(&content)?;
            let port = decode_port_payload(&pf.payload)?;
            Ok(Some(port))
        }
    }
}

/// Decode a base64-encoded PIA port-forward payload and return the port number.
/// Extracted as a pure function so it can be unit-tested without I/O.
pub(crate) fn decode_port_payload(payload: &str) -> Result<u16> {
    let decoded_bytes = general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|e| anyhow::anyhow!("base64 decode error: {}", e))?;
    let decoded = String::from_utf8(decoded_bytes)?;
    let p: PiaPortPayload = serde_json::from_str(&decoded)?;
    Ok(p.port)
}

async fn read_wg_stats(interface: &str) -> Result<(u64, u64)> {
    let output = tokio::process::Command::new("wg")
        .args(["show", interface, "transfer"])
        .output()
        .await?;

    if !output.status.success() {
        anyhow::bail!("wg show failed");
    }

    let text = String::from_utf8_lossy(&output.stdout);
    // Output format: <pubkey>\t<rx_bytes>\t<tx_bytes>
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

async fn check_kill_switch() -> Result<bool> {
    let output = tokio::process::Command::new("nft")
        .args(["list", "table", "inet", "pia_kill_switch"])
        .output()
        .await?;
    Ok(output.status.success())
}

/// TCP-connect to port 443 of the given IP and return round-trip time in ms.
/// Returns `None` on timeout or connection failure.
async fn measure_latency(ip: &str) -> Option<u32> {
    let addr = format!("{}:443", ip);
    let start = std::time::Instant::now();
    let result = tokio::time::timeout(
        Duration::from_millis(2000),
        tokio::net::TcpStream::connect(addr.as_str()),
    )
    .await;
    match result {
        Ok(Ok(_)) => Some(start.elapsed().as_millis() as u32),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1_073_741_824), "1.00 GiB");
    }

    #[test]
    fn test_connection_status_label() {
        assert_eq!(ConnectionStatus::Disconnected.label(), "Disconnected");
        assert_eq!(ConnectionStatus::Connecting.label(), "Connecting...");
        assert_eq!(ConnectionStatus::Connected.label(), "Connected");
        assert_eq!(
            ConnectionStatus::KillSwitchActive.label(),
            "Kill switch active"
        );
        assert_eq!(ConnectionStatus::Error("boom".to_string()).label(), "Error");
    }

    #[test]
    fn test_connection_status_is_connected() {
        assert!(ConnectionStatus::Connected.is_connected());
        assert!(ConnectionStatus::KillSwitchActive.is_connected());
        assert!(!ConnectionStatus::Disconnected.is_connected());
        assert!(!ConnectionStatus::Connecting.is_connected());
        assert!(!ConnectionStatus::Error("x".to_string()).is_connected());
    }

    #[test]
    fn test_port_forward_decode() {
        // Construct a base64-encoded JSON payload: {"port":54821,"expires_at":"..."}
        let inner = r#"{"port":54821,"expires_at":"2024-01-01T00:00:00Z"}"#;
        let payload = general_purpose::STANDARD.encode(inner);
        let port = decode_port_payload(&payload).unwrap();
        assert_eq!(port, 54821);
    }
}
