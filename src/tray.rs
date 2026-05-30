use crate::state::{AppState, ConnectionStatus};
use ksni::Tray;
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Messages sent from the tray thread to the GTK main thread.
// ---------------------------------------------------------------------------

pub enum TrayMessage {
    ShowWindow,
    Quit,
}

// ---------------------------------------------------------------------------
// The tray runs on its own OS thread. It holds a Handle to the main Tokio
// runtime so that spawned D-Bus tasks are driven by the main runtime's worker
// threads rather than a stranded single-threaded runtime.
// ---------------------------------------------------------------------------

struct VexTray {
    state: Arc<RwLock<AppState>>,
    handle: tokio::runtime::Handle,
    tx: async_channel::Sender<TrayMessage>,
}

impl VexTray {
    fn read_state(&self) -> AppState {
        self.handle
            .block_on(async { self.state.read().await.clone() })
    }
}

impl Tray for VexTray {
    fn id(&self) -> String {
        "vex-vpn".to_string()
    }

    fn title(&self) -> String {
        let s = self.read_state();
        match &s.status {
            ConnectionStatus::Connected => s
                .active_profile()
                .map(|p| format!("vex-vpn — {}", p.name))
                .unwrap_or_else(|| "vex-vpn — Connected".to_string()),
            ConnectionStatus::Stale(_) => "vex-vpn — Reconnecting\u{2026}".to_string(),
            other => format!("vex-vpn — {}", other.label()),
        }
    }

    fn icon_name(&self) -> String {
        let s = self.read_state();
        match s.status {
            ConnectionStatus::Connected => "network-vpn-symbolic",
            ConnectionStatus::Connecting => "network-vpn-acquiring-symbolic",
            ConnectionStatus::Stale(_) => "network-vpn-acquiring-symbolic",
            ConnectionStatus::KillSwitchActive => "network-vpn-no-route-symbolic",
            _ => "network-vpn-disabled-symbolic",
        }
        .to_string()
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let s = self.read_state();
        let is_connected = s.status.is_connected();
        let is_connecting = matches!(s.status, ConnectionStatus::Connecting);
        let profile_iface = s
            .active_profile()
            .map(|p| p.effective_interface().to_string())
            .unwrap_or_else(|| "wg0".to_string());

        vec![
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Open vex-vpn".to_string(),
                activate: Box::new(|tray: &mut VexTray| {
                    let _ = tray.tx.try_send(TrayMessage::ShowWindow);
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: if is_connected || is_connecting {
                    "Disconnect".to_string()
                } else {
                    "Connect".to_string()
                },
                activate: Box::new(move |tray: &mut VexTray| {
                    let iface = profile_iface.clone();
                    if is_connected || is_connecting {
                        tray.handle.spawn(async move {
                            if let Err(e) = crate::dbus::stop_wireguard_unit(&iface).await {
                                tracing::error!("disconnect failed: {}", e);
                            }
                        });
                    } else {
                        tray.handle.spawn(async move {
                            if let Err(e) = crate::dbus::start_wireguard_unit(&iface).await {
                                tracing::error!("connect failed: {}", e);
                            }
                        });
                    }
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Quit".to_string(),
                activate: Box::new(|tray: &mut VexTray| {
                    let _ = tray.tx.try_send(TrayMessage::Quit);
                }),
                ..Default::default()
            }),
        ]
    }
}

pub fn run_tray(
    state: Arc<RwLock<AppState>>,
    tx: async_channel::Sender<TrayMessage>,
    handle: tokio::runtime::Handle,
    mut state_change_rx: tokio::sync::broadcast::Receiver<()>,
) {
    let tray = VexTray {
        state,
        handle: handle.clone(),
        tx,
    };

    ksni::TrayService::new(tray).spawn();

    handle.block_on(async move {
        use tokio::sync::broadcast::error::RecvError;
        while let Ok(()) | Err(RecvError::Lagged(_)) = state_change_rx.recv().await {}
    });
}
