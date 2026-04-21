use crate::state::{AppState, ConnectionStatus, format_bytes};
use gtk4::prelude::*;
use libadwaita as adw;
use adw::prelude::*;
use std::sync::Arc;
use tokio::sync::RwLock;
use glib::clone;

const APP_CSS: &str = r#"
window.pia-window {
    background-color: #0d1117;
}

.pia-sidebar {
    background-color: #0d1117;
    border-right: 1px solid rgba(255,255,255,0.06);
}

.pia-content {
    background-color: #0d1117;
}

/* Hero connection button */
.connect-btn {
    border-radius: 9999px;
    min-width: 160px;
    min-height: 160px;
    font-size: 14px;
    font-weight: 600;
    letter-spacing: 0.08em;
    transition: all 200ms ease;
}

.connect-btn.disconnected {
    background: linear-gradient(145deg, #1a2332, #0f1923);
    border: 2px solid rgba(0, 195, 137, 0.3);
    color: #00c389;
}

.connect-btn.disconnected:hover {
    background: linear-gradient(145deg, #1e2a3a, #131f2e);
    border-color: rgba(0, 195, 137, 0.6);
    box-shadow: 0 0 32px rgba(0, 195, 137, 0.15);
}

.connect-btn.connected {
    background: linear-gradient(145deg, #003d28, #00291b);
    border: 2px solid rgba(0, 195, 137, 0.8);
    color: #00c389;
    box-shadow: 0 0 48px rgba(0, 195, 137, 0.2);
}

.connect-btn.connected:hover {
    background: linear-gradient(145deg, #004d32, #003322);
    border-color: #00c389;
}

.connect-btn.connecting {
    background: linear-gradient(145deg, #1a1a0d, #12120a);
    border: 2px solid rgba(255, 180, 0, 0.5);
    color: #ffb400;
}

/* Status pill */
.status-pill {
    border-radius: 9999px;
    padding: 4px 12px;
    font-size: 12px;
    font-weight: 600;
    letter-spacing: 0.06em;
}

.status-pill.status-connected {
    background-color: rgba(0, 195, 137, 0.12);
    color: #00c389;
}

.status-pill.status-disconnected {
    background-color: rgba(255,255,255,0.06);
    color: rgba(255,255,255,0.4);
}

.status-pill.status-connecting {
    background-color: rgba(255, 180, 0, 0.12);
    color: #ffb400;
}

/* Server row */
.server-row {
    background-color: #131b27;
    border: 1px solid rgba(255,255,255,0.06);
    border-radius: 12px;
    padding: 12px 16px;
    margin: 4px 0;
}

.server-row:hover {
    background-color: #1a2435;
    border-color: rgba(255,255,255,0.1);
}

/* Stat cards */
.stat-card {
    background-color: #131b27;
    border: 1px solid rgba(255,255,255,0.06);
    border-radius: 10px;
    padding: 12px 14px;
}

.stat-label {
    font-size: 11px;
    color: rgba(255,255,255,0.35);
    letter-spacing: 0.08em;
    text-transform: uppercase;
}

.stat-value {
    font-size: 15px;
    font-weight: 500;
    color: rgba(255,255,255,0.85);
    font-family: monospace;
}

/* Toggle rows */
.feature-row {
    background-color: #131b27;
    border: 1px solid rgba(255,255,255,0.06);
    border-radius: 10px;
}

.feature-row:not(:first-child) {
    margin-top: 4px;
}

/* Sidebar nav */
.nav-btn {
    border-radius: 8px;
    min-height: 44px;
    color: rgba(255,255,255,0.45);
    font-size: 13px;
}

.nav-btn:hover,
.nav-btn.active {
    background-color: rgba(255,255,255,0.06);
    color: white;
}

/* Typography */
.hero-ip {
    font-size: 13px;
    font-family: monospace;
    color: rgba(255,255,255,0.4);
}

.hero-location {
    font-size: 18px;
    font-weight: 600;
    color: white;
}

.section-title {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.1em;
    color: rgba(255,255,255,0.25);
    text-transform: uppercase;
    margin-bottom: 8px;
}

/* Kill switch specific */
.killswitch-active {
    color: #ff6b6b;
}

.port-badge {
    background-color: rgba(0, 195, 137, 0.12);
    color: #00c389;
    border-radius: 6px;
    padding: 2px 8px;
    font-size: 12px;
    font-family: monospace;
    font-weight: 600;
}
"#;

pub fn build_ui(app: &adw::Application, state: Arc<RwLock<AppState>>) {
    // Load CSS
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

    // Main layout: sidebar + content
    let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);

    // ── SIDEBAR ──────────────────────────────────────
    let sidebar = build_sidebar();
    sidebar.set_size_request(200, -1);
    hbox.append(&sidebar);

    // ── CONTENT STACK ─────────────────────────────────
    let stack = gtk4::Stack::new();
    stack.set_hexpand(true);

    let main_page = build_main_page(state.clone());
    stack.add_named(&main_page, Some("main"));

    let settings_page = build_settings_page(state.clone());
    stack.add_named(&settings_page, Some("settings"));

    hbox.append(&stack);
    window.set_content(Some(&hbox));

    // Wire up sidebar nav
    // (In a full impl, buttons would switch stack.set_visible_child_name)

    // Periodic UI refresh
    let state_ref = state.clone();
    let main_page_ref = main_page.clone();
    glib::timeout_add_seconds_local(3, move || {
        let state = state_ref.clone();
        // Refresh stat labels — in practice you'd update specific widgets
        // by keeping references to them. Here we show the pattern.
        glib::Continue(true)
    });

    window.present();
}

fn build_sidebar() -> gtk4::Box {
    let sidebar = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    sidebar.add_css_class("pia-sidebar");

    // Logo area
    let logo_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    logo_box.set_margin_top(24);
    logo_box.set_margin_start(20);
    logo_box.set_margin_bottom(24);

    let logo_icon = gtk4::Label::new(Some("⬡"));
    logo_icon.set_css_classes(&["logo-icon"]);
    // In production: gtk4::Image::from_icon_name("network-vpn-symbolic")

    let logo_label = gtk4::Label::new(Some("PIA"));
    logo_label.set_css_classes(&["logo-label"]);
    logo_label.add_css_class("hero-location");
    logo_label.set_halign(gtk4::Align::Start);

    logo_box.append(&logo_icon);
    logo_box.append(&logo_label);
    sidebar.append(&logo_box);

    // Nav items
    let nav_items = [
        ("go-home-symbolic", "Dashboard"),
        ("network-server-symbolic", "Servers"),
        ("preferences-system-symbolic", "Settings"),
    ];

    for (icon, label) in &nav_items {
        let btn = gtk4::Button::new();
        btn.add_css_class("nav-btn");
        btn.set_margin_start(8);
        btn.set_margin_end(8);
        btn.set_margin_bottom(2);

        let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
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

    // Spacer
    let spacer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    sidebar.append(&spacer);

    // Account info at bottom
    let acct_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    acct_box.set_margin_all(16);

    let avatar = adw::Avatar::new(32, Some("Account"), false);
    let acct_label = gtk4::Label::new(Some("PIA Account"));
    acct_label.set_css_classes(&["stat-label"]);

    acct_box.append(&avatar);
    acct_box.append(&acct_label);
    sidebar.append(&acct_box);

    sidebar
}

fn build_main_page(state: Arc<RwLock<AppState>>) -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    page.add_css_class("pia-content");
    page.set_margin_all(32);

    // ── HERO ─────────────────────────────────────────
    let hero = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    hero.set_halign(gtk4::Align::Center);
    hero.set_margin_bottom(32);

    // Status pill
    let status_pill = gtk4::Label::new(Some("● DISCONNECTED"));
    status_pill.set_css_classes(&["status-pill", "status-disconnected"]);
    status_pill.set_halign(gtk4::Align::Center);
    hero.append(&status_pill);

    // Big connect button
    let connect_btn = gtk4::Button::new();
    connect_btn.add_css_class("connect-btn");
    connect_btn.add_css_class("disconnected");
    connect_btn.set_halign(gtk4::Align::Center);

    let btn_inner = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    btn_inner.set_halign(gtk4::Align::Center);
    btn_inner.set_valign(gtk4::Align::Center);

    let btn_icon = gtk4::Image::from_icon_name("network-vpn-disabled-symbolic");
    btn_icon.set_pixel_size(32);

    let btn_label = gtk4::Label::new(Some("CONNECT"));
    btn_label.set_css_classes(&["connect-btn-label"]);

    btn_inner.append(&btn_icon);
    btn_inner.append(&btn_label);
    connect_btn.set_child(Some(&btn_inner));

    // Connect button click handler
    let state_for_btn = state.clone();
    let status_pill_ref = status_pill.clone();
    let connect_btn_ref = connect_btn.clone();
    let btn_label_ref = btn_label.clone();
    connect_btn.connect_clicked(move |_| {
        let state = state_for_btn.clone();
        let pill = status_pill_ref.clone();
        let btn = connect_btn_ref.clone();
        let lbl = btn_label_ref.clone();

        glib::spawn_future_local(async move {
            let current_status = {
                let s = state.read().await;
                s.status.clone()
            };

            match current_status {
                ConnectionStatus::Connected => {
                    pill.set_label("● DISCONNECTING...");
                    lbl.set_label("DISCONNECT");
                    let _ = crate::dbus::disconnect_vpn().await;
                }
                _ => {
                    pill.set_label("● CONNECTING...");
                    pill.set_css_classes(&["status-pill", "status-connecting"]);
                    btn.set_css_classes(&["connect-btn", "connecting"]);
                    lbl.set_label("CANCEL");
                    let _ = crate::dbus::connect_vpn().await;
                }
            }
        });
    });

    hero.append(&connect_btn);

    // Location info
    let location_label = gtk4::Label::new(Some("Select a server"));
    location_label.set_css_classes(&["hero-location"]);
    location_label.set_halign(gtk4::Align::Center);

    let ip_label = gtk4::Label::new(Some("—"));
    ip_label.set_css_classes(&["hero-ip"]);
    ip_label.set_halign(gtk4::Align::Center);

    hero.append(&location_label);
    hero.append(&ip_label);
    page.append(&hero);

    // ── STAT CARDS ────────────────────────────────────
    let stats_grid = gtk4::Grid::new();
    stats_grid.set_column_spacing(8);
    stats_grid.set_row_spacing(8);
    stats_grid.set_margin_bottom(24);

    let stats = [
        ("DOWNLOAD", "0 B", 0, 0),
        ("UPLOAD", "0 B", 0, 1),
        ("LATENCY", "— ms", 1, 0),
        ("PORT FWD", "—", 1, 1),
    ];

    for (label, value, row, col) in &stats {
        let card = build_stat_card(label, value);
        stats_grid.attach(&card, *col, *row, 1, 1);
    }
    page.append(&stats_grid);

    // ── FEATURE TOGGLES ───────────────────────────────
    let features_title = gtk4::Label::new(Some("FEATURES"));
    features_title.set_css_classes(&["section-title"]);
    features_title.set_halign(gtk4::Align::Start);
    page.append(&features_title);

    let features_box = gtk4::Box::new(gtk4::Orientation::Vertical, 4);

    let kill_switch_row = build_toggle_row(
        "network-vpn-symbolic",
        "Kill Switch",
        "Block all traffic if VPN drops",
        false,
        {
            let state = state.clone();
            move |active| {
                let state = state.clone();
                glib::spawn_future_local(async move {
                    let iface = {
                        let s = state.read().await;
                        s.interface.clone()
                    };
                    if active {
                        let _ = crate::dbus::apply_kill_switch(&iface).await;
                    } else {
                        let _ = crate::dbus::remove_kill_switch().await;
                    }
                });
            }
        },
    );
    features_box.append(&kill_switch_row);

    let pf_state = state.clone();
    let port_forward_row = build_toggle_row(
        "network-transmit-receive-symbolic",
        "Port Forwarding",
        "Allow inbound connections",
        false,
        move |active| {
            let state = pf_state.clone();
            glib::spawn_future_local(async move {
                if active {
                    let _ = crate::dbus::enable_port_forward().await;
                } else {
                    let _ = crate::dbus::disable_port_forward().await;
                }
            });
        },
    );
    features_box.append(&port_forward_row);

    let auto_connect_row = build_toggle_row(
        "system-run-symbolic",
        "Auto Connect",
        "Connect on system startup",
        false,
        |_active| {
            // TODO: write to a config file / modify systemd unit WantedBy
        },
    );
    features_box.append(&auto_connect_row);

    page.append(&features_box);

    page
}

fn build_stat_card(label: &str, value: &str) -> gtk4::Box {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    card.add_css_class("stat-card");
    card.set_hexpand(true);

    let lbl = gtk4::Label::new(Some(label));
    lbl.set_css_classes(&["stat-label"]);
    lbl.set_halign(gtk4::Align::Start);

    let val = gtk4::Label::new(Some(value));
    val.set_css_classes(&["stat-value"]);
    val.set_halign(gtk4::Align::Start);

    card.append(&lbl);
    card.append(&val);
    card
}

fn build_toggle_row(
    icon: &str,
    title: &str,
    subtitle: &str,
    default: bool,
    on_toggle: impl Fn(bool) + 'static,
) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.add_css_class("feature-row");
    row.set_title(title);
    row.set_subtitle(subtitle);

    let icon_widget = gtk4::Image::from_icon_name(icon);
    icon_widget.set_pixel_size(18);
    row.add_prefix(&icon_widget);

    let toggle = gtk4::Switch::new();
    toggle.set_active(default);
    toggle.set_valign(gtk4::Align::Center);
    toggle.connect_active_notify(move |sw| {
        on_toggle(sw.is_active());
    });
    row.add_suffix(&toggle);
    row.set_activatable_widget(Some(&toggle));

    row
}

fn build_settings_page(state: Arc<RwLock<AppState>>) -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    page.add_css_class("pia-content");
    page.set_margin_all(32);

    let title = gtk4::Label::new(Some("Settings"));
    title.set_css_classes(&["hero-location"]);
    title.set_halign(gtk4::Align::Start);
    title.set_margin_bottom(8);
    page.append(&title);

    // Protocol group
    let proto_group = adw::PreferencesGroup::new();
    proto_group.set_title("Protocol");

    let proto_row = adw::ComboRow::new();
    proto_row.set_title("Protocol");
    let protocols = gtk4::StringList::new(&["WireGuard", "OpenVPN UDP", "OpenVPN TCP"]);
    proto_row.set_model(Some(&protocols));
    proto_group.add(&proto_row);

    let iface_row = adw::EntryRow::new();
    iface_row.set_title("Interface name");
    iface_row.set_text("wg0");
    proto_group.add(&iface_row);

    page.append(&proto_group);

    // DNS group
    let dns_group = adw::PreferencesGroup::new();
    dns_group.set_title("DNS");

    let dns_row = adw::ComboRow::new();
    dns_row.set_title("DNS Provider");
    let dns_options = gtk4::StringList::new(&["PIA DNS", "Google (8.8.8.8)", "Cloudflare (1.1.1.1)", "Custom"]);
    dns_row.set_model(Some(&dns_options));
    dns_group.add(&dns_row);

    page.append(&dns_group);

    // Latency group
    let latency_group = adw::PreferencesGroup::new();
    latency_group.set_title("Server selection");

    let latency_row = adw::ActionRow::new();
    latency_row.set_title("Max latency");
    latency_row.set_subtitle("Only consider servers below this latency threshold");

    let latency_spin = gtk4::SpinButton::with_range(50.0, 500.0, 10.0);
    latency_spin.set_value(100.0);
    latency_spin.set_valign(gtk4::Align::Center);
    latency_row.add_suffix(&latency_spin);
    latency_group.add(&latency_row);

    let pf_filter_row = adw::SwitchRow::new();
    pf_filter_row.set_title("Only port-forward servers");
    pf_filter_row.set_subtitle("Filter server list to those supporting port forwarding");
    latency_group.add(&pf_filter_row);

    page.append(&latency_group);

    page
}
