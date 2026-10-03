use crate::dbus::{self, UnitError};
use crate::state::{AppState, Ctx};
use ksni::Tray;
use vex_vpn::vexos::{is_valid_unit_arg, protocol_label, KillSwitchMode, VpnState, AUTO_REGION};

// ---------------------------------------------------------------------------
// Messages sent from the tray thread to the GTK main thread.
// ---------------------------------------------------------------------------

pub enum TrayMessage {
    ShowWindow,
    ShowRegions,
    Quit,
}

// ---------------------------------------------------------------------------
// The tray runs on ksni's own thread and renders from a snapshot that is
// pushed via `Handle::update` after every status poll. Actions are spawned on
// the main Tokio runtime.
// ---------------------------------------------------------------------------

struct VexTray {
    snap: AppState,
    ctx: Ctx,
    tx: async_channel::Sender<TrayMessage>,
}

impl VexTray {
    fn state(&self) -> VpnState {
        self.snap
            .status
            .as_ref()
            .map(|s| s.state)
            .unwrap_or_default()
    }

    fn unit_action(
        &self,
        what: &'static str,
        fut: impl std::future::Future<Output = Result<(), UnitError>> + Send + 'static,
    ) {
        let ctx = self.ctx.clone();
        self.ctx.spawn(async move {
            match fut.await {
                Ok(()) | Err(UnitError::Cancelled) => {}
                Err(e) => {
                    tracing::warn!("tray: {what} failed: {e}");
                    let body = format!("{what} failed: {e}");
                    tokio::task::spawn_blocking(move || {
                        let _ = notify_rust::Notification::new()
                            .appname("vex-vpn")
                            .summary("vex-vpn")
                            .body(&body)
                            .icon("network-vpn-no-route-symbolic")
                            .show();
                    });
                }
            }
            ctx.poke();
        });
    }
}

impl Tray for VexTray {
    fn id(&self) -> String {
        "vex-vpn".to_string()
    }

    fn title(&self) -> String {
        if self.snap.backend.is_none() {
            return "vex-vpn — backend not installed".to_string();
        }
        match &self.snap.status {
            Some(s) if s.state == VpnState::Connected => {
                format!("vex-vpn — {}", s.region_name)
            }
            Some(s) => format!("vex-vpn — {}", s.state.label()),
            None => "vex-vpn".to_string(),
        }
    }

    fn icon_name(&self) -> String {
        match self.state() {
            VpnState::Connected => "network-vpn-symbolic",
            VpnState::Connecting => "network-vpn-acquiring-symbolic",
            VpnState::Error => "network-vpn-no-route-symbolic",
            VpnState::Disconnected | VpnState::Unknown => "network-vpn-disabled-symbolic",
        }
        .to_string()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.tx.try_send(TrayMessage::ShowWindow);
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, StandardItem};

        let open = StandardItem {
            label: "Open vex-vpn".to_string(),
            activate: Box::new(|t: &mut VexTray| {
                let _ = t.tx.try_send(TrayMessage::ShowWindow);
            }),
            ..Default::default()
        };
        let quit = StandardItem {
            label: "Quit".to_string(),
            activate: Box::new(|t: &mut VexTray| {
                let _ = t.tx.try_send(TrayMessage::Quit);
            }),
            ..Default::default()
        };

        let Some(status) = self.snap.status.clone() else {
            let label = if self.snap.backend.is_none() {
                "vexos-vpn backend not installed"
            } else {
                "Status unavailable"
            };
            return vec![
                StandardItem {
                    label: label.to_string(),
                    enabled: false,
                    ..Default::default()
                }
                .into(),
                ksni::MenuItem::Separator,
                open.into(),
                quit.into(),
            ];
        };

        let mut items: Vec<ksni::MenuItem<Self>> = vec![StandardItem {
            label: status.state.label().to_string(),
            enabled: false,
            ..Default::default()
        }
        .into()];

        let unit_active = self.snap.unit_active;
        let needs_sign_in = self.snap.can_sign_in();
        items.push(
            StandardItem {
                label: if unit_active {
                    "Disconnect"
                } else if needs_sign_in {
                    "Sign in to PIA\u{2026}"
                } else {
                    "Connect"
                }
                .to_string(),
                activate: Box::new(move |t: &mut VexTray| {
                    if unit_active {
                        t.unit_action("Disconnect", dbus::stop_unit(dbus::VPN_UNIT));
                    } else if needs_sign_in {
                        let _ = t.tx.try_send(TrayMessage::ShowWindow);
                    } else {
                        t.unit_action("Connect", dbus::start_unit(dbus::VPN_UNIT));
                    }
                }),
                ..Default::default()
            }
            .into(),
        );

        if status.killswitch_mode != KillSwitchMode::Off {
            let on = status.killswitch;
            items.push(
                CheckmarkItem {
                    label: "Kill switch".to_string(),
                    checked: on,
                    activate: Box::new(move |t: &mut VexTray| {
                        if on {
                            t.unit_action(
                                "Kill switch off",
                                dbus::stop_unit(dbus::KILLSWITCH_UNIT),
                            );
                        } else {
                            t.unit_action(
                                "Kill switch on",
                                dbus::start_unit(dbus::KILLSWITCH_UNIT),
                            );
                        }
                    }),
                    ..Default::default()
                }
                .into(),
            );
        }

