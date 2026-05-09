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

struct PiaTray {
    state: Arc<RwLock<AppState>>,
    handle: tokio::runtime::Handle,
    tx: async_channel::Sender<TrayMessage>,
}

impl PiaTray {
    fn read_state(&self) -> AppState {
        self.handle
            .block_on(async { self.state.read().await.clone() })
    }
}

impl Tray for PiaTray {
    fn id(&self) -> String {
        "vex-vpn".to_string()
    }

    fn title(&self) -> String {
        let s = self.read_state();
        match &s.status {
            ConnectionStatus::Connected => s
                .region
                .as_ref()
                .map(|r| format!("PIA — {}", r.name))
                .unwrap_or_else(|| "PIA — Connected".to_string()),
            ConnectionStatus::Stale(_) => "PIA — Reconnecting…".to_string(),
            other => format!("PIA — {}", other.label()),
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

        vec![
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Open PIA".to_string(),
                activate: Box::new(|tray: &mut PiaTray| {
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
                activate: Box::new(move |tray: &mut PiaTray| {
                    if is_connected || is_connecting {
                        tray.handle.spawn(async {
                            if let Err(e) = crate::dbus::disconnect_vpn().await {
                                tracing::error!("disconnect failed: {}", e);
                            }
                        });
                    } else {
                        tray.handle.spawn(async {
                            if let Err(e) = crate::dbus::connect_vpn().await {
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
                activate: Box::new(|tray: &mut PiaTray| {
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
    let tray = PiaTray {
        state,
        handle: handle.clone(),
        tx,
    };

    // ksni 0.2.x TrayService::spawn() returns () — there is no handle to call update() on.
    // The tray reads AppState live via read_state() on every menu open, so status changes
    // are always reflected. We still drain the broadcast receiver so the sender never blocks.
    ksni::TrayService::new(tray).spawn();

    handle.block_on(async move {
        use tokio::sync::broadcast::error::RecvError;
        // Drain broadcast signals — tray reads state live on menu open, so no action needed.
        while let Ok(()) | Err(RecvError::Lagged(_)) = state_change_rx.recv().await {}
    });
}
