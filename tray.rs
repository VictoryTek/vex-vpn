use crate::state::{AppState, ConnectionStatus};
use ksni::{self, Tray};
use std::sync::Arc;
use tokio::sync::RwLock;

struct PiaTray {
    state: Arc<RwLock<AppState>>,
}

impl Tray for PiaTray {
    fn id(&self) -> String {
        "pia-gui".to_string()
    }

    fn title(&self) -> String {
        let rt = tokio::runtime::Handle::current();
        let s = rt.block_on(async { self.state.read().await.clone() });
        match &s.status {
            ConnectionStatus::Connected => {
                if let Some(r) = &s.region {
                    format!("PIA — {}", r.name)
                } else {
                    "PIA — Connected".to_string()
                }
            }
            other => format!("PIA — {}", other.label()),
        }
    }

    fn icon_name(&self) -> String {
        let rt = tokio::runtime::Handle::current();
        let s = rt.block_on(async { self.state.read().await.clone() });
        match s.status {
            ConnectionStatus::Connected => "network-vpn-symbolic",
            ConnectionStatus::Connecting => "network-vpn-acquiring-symbolic",
            ConnectionStatus::KillSwitchActive => "network-vpn-no-route-symbolic",
            _ => "network-vpn-disabled-symbolic",
        }
        .to_string()
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        let rt = tokio::runtime::Handle::current();
        let s = rt.block_on(async { self.state.read().await.clone() });

        let connect_label = match s.status {
            ConnectionStatus::Connected | ConnectionStatus::Connecting => "Disconnect",
            _ => "Connect",
        };

        vec![
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: connect_label.to_string(),
                activate: Box::new(|tray: &mut PiaTray| {
                    let rt = tokio::runtime::Handle::current();
                    rt.spawn(async {
                        let _ = crate::dbus::connect_vpn().await;
                    });
                }),
                ..Default::default()
            }),
            ksni::MenuItem::Separator,
            ksni::MenuItem::Standard(ksni::menu::StandardItem {
                label: "Open PIA".to_string(),
                activate: Box::new(|_| {
                    // The GTK app window is already running; just present it
                    // We'd emit a signal here in a real implementation
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

pub fn run_tray(state: Arc<RwLock<AppState>>) {
    let tray = PiaTray { state };
    // ksni runs its own event loop on this thread
    if let Err(e) = ksni::run(tray) {
        tracing::warn!("Tray failed (may not be supported on this DE): {}", e);
    }
}