        let region = if status.region_setting == AUTO_REGION {
            if status.region_name.is_empty() {
                "Automatic (fastest)".to_string()
            } else {
                format!("Automatic \u{2014} {}", status.region_name)
            }
        } else if status.region_name.is_empty() {
            status.region_setting.clone()
        } else {
            status.region_name.clone()
        };
        items.push(region_menu(&self.snap, &status.region_setting, region));
        items.push(protocol_menu(&status.protocol_setting));

        items.push(ksni::MenuItem::Separator);
        items.push(open.into());
        items.push(quit.into());
        items
    }
}

/// Regions offered directly in the tray: the 10 fastest measured ones plus
/// the current choice. Everything else is in the window ("All regions…").
const TRAY_REGIONS: usize = 10;

fn region_menu(snap: &AppState, current: &str, label: String) -> ksni::MenuItem<VexTray> {
    use ksni::menu::{RadioGroup, RadioItem, StandardItem, SubMenu};

    let mut ids = vec![AUTO_REGION.to_string()];
    let mut labels = vec!["Automatic (fastest)".to_string()];
    if let Some(Ok(regions)) = &snap.regions {
        let mut fastest: Vec<_> = regions.iter().filter(|r| r.latency_s.is_some()).collect();
        fastest.sort_by(|a, b| {
            a.latency_s
                .partial_cmp(&b.latency_s)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        fastest.truncate(TRAY_REGIONS);
        if current != AUTO_REGION && !fastest.iter().any(|r| r.id == current) {
            if let Some(r) = regions.iter().find(|r| r.id == current) {
                fastest.push(r);
            }
        }
        for r in fastest {
            ids.push(r.id.clone());
            labels.push(match r.latency_s {
                Some(s) => format!("{} ({:.0} ms)", r.name, s * 1000.0),
                None => r.name.clone(),
            });
        }
    }
    let selected = ids
        .iter()
        .position(|id| id == current)
        .unwrap_or(usize::MAX);

    SubMenu {
        label: format!("Region: {}", label),
        submenu: vec![
            RadioGroup {
                selected,
                select: Box::new(move |t: &mut VexTray, i| {
                    if i == selected {
                        return; // already the current region
                    }
                    let Some(id) = ids.get(i).filter(|id| is_valid_unit_arg(id)).cloned() else {
                        return;
                    };
                    t.unit_action("Change region", async move {
                        dbus::run_oneshot(&dbus::region_unit(&id)).await
                    });
                }),
                options: labels
                    .into_iter()
                    .map(|label| RadioItem {
                        label,
                        ..Default::default()
                    })
                    .collect(),
            }
            .into(),
            ksni::MenuItem::Separator,
            StandardItem {
                label: "All regions\u{2026}".to_string(),
                activate: Box::new(|t: &mut VexTray| {
                    let _ = t.tx.try_send(TrayMessage::ShowRegions);
                }),
                ..Default::default()
            }
            .into(),
        ],
        ..Default::default()
    }
    .into()
}

fn protocol_menu(current: &str) -> ksni::MenuItem<VexTray> {
    use ksni::menu::{RadioGroup, RadioItem, SubMenu};

    const PROTOCOLS: [&str; 2] = ["wireguard", "openvpn"];
    let selected = PROTOCOLS
        .iter()
        .position(|p| *p == current)
        .unwrap_or(usize::MAX);
    SubMenu {
        label: format!("Protocol: {}", protocol_label(current)),
        submenu: vec![RadioGroup {
            selected,
            select: Box::new(move |t: &mut VexTray, i| {
                if i == selected {
                    return; // already the current protocol
                }
                let Some(proto) = PROTOCOLS.get(i) else {
                    return;
                };
                t.unit_action("Change protocol", async move {
                    dbus::run_oneshot(&dbus::protocol_unit(proto)).await
                });
            }),
            options: PROTOCOLS
                .iter()
                .map(|p| RadioItem {
                    label: protocol_label(p).to_string(),
                    ..Default::default()
                })
                .collect(),
        }
        .into()],
        ..Default::default()
    }
    .into()
}

/// Start the tray service and keep its snapshot in sync with `ctx.state`.
pub fn spawn(ctx: Ctx, tx: async_channel::Sender<TrayMessage>) {
    let service = ksni::TrayService::new(VexTray {
        snap: AppState::default(),
        ctx: ctx.clone(),
        tx,
    });
    let handle = service.handle();
    service.spawn();

    let mut changed = ctx.changed.subscribe();
    let state = ctx.state.clone();
    ctx.spawn(async move {
        use tokio::sync::broadcast::error::RecvError;
        while let Ok(()) | Err(RecvError::Lagged(_)) = changed.recv().await {
            let snap = state.read().await.clone();
            handle.update(move |t| t.snap = snap);
        }
    });
}
