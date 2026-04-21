use crate::state::{format_bytes, AppState, ConnectionStatus};
use crate::tray::TrayMessage;
use gtk4::prelude::*;
use libadwaita as adw;
use adw::prelude::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------

const APP_CSS: &str = r#"
window.pia-window { background-color: #0d1117; }

.pia-sidebar {
    background-color: #0a0f16;
    border-right: 1px solid rgba(255,255,255,0.06);
}

.connect-btn {
    border-radius: 9999px;
    min-width: 152px;
    min-height: 152px;
    padding: 0;
    transition: all 200ms ease;
}
.connect-btn.state-disconnected {
    background: #0f1923;
    border: 2px solid rgba(0,195,137,0.3);
    color: #00c389;
}
.connect-btn.state-disconnected:hover {
    border-color: rgba(0,195,137,0.7);
    box-shadow: 0 0 32px rgba(0,195,137,0.15);
}
.connect-btn.state-connected {
    background: #00291b;
    border: 2px solid #00c389;
    color: #00c389;
    box-shadow: 0 0 40px rgba(0,195,137,0.2);
}
.connect-btn.state-connecting {
    background: #12120a;
    border: 2px solid rgba(255,180,0,0.5);
    color: #ffb400;
}

.status-pill {
    border-radius: 9999px;
    padding: 4px 14px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: .09em;
}
.status-pill.state-connected    { background: rgba(0,195,137,.10); color: #00c389; }
.status-pill.state-disconnected { background: rgba(255,255,255,.06); color: rgba(255,255,255,.4); }
.status-pill.state-connecting   { background: rgba(255,180,0,.10); color: #ffb400; }
.status-pill.state-error        { background: rgba(255,80,80,.10); color: #ff5050; }

.stat-card {
    background: #111c2a;
    border: 1px solid rgba(255,255,255,.06);
    border-radius: 9px;
    padding: 11px 13px;
}
.stat-label {
    font-size: 10px;
    color: rgba(255,255,255,.28);
    letter-spacing: .09em;
}
.stat-value {
    font-size: 14px;
    font-weight: 500;
    color: rgba(255,255,255,.85);
    font-family: monospace;
}
.stat-value.green { color: #00c389; }

.section-title {
    font-size: 10px;
    font-weight: 600;
    letter-spacing: .10em;
    color: rgba(255,255,255,.22);
    margin-bottom: 6px;
}

.nav-btn {
    border-radius: 8px;
    min-height: 42px;
    color: rgba(255,255,255,.4);
    font-size: 13px;
}
.nav-btn:hover { background: rgba(255,255,255,.05); color: white; }
.nav-btn.active { background: rgba(0,195,137,.08); color: #00c389; }

.hero-location { font-size: 17px; font-weight: 600; color: #fff; }
.hero-ip { font-size: 12px; color: rgba(255,255,255,.3); font-family: monospace; }
.port-badge {
    background: rgba(0,195,137,.12);
    color: #00c389;
    border-radius: 5px;
    padding: 1px 7px;
    font-size: 11px;
    font-family: monospace;
    font-weight: 600;
}
"#;

// ---------------------------------------------------------------------------
// Shared widget handles updated by the refresh timer
// ---------------------------------------------------------------------------

struct LiveWidgets {
    status_pill: gtk4::Label,
    connect_btn: gtk4::Button,
    btn_icon: gtk4::Image,
    btn_label: gtk4::Label,
    location_label: gtk4::Label,
    ip_label: gtk4::Label,
    dl_value: gtk4::Label,
    ul_value: gtk4::Label,
    lat_value: gtk4::Label,
    port_value: gtk4::Label,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn build_ui(
    app: &adw::Application,
    state: Arc<RwLock<AppState>>,
    rx: Option<std::sync::mpsc::Receiver<TrayMessage>>,
) {
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(APP_CSS);
    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("no display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Private Internet Access")
        .default_width(760)
        .default_height(540)
        .resizable(false)
        .build();
    window.add_css_class("pia-window");

    let root = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    root.append(&build_sidebar());

    let initial_auto_connect = {
        // Read synchronously — at startup before the async runtime is loaded.
        crate::config::Config::load().auto_connect
    };

    let (main_page, live) = build_main_page(state.clone(), initial_auto_connect);
    root.append(&main_page);

    window.set_content(Some(&root));

    // Drain the tray→window channel and raise the window on ShowWindow.
    if let Some(rx) = rx {
        let window_ref = window.clone();
        glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    TrayMessage::ShowWindow => window_ref.present(),
                    TrayMessage::Quit => std::process::exit(0),
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // Refresh UI every 3 seconds from the poll loop state.
    glib::timeout_add_seconds_local(3, move || {
        let state = state.clone();
        let live = LiveWidgets {
            status_pill: live.status_pill.clone(),
            connect_btn: live.connect_btn.clone(),
            btn_icon: live.btn_icon.clone(),
            btn_label: live.btn_label.clone(),
            location_label: live.location_label.clone(),
            ip_label: live.ip_label.clone(),
            dl_value: live.dl_value.clone(),
            ul_value: live.ul_value.clone(),
            lat_value: live.lat_value.clone(),
            port_value: live.port_value.clone(),
        };
        glib::spawn_future_local(async move {
            let s = state.read().await.clone();
            refresh_widgets(&live, &s);
        });
        glib::ControlFlow::Continue
    });

    window.present();
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

fn build_sidebar() -> gtk4::Box {
    let sidebar = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    sidebar.add_css_class("pia-sidebar");
    sidebar.set_size_request(192, -1);

    // Logo
    let logo_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    logo_row.set_margin_top(22);
    logo_row.set_margin_start(18);
    logo_row.set_margin_bottom(20);

    let logo_img = gtk4::Image::from_icon_name("network-vpn-symbolic");
    logo_img.set_pixel_size(22);

    let logo_lbl = gtk4::Label::new(Some("Private Internet Access"));
    logo_lbl.set_css_classes(&["section-title"]);
    logo_lbl.set_wrap(true);
    logo_lbl.set_max_width_chars(16);
    logo_lbl.set_halign(gtk4::Align::Start);

    logo_row.append(&logo_img);
    logo_row.append(&logo_lbl);
    sidebar.append(&logo_row);

    // Nav items: (icon-name, label, active)
    let nav_items = [
        ("go-home-symbolic", "Dashboard", true),
        ("network-server-symbolic", "Servers", false),
        ("preferences-system-symbolic", "Settings", false),
    ];

    for (icon, label, active) in &nav_items {
        let btn = gtk4::Button::new();
        btn.add_css_class("nav-btn");
        if *active {
            btn.add_css_class("active");
        }
        btn.set_margin_start(8);
        btn.set_margin_end(8);
        btn.set_margin_bottom(2);

        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
        row.set_margin_start(8);

        let img = gtk4::Image::from_icon_name(icon);
        img.set_pixel_size(16);

        let lbl = gtk4::Label::new(Some(label));
        lbl.set_halign(gtk4::Align::Start);
        lbl.set_hexpand(true);

        row.append(&img);
        row.append(&lbl);
        btn.set_child(Some(&row));
        sidebar.append(&btn);
    }

    let spacer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    sidebar.append(&spacer);

    sidebar
}

// ---------------------------------------------------------------------------
// Main page
// ---------------------------------------------------------------------------

fn build_main_page(
    state: Arc<RwLock<AppState>>,
    initial_auto_connect: bool,
) -> (gtk4::Box, LiveWidgets) {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    page.set_margin_top(28);
    page.set_margin_bottom(28);
    page.set_margin_start(28);
    page.set_margin_end(28);
    page.set_hexpand(true);

    // ── Hero ─────────────────────────────────────────────────────────────

    let hero = gtk4::Box::new(gtk4::Orientation::Vertical, 14);
    hero.set_halign(gtk4::Align::Center);
    hero.set_margin_bottom(28);

    let status_pill = gtk4::Label::new(Some("● DISCONNECTED"));
    status_pill.set_css_classes(&["status-pill", "state-disconnected"]);
    status_pill.set_halign(gtk4::Align::Center);
    hero.append(&status_pill);

    // Connect button
    let connect_btn = gtk4::Button::new();
    connect_btn.set_css_classes(&["connect-btn", "state-disconnected"]);
    connect_btn.set_halign(gtk4::Align::Center);

    let btn_inner = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    btn_inner.set_halign(gtk4::Align::Center);
    btn_inner.set_valign(gtk4::Align::Center);

    let btn_icon = gtk4::Image::from_icon_name("network-vpn-disabled-symbolic");
    btn_icon.set_pixel_size(28);

    let btn_label = gtk4::Label::new(Some("CONNECT"));
    btn_label.set_css_classes(&["section-title"]);

    btn_inner.append(&btn_icon);
    btn_inner.append(&btn_label);
    connect_btn.set_child(Some(&btn_inner));

    // Button click — read current state, then toggle.
    {
        let state_c = state.clone();
        let pill_c = status_pill.clone();
        let btn_c = connect_btn.clone();
        let lbl_c = btn_label.clone();
        let icon_c = btn_icon.clone();

        connect_btn.connect_clicked(move |_| {
            let state = state_c.clone();
            let pill = pill_c.clone();
            let btn = btn_c.clone();
            let lbl = lbl_c.clone();
            let icon = icon_c.clone();

            glib::spawn_future_local(async move {
                let current = state.read().await.status.clone();
                match current {
                    ConnectionStatus::Connected | ConnectionStatus::KillSwitchActive => {
                        pill.set_label("● DISCONNECTING...");
                        set_state_class(&pill, "state-connecting");
                        set_state_class(&btn, "state-connecting");
                        lbl.set_label("CANCEL");
                        icon.set_icon_name(Some("network-vpn-acquiring-symbolic"));

                        if let Err(e) = crate::dbus::disconnect_vpn().await {
                            tracing::error!("disconnect: {}", e);
                        }
                    }
                    ConnectionStatus::Connecting => {
                        let _ = crate::dbus::disconnect_vpn().await;
                    }
                    _ => {
                        pill.set_label("● CONNECTING...");
                        set_state_class(&pill, "state-connecting");
                        set_state_class(&btn, "state-connecting");
                        lbl.set_label("CANCEL");
                        icon.set_icon_name(Some("network-vpn-acquiring-symbolic"));

                        if let Err(e) = crate::dbus::connect_vpn().await {
                            tracing::error!("connect: {}", e);
                        }
                    }
                }
            });
        });
    }

    hero.append(&connect_btn);

    let location_label = gtk4::Label::new(Some("Select a server"));
    location_label.set_css_classes(&["hero-location"]);
    location_label.set_halign(gtk4::Align::Center);

    let ip_label = gtk4::Label::new(Some("—"));
    ip_label.set_css_classes(&["hero-ip"]);
    ip_label.set_halign(gtk4::Align::Center);

    hero.append(&location_label);
    hero.append(&ip_label);
    page.append(&hero);

    // ── Stat cards ────────────────────────────────────────────────────────

    let stats_grid = gtk4::Grid::new();
    stats_grid.set_column_spacing(8);
    stats_grid.set_row_spacing(8);
    stats_grid.set_column_homogeneous(true);
    stats_grid.set_margin_bottom(22);

    let (dl_card, dl_value) = make_stat_card("DOWNLOAD", "0 B", false);
    let (ul_card, ul_value) = make_stat_card("UPLOAD", "0 B", false);
    let (lat_card, lat_value) = make_stat_card("LATENCY", "— ms", false);
    let (port_card, port_value) = make_stat_card("PORT FWD", "—", true);

    stats_grid.attach(&dl_card, 0, 0, 1, 1);
    stats_grid.attach(&ul_card, 1, 0, 1, 1);
    stats_grid.attach(&lat_card, 2, 0, 1, 1);
    stats_grid.attach(&port_card, 3, 0, 1, 1);
    page.append(&stats_grid);

    // ── Feature toggles ───────────────────────────────────────────────────

    let feat_title = gtk4::Label::new(Some("FEATURES"));
    feat_title.set_css_classes(&["section-title"]);
    feat_title.set_halign(gtk4::Align::Start);
    page.append(&feat_title);

    let feats = gtk4::Box::new(gtk4::Orientation::Vertical, 4);

    // Kill switch
    {
        let state_c = state.clone();
        feats.append(&make_toggle_row(
            "network-vpn-symbolic",
            "Kill Switch",
            "Block all traffic if VPN drops",
            false,
            move |active| {
                let state = state_c.clone();
                glib::spawn_future_local(async move {
                    let iface = state.read().await.interface.clone();
                    let res = if active {
                        crate::dbus::apply_kill_switch(&iface).await
                    } else {
                        crate::dbus::remove_kill_switch().await
                    };
                    if let Err(e) = res {
                        tracing::error!("kill switch toggle: {}", e);
                    }
                });
            },
        ));
    }

    // Port forwarding
    {
        feats.append(&make_toggle_row(
            "network-transmit-receive-symbolic",
            "Port Forwarding",
            "Allow inbound connections through VPN",
            false,
            move |active| {
                glib::spawn_future_local(async move {
                    let res = if active {
                        crate::dbus::enable_port_forward().await
                    } else {
                        crate::dbus::disable_port_forward().await
                    };
                    if let Err(e) = res {
                        tracing::error!("port forward toggle: {}", e);
                    }
                });
            },
        ));
    }

    // Auto connect — persisted via config.toml
    feats.append(&make_toggle_row(
        "system-run-symbolic",
        "Auto Connect",
        "Connect on graphical login",
        initial_auto_connect,
        move |active| {
            let mut cfg = crate::config::Config::load();
            cfg.auto_connect = active;
            if let Err(e) = cfg.save() {
                tracing::error!("Failed to save config: {}", e);
            }
        },
    ));

    page.append(&feats);

    let live = LiveWidgets {
        status_pill,
        connect_btn,
        btn_icon,
        btn_label,
        location_label,
        ip_label,
        dl_value,
        ul_value,
        lat_value,
        port_value,
    };

    (page, live)
}

// ---------------------------------------------------------------------------
// Widget helpers
// ---------------------------------------------------------------------------

/// Replace all state-* CSS classes on a widget, then add the new one.
fn set_state_class<W: IsA<gtk4::Widget>>(widget: &W, new_class: &str) {
    for cls in ["state-connected", "state-disconnected", "state-connecting", "state-error"] {
        widget.remove_css_class(cls);
    }
    widget.add_css_class(new_class);
}

/// Refresh all live widgets from the current AppState.
fn refresh_widgets(live: &LiveWidgets, s: &AppState) {
    let (pill_text, state_class, btn_text, btn_icon_name) = match &s.status {
        ConnectionStatus::Connected => (
            "● CONNECTED",
            "state-connected",
            "DISCONNECT",
            "network-vpn-symbolic",
        ),
        ConnectionStatus::Connecting => (
            "● CONNECTING...",
            "state-connecting",
            "CANCEL",
            "network-vpn-acquiring-symbolic",
        ),
        ConnectionStatus::KillSwitchActive => (
            "● KILL SWITCH ACTIVE",
            "state-error",
            "RECONNECT",
            "network-vpn-no-route-symbolic",
        ),
        ConnectionStatus::Error(_) => (
            "● ERROR",
            "state-error",
            "RETRY",
            "network-vpn-disabled-symbolic",
        ),
        ConnectionStatus::Disconnected => (
            "● DISCONNECTED",
            "state-disconnected",
            "CONNECT",
            "network-vpn-disabled-symbolic",
        ),
    };

    live.status_pill.set_label(pill_text);
    set_state_class(&live.status_pill, state_class);
    set_state_class(&live.connect_btn, state_class);
    live.btn_label.set_label(btn_text);
    live.btn_icon.set_icon_name(Some(btn_icon_name));

    // Location / IP
    if let Some(region) = &s.region {
        live.location_label.set_label(&region.name);
    } else {
        live.location_label.set_label(if s.status.is_connected() {
            "Connected"
        } else {
            "Select a server"
        });
    }

    if let Some(conn) = &s.connection {
        live.ip_label.set_label(&conn.peer_ip);
        live.dl_value.set_label(&format_bytes(conn.rx_bytes));
        live.ul_value.set_label(&format_bytes(conn.tx_bytes));
    } else {
        live.ip_label.set_label("—");
        live.dl_value.set_label("0 B");
        live.ul_value.set_label("0 B");
    }

    // Latency
    match s.latency_ms {
        Some(ms) => live.lat_value.set_label(&format!("{} ms", ms)),
        None => live.lat_value.set_label("— ms"),
    }

    // Port forwarding
    match s.forwarded_port {
        Some(port) => live.port_value.set_label(&port.to_string()),
        None => live.port_value.set_label("—"),
    }
}

fn make_stat_card(label: &str, default: &str, green: bool) -> (gtk4::Box, gtk4::Label) {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    card.add_css_class("stat-card");
    card.set_hexpand(true);

    let lbl = gtk4::Label::new(Some(label));
    lbl.set_css_classes(&["stat-label"]);
    lbl.set_halign(gtk4::Align::Start);

    let val = gtk4::Label::new(Some(default));
    val.set_css_classes(if green {
        &["stat-value", "green"] as &[&str]
    } else {
        &["stat-value"]
    });
    val.set_halign(gtk4::Align::Start);

    card.append(&lbl);
    card.append(&val);
    (card, val)
}

fn make_toggle_row(
    icon: &str,
    title: &str,
    subtitle: &str,
    default: bool,
    on_toggle: impl Fn(bool) + 'static,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(title);
    row.set_subtitle(subtitle);

    let img = gtk4::Image::from_icon_name(icon);
    img.set_pixel_size(16);
    row.add_prefix(&img);

    let sw = gtk4::Switch::new();
    sw.set_active(default);
    sw.set_valign(gtk4::Align::Center);
    sw.connect_active_notify(move |s| on_toggle(s.is_active()));
    row.add_suffix(&sw);
    row.set_activatable_widget(Some(&sw));

    row
}
