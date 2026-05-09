use crate::config::Config;
use crate::pia;
use anyhow::Result;
use base64::{engine::general_purpose, Engine};
use futures_util::stream::StreamExt;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use zbus::dbus_proxy;

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
    /// Tunnel is up (systemd active) but WireGuard peer handshake is stale.
    /// Inner value is seconds elapsed since the last handshake.
    Stale(u64),
}

impl ConnectionStatus {
    pub fn label(&self) -> &str {
        match self {
            Self::Disconnected => "Disconnected",
            Self::Connecting => "Connecting...",
            Self::Connected => "Connected",
            Self::KillSwitchActive => "Kill switch active",
            Self::Error(_) => "Error",
            Self::Stale(_) => "Reconnecting\u{2026}",
        }
    }

    pub fn is_connected(&self) -> bool {
        matches!(
            self,
            Self::Connected | Self::KillSwitchActive | Self::Stale(_)
        )
    }

    pub fn is_stale(&self) -> bool {
        matches!(self, Self::Stale(_))
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
    /// Consecutive 3-second poll cycles the status has been Stale.
    pub stale_cycles: u32,
    /// Non-PIA nameservers detected in /etc/resolv.conf while connected (heuristic).
    pub dns_leak_hint: Option<Vec<String>>,
    /// Mirror of Config::auto_reconnect at startup.
    pub auto_reconnect: bool,
    /// Unix timestamp (seconds) when the current connection was established.
    /// Set to Some when status transitions to Connected; cleared on disconnect.
    pub connection_start_ts: Option<u64>,
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
            stale_cycles: 0,
            dns_leak_hint: None,
            auto_reconnect: true,
            connection_start_ts: None,
        }
    }

    pub fn new_with_config(config: &Config) -> Self {
        Self {
            auto_connect: config.auto_connect,
            interface: config.interface.clone(),
            selected_region_id: config.selected_region_id.clone(),
            auto_reconnect: config.auto_reconnect,
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

pub async fn poll_loop(
    state: Arc<RwLock<AppState>>,
    state_change_tx: tokio::sync::broadcast::Sender<()>,
) {
    let mut prev_status = ConnectionStatus::Disconnected;
    loop {
        match poll_once(&state).await {
            Ok(()) => {}
            Err(e) => warn!("Poll error: {}", e),
        }
        let new_status = state.read().await.status.clone();

        // Stale cycle tracking and auto-restart watchdog.
        if new_status.is_stale() {
            let mut s = state.write().await;
            s.stale_cycles += 1;
            if s.stale_cycles >= 10 {
                // 10 × 3 s = 30 s in Stale → trigger restart
                s.stale_cycles = 0;
                drop(s);
                info!("Handshake watchdog: restarting pia-vpn.service");
                if let Err(e) = crate::dbus::restart_vpn_unit().await {
                    warn!("Watchdog restart failed: {}", e);
                }
            }
        } else {
            state.write().await.stale_cycles = 0;
        }

        // Fire desktop notification only on variant-level status change
        // (avoids spurious spawns while Stale(n) ticks up each cycle).
        if std::mem::discriminant(&new_status) != std::mem::discriminant(&prev_status) {
            // Broadcast state change to tray and other subscribers.
            let _ = state_change_tx.send(());

            let old = prev_status.clone();
            let new = new_status.clone();
            let region = state.read().await.region.as_ref().map(|r| r.name.clone());
            tokio::task::spawn_blocking(move || {
                notify_status_change(&old, &new, region.as_deref())
            });

            // Transition: was connected, now disconnecting/error → write history record.
            if matches!(
                prev_status,
                ConnectionStatus::Connected
                    | ConnectionStatus::KillSwitchActive
                    | ConnectionStatus::Stale(_)
            ) && !new_status.is_connected()
            {
                let s = state.read().await;
                if let Some(ts_start) = s.connection_start_ts {
                    let reason = match &new_status {
                        ConnectionStatus::Error(_) => "error",
                        ConnectionStatus::Disconnected => "user",
                        _ => "network",
                    };
                    let entry = crate::history::HistoryEntry {
                        ts_start,
                        ts_end: std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                        region: s
                            .region
                            .as_ref()
                            .map(|r| r.name.clone())
                            .unwrap_or_default(),
                        bytes_rx: s.connection.as_ref().map(|c| c.rx_bytes).unwrap_or(0),
                        bytes_tx: s.connection.as_ref().map(|c| c.tx_bytes).unwrap_or(0),
                        disconnect_reason: reason.to_string(),
                    };
                    drop(s);
                    tokio::task::spawn_blocking(move || crate::history::append_entry(&entry));
                }
                state.write().await.connection_start_ts = None;
            }

            // Transition: now connected → record start time.
            if new_status.is_connected()
                && !matches!(
                    prev_status,
                    ConnectionStatus::Connected
                        | ConnectionStatus::KillSwitchActive
                        | ConnectionStatus::Stale(_)
                )
            {
                state.write().await.connection_start_ts = Some(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                );
            }
        }
        prev_status = new_status;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

/// Send a desktop notification when the VPN connection status changes.
fn notify_status_change(old: &ConnectionStatus, new: &ConnectionStatus, region: Option<&str>) {
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
            if matches!(
                old,
                ConnectionStatus::Connected
                    | ConnectionStatus::KillSwitchActive
                    | ConnectionStatus::Stale(_)
            ) =>
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

pub(crate) async fn poll_once(state: &Arc<RwLock<AppState>>) -> Result<()> {
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

    // Handshake watchdog — upgrade Connected → Stale when handshake is stale.
    let new_status = if matches!(new_status, ConnectionStatus::Connected) {
        match read_wg_handshake(&interface).await {
            Some(elapsed) if elapsed > 180 => ConnectionStatus::Stale(elapsed),
            _ => ConnectionStatus::Connected,
        }
    } else {
        new_status
    };

    let region = region_raw.ok();
    let wg_info = wg_info_raw.ok();
    let forwarded_port = forwarded_port_raw.unwrap_or(None);
    let (rx_bytes, tx_bytes) = wg_stats_raw.unwrap_or((0, 0));
    let kill_switch_active = kill_switch_raw.unwrap_or(false);
    let pf_active = pf_status_raw.map(|s| s == "active").unwrap_or(false);

    // DNS leak heuristic — only meaningful when connected.
    let dns_leak_hint = if new_status.is_connected() {
        check_dns_leak_hint()
    } else {
        None
    };

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
    s.dns_leak_hint = dns_leak_hint;

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
// Helper — wg binary path
// ---------------------------------------------------------------------------

/// Returns the path to the `wg` binary, preferring the NixOS capability wrapper
/// at `/run/wrappers/bin/wg` (which has `CAP_NET_ADMIN` set via `security.wrappers`).
/// Falls back to `wg` in PATH for non-NixOS environments.
fn wg_binary() -> &'static str {
    if std::path::Path::new("/run/wrappers/bin/wg").exists() {
        "/run/wrappers/bin/wg"
    } else {
        "wg"
    }
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
    let output = tokio::process::Command::new(wg_binary())
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
            let rx = parts[1].parse::<u64>().unwrap_or_else(|_| {
                warn!("wg show transfer: malformed rx value {:?}", parts[1]);
                0
            });
            let tx = parts[2].parse::<u64>().unwrap_or_else(|_| {
                warn!("wg show transfer: malformed tx value {:?}", parts[2]);
                0
            });
            return Ok((rx, tx));
        }
    }
    Ok((0, 0))
}

/// Parse `wg show <iface> latest-handshakes`.
/// Returns the number of seconds elapsed since the most recent peer handshake,
/// or `None` if no handshake has occurred yet or the command fails.
async fn read_wg_handshake(interface: &str) -> Option<u64> {
    let output = tokio::process::Command::new(wg_binary())
        .args(["show", interface, "latest-handshakes"])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    // Format: <pubkey>\t<unix_timestamp>  (timestamp is 0 if no handshake)
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

/// Heuristic DNS leak check: returns any non-PIA nameservers found in
/// `/etc/resolv.conf`, or `None` if no potential leak is detected.
/// This does NOT probe live DNS traffic — it is a best-effort hint only.
pub(crate) fn check_dns_leak_hint() -> Option<Vec<String>> {
    let content = std::fs::read_to_string("/etc/resolv.conf").ok()?;
    let non_pia: Vec<String> = content
        .lines()
        .filter(|l| l.starts_with("nameserver"))
        .filter_map(|l| l.split_whitespace().nth(1))
        .filter(|ip| {
            // Allow PIA DNS range, loopback, and link-local.
            !ip.starts_with("10.0.0.") && !ip.starts_with("127.") && *ip != "::1"
        })
        .map(|s| s.to_string())
        .collect();
    if non_pia.is_empty() {
        None
    } else {
        Some(non_pia)
    }
}

// ---------------------------------------------------------------------------
// Background watcher tasks (B3, F7)
// ---------------------------------------------------------------------------

// Local proxy definitions used only by the watcher tasks.
// TODO(D): consolidate with dbus.rs proxies once the public API surface is
// stabilised.  WatcherSystemdManager intentionally exposes only `load_unit`
// (not start_unit/stop_unit), so a direct merge requires method-set changes.
// WatcherSystemdUnit and WatcherNetworkManager are identical to their dbus.rs
// counterparts but the generated proxy names differ; keep separate until the
// dbus.rs types are exported pub and their proxy names made accessible here.

#[dbus_proxy(
    interface = "org.freedesktop.systemd1.Manager",
    default_service = "org.freedesktop.systemd1",
    default_path = "/org/freedesktop/systemd1"
)]
trait WatcherSystemdManager {
    fn load_unit(&self, name: &str) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}

#[dbus_proxy(
    interface = "org.freedesktop.systemd1.Unit",
    default_service = "org.freedesktop.systemd1"
)]
trait WatcherSystemdUnit {
    #[dbus_proxy(property)]
    fn active_state(&self) -> zbus::Result<String>;
}

