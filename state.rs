use anyhow::Result;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, warn};

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    KillSwitchActive, // connected but traffic blocked (kill switch engaged while down)
    Error(String),
}

impl Default for ConnectionStatus {
    fn default() -> Self {
        Self::Disconnected
    }
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

    pub fn css_class(&self) -> &str {
        match self {
            Self::Connected => "status-connected",
            Self::Connecting => "status-connecting",
            Self::KillSwitchActive => "status-killswitch",
            _ => "status-disconnected",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct RegionInfo {
    pub id: String,
    pub name: String,
    pub country: String,
    pub latency_ms: Option<u32>,
    pub port_forward: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectionInfo {
    pub server_ip: String,
    pub peer_ip: String,
    pub external_ip: Option<String>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub connected_since: Option<std::time::Instant>,
}

#[derive(Debug, Clone, Default)]
pub struct AppState {
    pub status: ConnectionStatus,
    pub region: Option<RegionInfo>,
    pub connection: Option<ConnectionInfo>,
    pub kill_switch_enabled: bool,
    pub port_forward_enabled: bool,
    pub forwarded_port: Option<u16>,
    pub auto_connect: bool,
    pub interface: String,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            interface: "wg0".to_string(),
            ..Default::default()
        }
    }
}

// PIA state file schemas (written by tadfisher's systemd service)
#[derive(Deserialize, Debug)]
struct PiaRegionJson {
    id: String,
    name: String,
    country: String,
    port_forward: Option<bool>,
    servers: PiaServers,
}

#[derive(Deserialize, Debug)]
struct PiaServers {
    meta: Vec<PiaMeta>,
    wg: Vec<PiaMeta>,
}

#[derive(Deserialize, Debug)]
struct PiaMeta {
    ip: String,
    cn: String,
}

#[derive(Deserialize, Debug)]
struct PiaWireguardJson {
    server_ip: String,
    peer_ip: String,
}

#[derive(Deserialize, Debug)]
struct PiaPortForwardJson {
    payload: String, // base64 encoded
}

#[derive(Deserialize, Debug)]
struct PiaPortPayload {
    port: u16,
}

pub async fn poll_loop(state: Arc<RwLock<AppState>>) {
    loop {
        match poll_once(&state).await {
            Ok(()) => {}
            Err(e) => warn!("Poll error: {}", e),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn poll_once(state: Arc<RwLock<AppState>>) -> Result<()> {
    let interface = {
        let s = state.read().await;
        s.interface.clone()
    };

    // Check systemd service status via D-Bus
    let svc_status = crate::dbus::get_service_status("pia-vpn.service").await;

    let new_status = match svc_status.as_deref() {
        Ok("active") => ConnectionStatus::Connected,
        Ok("activating") => ConnectionStatus::Connecting,
        Ok("deactivating") => ConnectionStatus::Disconnected,
        Ok("failed") => ConnectionStatus::Error("Service failed".to_string()),
        _ => ConnectionStatus::Disconnected,
    };

    // Read PIA state files from systemd StateDirectory
    let state_dir = "/var/lib/pia-vpn";

    let region = read_region(state_dir).await.ok();
    let wg_info = read_wireguard(state_dir).await.ok();
    let forwarded_port = read_port_forward(state_dir).await.ok().flatten();

    // Read WireGuard interface stats
    let (rx_bytes, tx_bytes) = read_wg_stats(&interface).await.unwrap_or((0, 0));

    // Read kill switch state from nftables
    let kill_switch_active = check_kill_switch().await.unwrap_or(false);

    let mut s = state.write().await;
    s.status = new_status;
    s.region = region;
    s.forwarded_port = forwarded_port;
    s.kill_switch_enabled = kill_switch_active;

    if let Some(wg) = wg_info {
        let conn = s.connection.get_or_insert_with(ConnectionInfo::default);
        conn.server_ip = wg.server_ip;
        conn.peer_ip = wg.peer_ip;
        conn.rx_bytes = rx_bytes;
        conn.tx_bytes = tx_bytes;
    }

    debug!("State poll complete: {:?}", s.status);
    Ok(())
}

async fn read_region(dir: &str) -> Result<RegionInfo> {
    let path = format!("{}/region.json", dir);
    let content = tokio::fs::read_to_string(&path).await?;
    let r: PiaRegionJson = serde_json::from_str(&content)?;
    Ok(RegionInfo {
        id: r.id,
        name: r.name.clone(),
        country: r.country,
        latency_ms: None,
        port_forward: r.port_forward.unwrap_or(false),
    })
}

async fn read_wireguard(dir: &str) -> Result<PiaWireguardJson> {
    let path = format!("{}/wireguard.json", dir);
    let content = tokio::fs::read_to_string(&path).await?;
    Ok(serde_json::from_str(&content)?)
}

async fn read_port_forward(dir: &str) -> Result<Option<u16>> {
    let path = format!("{}/portforward.json", dir);
    let content = tokio::fs::read_to_string(&path).await;
    match content {
        Err(_) => Ok(None),
        Ok(c) => {
            let pf: PiaPortForwardJson = serde_json::from_str(&c)?;
            // payload is base64 encoded JSON
            let decoded = base64_decode(&pf.payload)?;
            let payload: PiaPortPayload = serde_json::from_str(&decoded)?;
            Ok(Some(payload.port))
        }
    }
}

fn base64_decode(input: &str) -> Result<String> {
    use std::io::Read;
    // simple base64 decode without external dep — just call base64 binary
    let output = std::process::Command::new("base64")
        .arg("-d")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()?;
    // For simplicity, use a manual approach
    // In real code, add the `base64` crate to Cargo.toml
    Ok(String::from_utf8(
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("echo '{}' | base64 -d", input))
            .output()?
            .stdout,
    )?)
}

async fn read_wg_stats(interface: &str) -> Result<(u64, u64)> {
    let output = tokio::process::Command::new("wg")
        .args(["show", interface, "transfer"])
        .output()
        .await?;

    let text = String::from_utf8_lossy(&output.stdout);
    // wg show <iface> transfer outputs: <pubkey> <rx_bytes> <tx_bytes>
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
