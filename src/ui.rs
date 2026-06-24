use crate::state::{format_bytes, AppState, ConnectionStatus};
use crate::tray::TrayMessage;
use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;
use std::sync::Arc;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------

const APP_CSS: &str = r#"
window.vex-window { background-color: #0d1117; }

.vex-sidebar {
    background-color: #0a0f16;
    border-right: 1px solid rgba(255,255,255,0.10);
}

.section-title {
    font-size: 10px;
    font-weight: 600;
    letter-spacing: .10em;
    color: #a0a0a0;
    margin-bottom: 6px;
}
.stat-label {
    font-size: 10px;
    color: #a0a0a0;
    letter-spacing: .09em;
}
.stat-value {
    font-size: 14px;
    font-weight: 500;
    color: #fafafa;
    font-family: monospace;
}

.hero-profile { font-size: 17px; font-weight: 600; color: #fafafa; }
.hero-ip      { font-size: 12px; color: #a0a0a0; font-family: monospace; }

.nav-btn {
    border-radius: 8px;
    min-height: 42px;
    color: #c8c8c8;
    font-size: 13px;
}
.nav-btn:hover  { background: rgba(255,255,255,.08); color: #ffffff; }
.nav-btn.active { background: rgba(0,195,137,.15);  color: #00c389; }

.stat-card {
    background: #111c2a;
    border: 1px solid rgba(255,255,255,.10);
    border-radius: 9px;
    padding: 11px 13px;
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
    border: 2px solid rgba(0,195,137,0.45);
    color: #00c389;
}
.connect-btn.state-disconnected:hover {
    border-color: rgba(0,195,137,0.85);
    box-shadow: 0 0 32px rgba(0,195,137,0.20);
}
.connect-btn.state-connected {
    background: #00291b;
    border: 2px solid #00c389;
    color: #00c389;
    box-shadow: 0 0 40px rgba(0,195,137,0.25);
}
.connect-btn.state-connecting {
    background: #1a1306;
    border: 2px solid rgba(255,180,0,0.7);
    color: #ffb400;
}

.status-pill {
    border-radius: 9999px;
    padding: 4px 14px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: .09em;
}
.status-pill.state-connected    { background: rgba(0,195,137,.18);  color: #00c389; }
.status-pill.state-disconnected { background: rgba(255,255,255,.10); color: #d8d8d8; }
.status-pill.state-connecting   { background: rgba(255,180,0,.18);  color: #ffb400; }
.status-pill.state-error        { background: rgba(255,80,80,.18);  color: #ff7878; }
"#;

// ---------------------------------------------------------------------------
// Live widget handles
// ---------------------------------------------------------------------------

struct LiveWidgets {
    status_pill: gtk4::Label,
    connect_btn: gtk4::Button,
    btn_icon: gtk4::Image,
    btn_label: gtk4::Label,
    profile_label: gtk4::Label,
    ip_label: gtk4::Label,
    dl_value: gtk4::Label,
    ul_value: gtk4::Label,
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn build_ui(
    app: &adw::Application,
    state: Arc<RwLock<AppState>>,
    rx: Option<async_channel::Receiver<TrayMessage>>,
) -> adw::ApplicationWindow {
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(APP_CSS);
    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("no display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("vex-vpn")
        .default_width(760)
        .default_height(540)
        .resizable(false)
        .build();
    window.add_css_class("vex-window");

    let root = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    let (sidebar_box, history_btn, profiles_btn) = build_sidebar();
    root.append(&sidebar_box);

    let toast_overlay = adw::ToastOverlay::new();

    let (main_page, live) = build_main_page(state.clone(), window.clone(), toast_overlay.clone());

    let nav_view = adw::NavigationView::new();
    let dashboard_page = adw::NavigationPage::builder()
        .title("Dashboard")
        .child(&main_page)
        .build();
    nav_view.push(&dashboard_page);

    // Profiles button → push profiles page.
    {
        let nav_c = nav_view.clone();
        let state_c = state.clone();
        profiles_btn.connect_clicked(move |_| {
            let page = crate::ui_profiles::build_profiles_page(state_c.clone(), nav_c.clone());
            nav_c.push(&page);
        });
    }

    // History button → push history page.
    {
        let nav_c = nav_view.clone();
        history_btn.connect_clicked(move |_| {
            nav_c.push(&build_history_page());
        });
    }

    nav_view.set_hexpand(true);
    root.append(&nav_view);

    let header = adw::HeaderBar::new();
    header.set_show_title(false);
    header.set_show_end_title_buttons(true);
    header.set_show_start_title_buttons(true);

    let menu_button = gtk4::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main menu")
        .menu_model(&build_primary_menu())
        .build();
    header.pack_end(&menu_button);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toast_overlay.set_child(Some(&root));
    toolbar_view.set_content(Some(&toast_overlay));

    window.set_content(Some(&toolbar_view));

    // Drain tray→window messages.
    if let Some(rx) = rx {
        let window_ref = window.clone();
        let app_ref = app.clone();
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                match msg {
                    TrayMessage::ShowWindow => window_ref.present(),
                    TrayMessage::Quit => app_ref.quit(),
                }
            }
        });
    }

    // Detect whether a system kill switch service is already active and notify.
    {
        let state_c = state.clone();
        let toast_c = toast_overlay.clone();
        glib::spawn_future_local(async move {
            let service = state_c.read().await.kill_switch_service_name.clone();
            let unit = format!("{}.service", service);
            if let Ok(status) = crate::dbus::get_service_status(&unit).await {
                if status == "active" {
                    let toast = adw::Toast::new(
                        "System kill switch is already active — vex-vpn is deferring to it.",
                    );
                    toast.set_timeout(8);
                    toast_c.add_toast(toast);
                }
            }
        });
    }

    // Refresh UI every 3 seconds.
    glib::timeout_add_seconds_local(3, move || {
        let state = state.clone();
        let live = LiveWidgets {
            status_pill: live.status_pill.clone(),
            connect_btn: live.connect_btn.clone(),
            btn_icon: live.btn_icon.clone(),
            btn_label: live.btn_label.clone(),
            profile_label: live.profile_label.clone(),
            ip_label: live.ip_label.clone(),
            dl_value: live.dl_value.clone(),
            ul_value: live.ul_value.clone(),
        };
        glib::spawn_future_local(async move {
            let s = state.read().await.clone();
            refresh_widgets(&live, &s);
        });
        glib::ControlFlow::Continue
    });

    window.present();
    window
}

// ---------------------------------------------------------------------------
// Primary menu
// ---------------------------------------------------------------------------

pub fn build_primary_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let view_section = gio::Menu::new();
    view_section.append(Some("Preferences"), Some("app.preferences"));
    view_section.append(Some("Keyboard Shortcuts"), Some("app.show-shortcuts"));
    menu.append_section(None, &view_section);

    let app_section = gio::Menu::new();
    app_section.append(Some("About vex-vpn"), Some("app.about"));
    app_section.append(Some("Quit"), Some("app.quit"));
    menu.append_section(None, &app_section);

    menu
}

pub fn show_shortcuts_window(parent: &adw::ApplicationWindow) {
    let builder = gtk4::Builder::from_string(include_str!("../assets/shortcuts.ui"));
    match builder.object::<gtk4::ShortcutsWindow>("help_overlay") {
        Some(win) => {
            win.set_transient_for(Some(parent));
            win.present();
        }
        None => {
            tracing::error!("shortcuts window object 'help_overlay' not found in XML");
        }
    }
}

pub fn show_about_window(parent: &adw::ApplicationWindow) {
    let about = adw::AboutWindow::builder()
        .transient_for(parent)
        .modal(true)
        .application_name("vex-vpn")
        .application_icon("network-vpn-symbolic")
        .developer_name("vex-vpn contributors")
        .version(env!("CARGO_PKG_VERSION"))
        .website("https://github.com/victorytek/vex-vpn")
        .license_type(gtk4::License::MitX11)
        .build();
    about.present();
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

fn build_sidebar() -> (gtk4::Box, gtk4::Button, gtk4::Button) {
    let sidebar = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    sidebar.add_css_class("vex-sidebar");
    sidebar.set_size_request(192, -1);

    let logo_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    logo_row.set_margin_top(22);
    logo_row.set_margin_start(18);
    logo_row.set_margin_bottom(20);

    let logo_img = gtk4::Image::from_icon_name("network-vpn-symbolic");
    logo_img.set_pixel_size(22);

    let logo_lbl = gtk4::Label::new(Some("vex-vpn"));
    logo_lbl.set_css_classes(&["section-title"]);
    logo_lbl.set_halign(gtk4::Align::Start);

    logo_row.append(&logo_img);
    logo_row.append(&logo_lbl);
    sidebar.append(&logo_row);

    // Dashboard nav button.
    let dash_btn = nav_button("go-home-symbolic", "Dashboard", true);
    sidebar.append(&dash_btn);

    // Profiles nav button.
    let profiles_btn = nav_button("network-server-symbolic", "Profiles", false);
    sidebar.append(&profiles_btn);

    let spacer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    sidebar.append(&spacer);

    // History button at bottom.
    let history_btn = nav_button("document-open-recent-symbolic", "History", false);
    history_btn.set_margin_bottom(8);
    sidebar.append(&history_btn);

    (sidebar, history_btn, profiles_btn)
}

fn nav_button(icon: &str, label: &str, active: bool) -> gtk4::Button {
    let btn = gtk4::Button::new();
    btn.add_css_class("nav-btn");
    if active {
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
    btn
}

// ---------------------------------------------------------------------------
// History page
// ---------------------------------------------------------------------------

fn build_history_page() -> adw::NavigationPage {
    let list_box = gtk4::ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    let placeholder = adw::ActionRow::new();
    placeholder.set_title("Loading\u{2026}");
    list_box.append(&placeholder);

    let scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .vexpand(true)
        .child(&list_box)
        .build();

    let clamp = adw::Clamp::new();
    clamp.set_child(Some(&scroll));
    clamp.set_maximum_size(600);
    clamp.set_margin_top(12);
    clamp.set_margin_bottom(12);
    clamp.set_margin_start(12);
    clamp.set_margin_end(12);

    let page = adw::NavigationPage::builder()
        .title("Connection History")
        .child(&clamp)
        .build();

    glib::spawn_future_local(async move {
        let entries = tokio::task::spawn_blocking(|| crate::history::load_recent(50))
            .await
            .unwrap_or_default();

        while let Some(child) = list_box.first_child() {
            list_box.remove(&child);
        }

        if entries.is_empty() {
            let row = adw::ActionRow::new();
            row.set_title("No connections recorded yet");
            list_box.append(&row);
        } else {
            for e in &entries {
                let duration = crate::history::format_duration(e.ts_end.saturating_sub(e.ts_start));
                let when = crate::history::format_timestamp(e.ts_start);
                let row = adw::ActionRow::new();
                row.set_title(&e.profile_name);
                row.set_subtitle(&format!("{} \u{2014} {}", when, duration));
                list_box.append(&row);
            }
        }
    });

    page
}

// ---------------------------------------------------------------------------
// Main dashboard page
// ---------------------------------------------------------------------------

fn build_main_page(
    state: Arc<RwLock<AppState>>,
    _window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
) -> (gtk4::Box, LiveWidgets) {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    page.set_margin_top(28);
    page.set_margin_bottom(28);
    page.set_margin_start(28);
    page.set_margin_end(28);
    page.set_hexpand(true);

    // ── Hero ──────────────────────────────────────────────────────────────

    let hero = gtk4::Box::new(gtk4::Orientation::Vertical, 14);
    hero.set_halign(gtk4::Align::Center);
    hero.set_margin_bottom(28);

    let status_pill = gtk4::Label::new(Some("● DISCONNECTED"));
    status_pill.set_css_classes(&["status-pill", "state-disconnected"]);
    status_pill.set_halign(gtk4::Align::Center);
    hero.append(&status_pill);

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

    // Button click — toggle connect/disconnect.
    {
        let state_c = state.clone();
        let pill_c = status_pill.clone();
        let btn_c = connect_btn.clone();
        let lbl_c = btn_label.clone();
        let icon_c = btn_icon.clone();
        let toast_c = toasts.clone();

        connect_btn.connect_clicked(move |_| {
            let state = state_c.clone();
            let pill = pill_c.clone();
            let btn = btn_c.clone();
            let lbl = lbl_c.clone();
            let icon = icon_c.clone();
            let toast = toast_c.clone();

            glib::spawn_future_local(async move {
                let s = state.read().await;
                let current = s.status.clone();
                let iface = s
                    .active_profile()
                    .map(|p| p.effective_interface().to_string())
                    .unwrap_or_else(|| "wg0".to_string());
                drop(s);

                match current {
                    ConnectionStatus::Connected | ConnectionStatus::KillSwitchActive => {
                        pill.set_label("● DISCONNECTING...");
                        set_state_class(&pill, "state-connecting");
                        set_state_class(&btn, "state-connecting");
                        if let Err(e) = crate::dbus::stop_wireguard_unit(&iface).await {
                            tracing::error!("disconnect: {}", e);
                            toast.add_toast(adw::Toast::new(&format!("Disconnect failed: {e:#}")));
                        }
                    }
                    ConnectionStatus::Connecting => {
                        let _ = crate::dbus::stop_wireguard_unit(&iface).await;
                    }
                    _ => {
                        pill.set_label("● CONNECTING...");
                        set_state_class(&pill, "state-connecting");
                        set_state_class(&btn, "state-connecting");
                        lbl.set_label("CANCEL");
                        icon.set_icon_name(Some("network-vpn-acquiring-symbolic"));

                        if let Err(e) = crate::dbus::start_wireguard_unit(&iface).await {
                            tracing::error!("connect: {}", e);
                            set_state_class(&pill, "state-disconnected");
                            set_state_class(&btn, "state-disconnected");
                            lbl.set_label("CONNECT");
                            icon.set_icon_name(Some("network-vpn-disabled-symbolic"));
                            toast.add_toast(adw::Toast::new(&format!("Connect failed: {e:#}")));
                        }
                    }
                }
            });
        });
    }

    hero.append(&connect_btn);

    let profile_label = gtk4::Label::new(Some("No profile selected"));
    profile_label.set_css_classes(&["hero-profile"]);
    profile_label.set_halign(gtk4::Align::Center);

    let ip_label = gtk4::Label::new(Some("—"));
    ip_label.set_css_classes(&["hero-ip"]);
    ip_label.set_halign(gtk4::Align::Center);

    hero.append(&profile_label);
    hero.append(&ip_label);
    page.append(&hero);

    // ── Stat cards ────────────────────────────────────────────────────────

    let stats_grid = gtk4::Grid::new();
    stats_grid.set_column_spacing(8);
    stats_grid.set_row_spacing(8);
    stats_grid.set_column_homogeneous(true);
    stats_grid.set_margin_bottom(22);

    let (dl_card, dl_value) = make_stat_card("DOWNLOAD", "0 B");
    let (ul_card, ul_value) = make_stat_card("UPLOAD", "0 B");

    stats_grid.attach(&dl_card, 0, 0, 1, 1);
    stats_grid.attach(&ul_card, 1, 0, 1, 1);
    page.append(&stats_grid);

    let live = LiveWidgets {
        status_pill,
        connect_btn,
        btn_icon,
        btn_label,
        profile_label,
        ip_label,
        dl_value,
        ul_value,
    };

    (page, live)
}

fn make_stat_card(label: &str, init_val: &str) -> (gtk4::Box, gtk4::Label) {
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    card.add_css_class("stat-card");

    let lbl = gtk4::Label::new(Some(label));
    lbl.set_css_classes(&["stat-label"]);
    lbl.set_halign(gtk4::Align::Start);

    let val = gtk4::Label::new(Some(init_val));
    val.set_css_classes(&["stat-value"]);
    val.set_halign(gtk4::Align::Start);

    card.append(&lbl);
    card.append(&val);
    (card, val)
}

// ---------------------------------------------------------------------------
// Widget refresh
// ---------------------------------------------------------------------------

fn refresh_widgets(live: &LiveWidgets, s: &AppState) {
    match &s.status {
        ConnectionStatus::Connected => {
            live.status_pill.set_label("● CONNECTED");
            set_state_class(&live.status_pill, "state-connected");
            set_state_class(&live.connect_btn, "state-connected");
            live.btn_label.set_label("DISCONNECT");
            live.btn_icon.set_icon_name(Some("network-vpn-symbolic"));
        }
        ConnectionStatus::KillSwitchActive => {
            live.status_pill.set_label("● KILL SWITCH");
            set_state_class(&live.status_pill, "state-connected");
            set_state_class(&live.connect_btn, "state-connected");
            live.btn_label.set_label("DISCONNECT");
            live.btn_icon
                .set_icon_name(Some("network-vpn-no-route-symbolic"));
        }
        ConnectionStatus::Connecting => {
            live.status_pill.set_label("● CONNECTING...");
            set_state_class(&live.status_pill, "state-connecting");
            set_state_class(&live.connect_btn, "state-connecting");
            live.btn_label.set_label("CANCEL");
            live.btn_icon
                .set_icon_name(Some("network-vpn-acquiring-symbolic"));
        }
        ConnectionStatus::Stale(_) => {
            live.status_pill.set_label("● RECONNECTING...");
            set_state_class(&live.status_pill, "state-connecting");
            set_state_class(&live.connect_btn, "state-connecting");
            live.btn_label.set_label("CANCEL");
            live.btn_icon
                .set_icon_name(Some("network-vpn-acquiring-symbolic"));
        }
        ConnectionStatus::Error(msg) => {
            live.status_pill.set_label(&format!("● ERROR: {}", msg));
            set_state_class(&live.status_pill, "state-error");
            set_state_class(&live.connect_btn, "state-disconnected");
            live.btn_label.set_label("CONNECT");
            live.btn_icon
                .set_icon_name(Some("network-vpn-disabled-symbolic"));
        }
        ConnectionStatus::Disconnected => {
            live.status_pill.set_label("● DISCONNECTED");
            set_state_class(&live.status_pill, "state-disconnected");
            set_state_class(&live.connect_btn, "state-disconnected");
            live.btn_label.set_label("CONNECT");
            live.btn_icon
                .set_icon_name(Some("network-vpn-disabled-symbolic"));
        }
    }

    // Profile name.
    if let Some(profile) = s.active_profile() {
        live.profile_label.set_label(&profile.name);
    } else {
        live.profile_label.set_label("No profile selected");
    }

    // IP / stats.
    if let Some(conn) = &s.connection {
        if !conn.local_ip.is_empty() {
            live.ip_label.set_label(&conn.local_ip);
        } else {
            live.ip_label.set_label("—");
        }
        live.dl_value.set_label(&format_bytes(conn.rx_bytes));
        live.ul_value.set_label(&format_bytes(conn.tx_bytes));
    } else {
        live.ip_label.set_label("—");
        live.dl_value.set_label("0 B");
        live.ul_value.set_label("0 B");
    }
}

fn set_state_class(widget: &impl gtk4::prelude::WidgetExt, new_class: &str) {
    for cls in &[
        "state-connected",
        "state-disconnected",
        "state-connecting",
        "state-error",
    ] {
        widget.remove_css_class(cls);
    }
    widget.add_css_class(new_class);
}