#[dbus_proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait WatcherNetworkManager {
    #[dbus_proxy(signal)]
    fn state_changed(&self, state: u32) -> zbus::Result<()>;
}

/// Subscribe to `PropertiesChanged` on pia-vpn.service and trigger an immediate
/// `poll_once()` whenever `ActiveState` changes.  This eliminates worst-case
/// 3 s staleness on connect/disconnect.
pub async fn watch_vpn_unit_state(
    state: Arc<RwLock<AppState>>,
    state_change_tx: tokio::sync::broadcast::Sender<()>,
) {
    let conn = match crate::dbus::system_conn().await {
        Ok(c) => c,
        Err(e) => {
            warn!("unit watch: D-Bus unavailable: {}", e);
            return;
        }
    };

    let manager = match WatcherSystemdManagerProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            warn!("unit watch: manager proxy failed: {}", e);
            return;
        }
    };

    let unit_path = match manager.load_unit("pia-vpn.service").await {
        Ok(p) => p,
        Err(e) => {
            warn!(
                "unit watch: load_unit failed (unit may not be loaded yet): {}",
                e
            );
            return;
        }
    };

    let unit = match WatcherSystemdUnitProxy::builder(&conn)
        .path(unit_path.as_ref())
        .map_err(anyhow::Error::from)
    {
        Ok(b) => match b.build().await {
            Ok(p) => p,
            Err(e) => {
                warn!("unit watch: unit proxy build failed: {}", e);
                return;
            }
        },
        Err(e) => {
            warn!("unit watch: unit proxy path failed: {}", e);
            return;
        }
    };

    let mut stream = unit.receive_active_state_changed().await;
    while stream.next().await.is_some() {
        // Don't write directly — trigger a full consistent poll.
        match poll_once(&state).await {
            Ok(()) => {
                let _ = state_change_tx.send(());
                debug!("PropertiesChanged triggered poll");
            }
            Err(e) => warn!("Triggered poll error: {}", e),
        }
    }
    warn!("unit watch: ActiveState stream ended unexpectedly");
}

