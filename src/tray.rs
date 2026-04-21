use crate::state::{AppState, ConnectionStatus};
use ksni::Tray;
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Messages sent from the tray thread to the GTK main thread.
// ---------------------------------------------------------------------------

pub enum TrayMessage {
    ShowWindow,
    #[allow(dead_code)]
    Quit,
}

// ---------------------------------------------------------------------------
// The tray runs on its own OS thread with its own Tokio runtime so it never
// blocks the GTK main thread and never panics from Handle::current() being
// unavailable.
// ---------------------------------------------------------------------------

struct PiaTray {
    state: Arc<RwLock<AppState>>,
    rt: tokio::runtime::Runtime,
    tx: std::sync::mpsc::SyncSender<TrayMessage>,
}

impl PiaTray {
    fn read_state(&self) -> AppState {
        self.rt.block_on(async { self.state.read().await.clone() })
    }
}

impl Tray for PiaTray {
    fn id(&self) -> String {
        "pia-gui".to_string()
    }

    fn title(&self) -> String {
        let s = self.read_state();
        match &s.status {
            ConnectionStatus::Connected => s
                .region
                .as_ref()
                .map(|r| format!("PIA — {}", r.name))
                .unwrap_or_else(|| "PIA — Connected".to_string()),
            other => format!("PIA — {}", other.label()),
        }
    }

    fn icon_name(&self) -> String {
        let s = self.read_state();
        match s.status {
            ConnectionStatus::Connected => "network-vpn-symbolic",
            ConnectionStatus::Connecting => "network-vpn-acquiring-symbolic",
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
                    let _ = tray.tx.send(TrayMessage::ShowWindow);
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
                        tray.rt.spawn(async {
                            if let Err(e) = crate::dbus::disconnect_vpn().await {
                                tracing::error!("disconnect failed: {}", e);
                            }
                        });
                    } else {
                        tray.rt.spawn(async {
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
                activate: Box::new(|_| {
                    std::process::exit(0);
                }),
                ..Default::default()
            }),
        ]
    }
}

pub fn run_tray(state: Arc<RwLock<AppState>>, tx: std::sync::mpsc::SyncSender<TrayMessage>) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("Failed to create tray runtime: {}", e);
            return;
        }
    };

    let tray = PiaTray { state, rt, tx };

    if let Err(e) = ksni::TrayService::new(tray).run() {
        tracing::warn!(
            "System tray unavailable (may not be supported on this desktop): {}",
            e
        );
    }
}
