use crate::config::Config;
use crate::profile::VpnProfile;
use anyhow::Result;
use futures_util::stream::StreamExt;
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
    /// Tunnel is up but WireGuard peer handshake is stale.
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
pub struct ConnectionInfo {
    pub local_ip: String,
    pub remote_endpoint: String,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub status: ConnectionStatus,
    pub active_profile_id: Option<String>,
    pub profiles: Vec<VpnProfile>,
    pub connection: Option<ConnectionInfo>,
    pub kill_switch_enabled: bool,
    pub auto_reconnect: bool,
    pub stale_cycles: u32,
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
            active_profile_id: None,
            profiles: Vec::new(),
            connection: None,
            kill_switch_enabled: false,
            auto_reconnect: true,
            stale_cycles: 0,
            connection_start_ts: None,
        }
    }

    pub fn new_with_config(config: &Config) -> Self {
        Self {
            active_profile_id: config.active_profile_id.clone(),
            profiles: config.profiles.clone(),
            auto_reconnect: config.auto_reconnect,
            ..Self::new()
        }
    }

    /// Return the currently active profile from the profiles list.
    pub fn active_profile(&self) -> Option<&VpnProfile> {
        self.active_profile_id
            .as_deref()
            .and_then(|id| self.profiles.iter().find(|p| p.id == id))
    }
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
                s.stale_cycles = 0;
                let iface = s
                    .active_profile()
                    .map(|p| p.effective_interface().to_string())
                    .unwrap_or_else(|| "wg0".to_string());
                drop(s);
                info!("Handshake watchdog: restarting wg-quick@{}.service", iface);
                if let Err(e) = crate::dbus::restart_wireguard_unit(&iface).await {
                    warn!("Watchdog restart failed: {}", e);
                }
            }
        } else {
            state.write().await.stale_cycles = 0;
        }

        // Fire desktop notification only on variant-level status change.
        if std::mem::discriminant(&new_status) != std::mem::discriminant(&prev_status) {
            let _ = state_change_tx.send(());

            let old = prev_status.clone();
            let new = new_status.clone();
            let profile_name = state.read().await.active_profile().map(|p| p.name.clone());
            tokio::task::spawn_blocking(move || {
                notify_status_change(&old, &new, profile_name.as_deref())
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
                        profile_name: s
                            .active_profile()
                            .map(|p| p.name.clone())
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

fn notify_status_change(
    old: &ConnectionStatus,
    new: &ConnectionStatus,
    profile_name: Option<&str>,
) {
    use notify_rust::{Notification, Urgency};
    let result = match new {
        ConnectionStatus::Connected => {
            let body = profile_name
                .map(|n| format!("Connected to {}", n))
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
    let active_profile = {
        let s = state.read().await;
        s.active_profile().cloned()
    };

    let Some(profile) = active_profile else {
        let mut s = state.write().await;
        s.status = ConnectionStatus::Disconnected;
        s.connection = None;
        return Ok(());
    };

    let backend = crate::backend::backend_for_profile(&profile);

    let (new_status_res, conn_info_res) =
        tokio::join!(backend.status(&profile), backend.connection_info(&profile),);

    let new_status = match new_status_res {
        Ok(s) => s,
        Err(e) => {
            debug!("Backend status error: {}", e);
            ConnectionStatus::Disconnected
        }
    };

    let conn_info = conn_info_res.unwrap_or(None);
    let kill_switch_active = check_kill_switch().await.unwrap_or(false);

    let mut s = state.write().await;
    s.status = new_status;
    s.kill_switch_enabled = kill_switch_active;

    if let Some(info) = conn_info {
        let c = s.connection.get_or_insert_with(ConnectionInfo::default);
        c.local_ip = info.local_ip;
        c.remote_endpoint = info.remote_endpoint;
        c.rx_bytes = info.rx_bytes;
        c.tx_bytes = info.tx_bytes;
    } else if !s.status.is_connected() {
        s.connection = None;
    }

    debug!("State poll: {:?}", s.status);
    Ok(())
}

async fn check_kill_switch() -> Result<bool> {
    let output = tokio::process::Command::new("nft")
        .args(["list", "table", "inet", "vex_kill_switch"])
        .output()
        .await?;
    Ok(output.status.success())
}

// ---------------------------------------------------------------------------
// Background watcher tasks
// ---------------------------------------------------------------------------

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

pub async fn watch_vpn_unit_state(
    state: Arc<RwLock<AppState>>,
    state_change_tx: tokio::sync::broadcast::Sender<()>,
) {
    let iface = {
        let s = state.read().await;
        s.active_profile()
            .map(|p| p.effective_interface().to_string())
            .unwrap_or_else(|| "wg0".to_string())
    };

    let unit_name = format!("wg-quick@{}.service", iface);

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

    let unit_path = match manager.load_unit(&unit_name).await {
        Ok(p) => p,
        Err(e) => {
            warn!("unit watch: load_unit({}) failed: {}", unit_name, e);
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

        let was_disconnected = prev_nm_state != crate::dbus::NM_CONNECTED_GLOBAL;
        let now_connected = new_nm_state == crate::dbus::NM_CONNECTED_GLOBAL;

        if now_connected && was_disconnected {
            let s = state.read().await;
            let auto_reconnect = s.auto_reconnect;
            let vpn_connected = s.status.is_connected();
            let iface = s
                .active_profile()
                .map(|p| p.effective_interface().to_string());
            drop(s);

            if auto_reconnect && vpn_connected {
                if let Some(iface) = iface {
                    info!("Network restored — debouncing VPN reconnect (2 s)");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    if state.read().await.status.is_connected() {
                        info!("Auto-reconnect: restarting wg-quick@{}.service", iface);
                        if let Err(e) = crate::dbus::restart_wireguard_unit(&iface).await {
                            warn!("Auto-reconnect failed: {}", e);
                        }
                    }
                }
            }
        }
        prev_nm_state = new_nm_state;
    }
    warn!("NM watch: StateChanged stream ended unexpectedly");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1_048_576), "1.0 MiB");
    }

    #[test]
    fn test_connection_status_label() {
        assert_eq!(ConnectionStatus::Disconnected.label(), "Disconnected");
        assert_eq!(ConnectionStatus::Connected.label(), "Connected");
    }

    #[test]
    fn test_connection_status_is_connected() {
        assert!(!ConnectionStatus::Disconnected.is_connected());
        assert!(ConnectionStatus::Connected.is_connected());
        assert!(ConnectionStatus::KillSwitchActive.is_connected());
        assert!(ConnectionStatus::Stale(0).is_connected());
    }

    #[test]
    fn test_app_state_default() {
        let s = AppState::new();
        assert_eq!(s.status, ConnectionStatus::Disconnected);
        assert!(s.active_profile_id.is_none());
        assert!(s.profiles.is_empty());
    }
}