/// Subscribe to NetworkManager `StateChanged` and restart the VPN when
/// connectivity is restored.  Exits gracefully if NM is unavailable.
pub async fn watch_network_manager(state: Arc<RwLock<AppState>>) {
    let conn = match crate::dbus::system_conn().await {
        Ok(c) => c,
        Err(e) => {
            warn!("NM watch: D-Bus unavailable: {}", e);
            return;
        }
    };

    let proxy = match WatcherNetworkManagerProxy::new(&conn).await {
        Ok(p) => p,
        Err(e) => {
            info!(
                "NM watch: NetworkManager proxy unavailable (NM may not be running): {}",
                e
            );
            return;
        }
    };

    let mut stream = match proxy.receive_state_changed().await {
        Ok(s) => s,
        Err(e) => {
            info!("NM watch: StateChanged subscribe failed: {}", e);
            return;
        }
    };

    let mut prev_nm_state: u32 = 0;
    while let Some(msg) = stream.next().await {
        let args = match msg.args() {
            Ok(a) => a,
            Err(_) => continue,
        };
        let new_nm_state = args.state;

        // React only when transitioning TO fully connected FROM a lower state.
        let was_disconnected = prev_nm_state != crate::dbus::NM_CONNECTED_GLOBAL;
        let now_connected = new_nm_state == crate::dbus::NM_CONNECTED_GLOBAL;

        if now_connected && was_disconnected {
            let auto_reconnect = state.read().await.auto_reconnect;
            let vpn_is_connected = state.read().await.status.is_connected();

            if auto_reconnect && vpn_is_connected {
                info!("Network restored \u{2014} debouncing VPN reconnect (2 s)");
                tokio::time::sleep(Duration::from_secs(2)).await;
                // Re-check: VPN still connected after debounce?
                if state.read().await.status.is_connected() {
                    info!("Auto-reconnect: restarting pia-vpn.service");
                    if let Err(e) = crate::dbus::restart_vpn_unit().await {
                        warn!("Auto-reconnect failed: {}", e);
                    }
                }
            }
        }
        prev_nm_state = new_nm_state;
    }
    warn!("NM watch: StateChanged stream ended unexpectedly");
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
