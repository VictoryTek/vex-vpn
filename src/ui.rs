use crate::pia;
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
window.pia-window { background-color: #0d1117; }

.pia-sidebar {
    background-color: #0a0f16;
    border-right: 1px solid rgba(255,255,255,0.10);
}

/* Section / stat labels — solid colors meeting WCAG AA on #0d1117. */
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
.stat-value.green { color: #00c389; }

.hero-location { font-size: 17px; font-weight: 600; color: #fafafa; }
.hero-ip       { font-size: 12px; color: #a0a0a0; font-family: monospace; }

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

/* AdwActionRow inside .boxed-list .feature-list — bumped card bg so dim
   subtitles still pass AA against our forced near-black window. */
.feature-list > row { background-color: #15202b; }
.feature-list > row .subtitle { color: #b8b8b8; opacity: 1.0; }
.feature-list > row .title    { color: #fafafa; }

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

.port-badge {
    background: rgba(0,195,137,.18);
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
    kill_switch_sw: gtk4::Switch,
    port_forward_sw: gtk4::Switch,
    kill_switch_updating: std::rc::Rc<std::cell::Cell<bool>>,
    port_forward_updating: std::rc::Rc<std::cell::Cell<bool>>,
    server_row: adw::ActionRow,
    dns_banner: adw::Banner,
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
        .title("Private Internet Access")
        .default_width(760)
        .default_height(540)
        .resizable(false)
        .build();
    window.add_css_class("pia-window");

    let root = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    let (sidebar_box, history_btn) = build_sidebar();
    root.append(&sidebar_box);

    let initial_auto_connect = {
        // Read synchronously — at startup before the async runtime is loaded.
        crate::config::Config::load()
            .unwrap_or_default()
            .auto_connect
    };

    let initial_kill_switch = crate::config::Config::load()
        .unwrap_or_default()
        .kill_switch_enabled;

    // Toast overlay wraps all content so any async error can surface a brief
    // non-blocking notification to the user.
    let toast_overlay = adw::ToastOverlay::new();

    let (main_page, live) = build_main_page(
        state.clone(),
        initial_auto_connect,
        initial_kill_switch,
        toast_overlay.clone(),
    );

    // Wrap the dashboard and server list in a NavigationView.
    let nav_view = adw::NavigationView::new();
    let dashboard_page = adw::NavigationPage::builder()
        .title("Dashboard")
        .child(&main_page)
        .build();
    nav_view.push(&dashboard_page);

    // Make the server row activatable — clicking it pushes the server list page.
    {
        let nav_view_c = nav_view.clone();
        let state_c = state.clone();
        let server_row_c = live.server_row.clone();
        live.server_row.set_activatable(true);
        live.server_row.connect_activated(move |_| {
            let server_page = build_server_list_page(state_c.clone(), &nav_view_c, &server_row_c);
            nav_view_c.push(&server_page);
        });
    }

    // Wire History button from sidebar to push a history navigation page.
    {
        let nav_view_h = nav_view.clone();
        history_btn.connect_clicked(move |_| {
            nav_view_h.push(&build_history_page());
        });
    }

    nav_view.set_hexpand(true);
    root.append(&nav_view);

    // Wrap content in an AdwToolbarView with an AdwHeaderBar so the window
    // has a draggable area and somewhere to host the primary menu.
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

    // Drain the tray→window channel and raise the window on ShowWindow.
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
            kill_switch_sw: live.kill_switch_sw.clone(),
            port_forward_sw: live.port_forward_sw.clone(),
            kill_switch_updating: live.kill_switch_updating.clone(),
            port_forward_updating: live.port_forward_updating.clone(),
            server_row: live.server_row.clone(),
            dns_banner: live.dns_banner.clone(),
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
// Primary menu (gio::Menu) and About window
// ---------------------------------------------------------------------------

/// Build the primary application menu shown by the headerbar MenuButton.
/// Targets `app.*` action names registered in `main.rs`.
pub fn build_primary_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let account_section = gio::Menu::new();
    account_section.append(Some("Switch account…"), Some("app.switch-account"));
    menu.append_section(None, &account_section);

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

/// Show the `GtkShortcutsWindow` loaded from assets/shortcuts.ui.
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

/// Show an `AdwAboutWindow` transient for `parent`.
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

fn build_sidebar() -> (gtk4::Box, gtk4::Button) {
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
    let nav_items = [("go-home-symbolic", "Dashboard", true)];

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

    // History button — pinned at the bottom of the sidebar.
    let history_btn = gtk4::Button::new();
    history_btn.add_css_class("nav-btn");
    history_btn.set_margin_start(8);
    history_btn.set_margin_end(8);
    history_btn.set_margin_bottom(8);

    let hist_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    hist_row.set_margin_start(8);

    let hist_img = gtk4::Image::from_icon_name("document-open-recent-symbolic");
    hist_img.set_pixel_size(16);

    let hist_lbl = gtk4::Label::new(Some("History"));
    hist_lbl.set_halign(gtk4::Align::Start);
    hist_lbl.set_hexpand(true);

    hist_row.append(&hist_img);
    hist_row.append(&hist_lbl);
    history_btn.set_child(Some(&hist_row));
    sidebar.append(&history_btn);

    (sidebar, history_btn)
}

// ---------------------------------------------------------------------------
// History navigation page
// ---------------------------------------------------------------------------

fn build_history_page() -> adw::NavigationPage {
    let list_box = gtk4::ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    // Placeholder row shown while history is loaded off the main thread.
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

    // Load history off the GTK main thread to avoid blocking on slow disks.
    glib::spawn_future_local(async move {
        let entries = tokio::task::spawn_blocking(|| crate::history::load_recent(50))
            .await
            .unwrap_or_default();

        // Clear the loading placeholder.
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
                row.set_title(&e.region);
                row.set_subtitle(&format!("{} \u{2014} {}", when, duration));
                list_box.append(&row);
            }
        }
    });

    page
}

// ---------------------------------------------------------------------------
// Main page
// ---------------------------------------------------------------------------

fn build_main_page(
    state: Arc<RwLock<AppState>>,
    initial_auto_connect: bool,
    initial_kill_switch: bool,
    toasts: adw::ToastOverlay,
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
        let toast_c = toasts.clone();

        connect_btn.connect_clicked(move |_| {
            let state = state_c.clone();
            let pill = pill_c.clone();
            let btn = btn_c.clone();
            let lbl = lbl_c.clone();
            let icon = icon_c.clone();
            let toast = toast_c.clone();

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
                            pill.set_label("● ERROR");
                            set_state_class(&pill, "state-error");
                            toast.add_toast(adw::Toast::new(&format!("Disconnect failed: {e:#}")));
                        }
                    }
                    ConnectionStatus::Connecting => {
                        if let Err(e) = crate::dbus::disconnect_vpn().await {
                            tracing::error!("cancel: {}", e);
                            pill.set_label("● ERROR");
                            set_state_class(&pill, "state-error");
                            toast.add_toast(adw::Toast::new(&format!("Cancel failed: {e:#}")));
                        }
                    }
                    _ => {
                        pill.set_label("● CONNECTING...");
                        set_state_class(&pill, "state-connecting");
                        set_state_class(&btn, "state-connecting");
                        lbl.set_label("CANCEL");
                        icon.set_icon_name(Some("network-vpn-acquiring-symbolic"));

                        if let Err(e) = crate::dbus::connect_vpn().await {
                            tracing::error!("connect: {}", e);
                            pill.set_label("● ERROR");
                            set_state_class(&pill, "state-error");
                            toast.add_toast(adw::Toast::new(&format!("Connect failed: {e:#}")));
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

    // DNS leak warning banner — hidden until a potential leak is detected.
    let dns_banner = adw::Banner::new("Possible DNS leak detected");
    dns_banner.set_revealed(false);
    page.append(&dns_banner);

    // ── Server picker placeholder ─────────────────────────────────────────
    // Full server list UI is deferred. This row is wired to AppState so once
    // a region is known (today: written by the backend after auto-select) the
    // subtitle is updated by refresh_widgets.
    let server_group = gtk4::ListBox::new();
    server_group.set_selection_mode(gtk4::SelectionMode::None);
    server_group.add_css_class("boxed-list");
    server_group.set_margin_bottom(22);

    let server_row = adw::ActionRow::new();
    server_row.set_title("Server");
    server_row.set_subtitle("Sign in to load servers");
    let server_icon = gtk4::Image::from_icon_name("network-server-symbolic");
    server_icon.set_pixel_size(16);
    server_row.add_prefix(&server_icon);
    let chevron = gtk4::Image::from_icon_name("go-next-symbolic");
    chevron.add_css_class("dim-label");
    server_row.add_suffix(&chevron);
    server_row.set_activatable(false);
    server_group.append(&server_row);
    page.append(&server_group);

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

    let feats = gtk4::ListBox::new();
    feats.set_selection_mode(gtk4::SelectionMode::None);
    feats.add_css_class("boxed-list");
    feats.add_css_class("feature-list");

    // Kill switch
    let kill_switch_updating = std::rc::Rc::new(std::cell::Cell::new(false));
    let kill_switch_sw = {
        let state_c = state.clone();
        let guard = kill_switch_updating.clone();
        let toasts_ks = toasts.clone();
        let (row, sw) = make_toggle_row(
            "network-vpn-symbolic",
            "Kill Switch",
            "Block all traffic if VPN drops",
            initial_kill_switch,
            move |active| {
                if guard.get() {
                    return;
                }
                let state = state_c.clone();
                let toasts = toasts_ks.clone();
                glib::spawn_future_local(async move {
                    let iface = state.read().await.interface.clone();
                    let res = if active {
                        crate::helper::apply_kill_switch(&iface).await
                    } else {
                        crate::helper::remove_kill_switch().await
                    };
                    match res {
                        Ok(()) => {
                            state.write().await.kill_switch_enabled = active;
                        }
                        Err(e) => {
                            tracing::error!("kill switch toggle: {}", e);
                            toasts.add_toast(adw::Toast::new(&format!("Kill switch error: {e:#}")));
                        }
                    }
                });
            },
        );
        feats.append(&row);
        sw
    };

    // Port forwarding
    let port_forward_updating = std::rc::Rc::new(std::cell::Cell::new(false));
    let port_forward_sw = {
        let guard = port_forward_updating.clone();
        let toasts_pf = toasts.clone();
        let (row, sw) = make_toggle_row(
            "network-transmit-receive-symbolic",
            "Port Forwarding",
            "Allow inbound connections through VPN",
            false,
            move |active| {
                if guard.get() {
                    return;
                }
                let toasts = toasts_pf.clone();
                glib::spawn_future_local(async move {
                    let res = if active {
                        crate::dbus::enable_port_forward().await
                    } else {
                        crate::dbus::disable_port_forward().await
                    };
                    if let Err(e) = res {
                        tracing::error!("port forward toggle: {}", e);
                        toasts.add_toast(adw::Toast::new(&format!("Port forwarding error: {e:#}")));
                    }
                });
            },
        );
        feats.append(&row);
        sw
    };

    // Auto connect — persisted via config.toml
    {
        let (row, _) = make_toggle_row(
            "system-run-symbolic",
            "Auto Connect",
            "Connect on graphical login",
            initial_auto_connect,
            move |active| {
                let mut cfg = crate::config::Config::load().unwrap_or_else(|e| {
                    tracing::warn!("Failed to load config: {e:#}");
                    crate::config::Config::default()
                });
                cfg.auto_connect = active;
                if let Err(e) = cfg.save() {
                    tracing::error!("Failed to save config: {}", e);
                }
            },
        );
        feats.append(&row);
    }

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
        kill_switch_sw,
        port_forward_sw,
        kill_switch_updating,
        port_forward_updating,
        server_row,
        dns_banner,
    };

    (page, live)
}

// ---------------------------------------------------------------------------
// Server list page
// ---------------------------------------------------------------------------

fn build_server_list_page(
    state: Arc<RwLock<AppState>>,
    nav_view: &adw::NavigationView,
    dashboard_server_row: &adw::ActionRow,
) -> adw::NavigationPage {
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);

    // Search entry at the top
    let search_entry = gtk4::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Search servers…"));
    content.append(&search_entry);

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_min_content_height(300);

    let list_box = gtk4::ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");
    list_box.add_css_class("feature-list");
    scrolled.set_child(Some(&list_box));
    content.append(&scrolled);

    // Populate from current state — snapshot the regions.
    let nav_view_c = nav_view.clone();
    let dashboard_row_c = dashboard_server_row.clone();
    let state_c = state.clone();
    let list_box_c = list_box.clone();

    glib::spawn_future_local(async move {
        let regions = {
            let s = state_c.read().await;
            s.regions.clone()
        };

        for region in &regions {
            let row = build_server_row(region);
            list_box_c.append(&row);

            // Measure latency asynchronously
            if let Some(meta) = region.servers.meta.first() {
                let ip = meta.ip.clone();
                let row_ref = row.clone();
                glib::spawn_future_local(async move {
                    if let Some(lat) = pia::PiaClient::measure_latency(&ip).await {
                        row_ref.set_subtitle(&format!("{} ms", lat.as_millis()));
                    }
                });
            }

            // On click: select this region
            let region_id = region.id.clone();
            let region_name = region.name.clone();
            let state_c2 = state_c.clone();
            let nav_view_c2 = nav_view_c.clone();
            let dashboard_row_c2 = dashboard_row_c.clone();

            row.connect_activated(move |_| {
                let region_id = region_id.clone();
                let region_name = region_name.clone();
                let state = state_c2.clone();
                let nav_view = nav_view_c2.clone();
                let dashboard_row = dashboard_row_c2.clone();

                glib::spawn_future_local(async move {
                    // Update state
                    state.write().await.selected_region_id = Some(region_id.clone());

                    // Persist to config
                    let mut cfg = crate::config::Config::load().unwrap_or_else(|e| {
                        tracing::warn!("Failed to load config: {e:#}");
                        crate::config::Config::default()
                    });
                    cfg.selected_region_id = Some(region_id);
                    if let Err(e) = cfg.save() {
                        tracing::error!("Failed to save config: {}", e);
                    }

                    // Update dashboard
                    dashboard_row.set_subtitle(&region_name);

                    // Pop back to dashboard
                    nav_view.pop();
                });
            });
        }

        if regions.is_empty() {
            let empty_label = gtk4::Label::new(Some("Sign in to load servers"));
            empty_label.add_css_class("dim-label");
            empty_label.set_margin_top(24);
            list_box_c.append(&empty_label);
        }
    });

    // Search filtering
    let list_box_filter = list_box.clone();
    search_entry.connect_search_changed(move |entry| {
        let query = entry.text().to_string().to_lowercase();
        let mut child = list_box_filter.first_child();
        while let Some(widget) = child {
            if let Some(row) = widget.downcast_ref::<adw::ActionRow>() {
                let title = row.title().to_string().to_lowercase();
                row.set_visible(query.is_empty() || title.contains(&query));
            }
            child = widget.next_sibling();
        }
    });

    adw::NavigationPage::builder()
        .title("Servers")
        .child(&content)
        .build()
}

/// Build a single server row for the server list page.
fn build_server_row(region: &pia::Region) -> adw::ActionRow {
    let row = adw::ActionRow::new();
    row.set_title(&region.name);
    row.set_activatable(true);
    row.set_subtitle("—");

    let icon = gtk4::Image::from_icon_name("network-server-symbolic");
    icon.set_pixel_size(16);
    row.add_prefix(&icon);

    // Port-forward badge
    if region.port_forward {
        let badge = gtk4::Label::new(Some("PF"));
        badge.add_css_class("port-badge");
        row.add_suffix(&badge);
    }

    // Geo badge
    if region.geo {
        let geo = gtk4::Label::new(Some("geo"));
        geo.add_css_class("dim-label");
        row.add_suffix(&geo);
    }

    let chevron = gtk4::Image::from_icon_name("go-next-symbolic");
    chevron.add_css_class("dim-label");
    row.add_suffix(&chevron);

    row
}

// ---------------------------------------------------------------------------
// Widget helpers
// ---------------------------------------------------------------------------

/// Replace all state-* CSS classes on a widget, then add the new one.
fn set_state_class<W: IsA<gtk4::Widget>>(widget: &W, new_class: &str) {
    for cls in [
        "state-connected",
        "state-disconnected",
        "state-connecting",
        "state-error",
    ] {
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
        ConnectionStatus::Stale(_) => (
            "● RECONNECTING...",
            "state-connecting",
            "DISCONNECT",
            "network-vpn-acquiring-symbolic",
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
        live.server_row.set_subtitle(&region.name);
    } else if let Some(ref selected_id) = s.selected_region_id {
        // Show the selected region name from the server list
        if let Some(r) = s.regions.iter().find(|r| &r.id == selected_id) {
            live.location_label.set_label(&r.name);
            live.server_row.set_subtitle(&r.name);
        } else {
            live.location_label.set_label(selected_id);
            live.server_row.set_subtitle(selected_id);
        }
    } else if !s.regions.is_empty() {
        live.location_label.set_label("Select a server");
        live.server_row.set_subtitle("Tap to choose a server");
    } else {
        live.location_label.set_label(if s.status.is_connected() {
            "Connected"
        } else {
            "Select a server"
        });
        live.server_row.set_subtitle(if s.auth_token.is_some() {
            "Tap to choose a server"
        } else {
            "Sign in to load servers"
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

    // Kill switch and port forward toggle sync
    live.kill_switch_updating.set(true);
    live.kill_switch_sw.set_active(s.kill_switch_enabled);
    live.kill_switch_updating.set(false);

    live.port_forward_updating.set(true);
    live.port_forward_sw.set_active(s.port_forward_enabled);
    live.port_forward_updating.set(false);

    // DNS leak banner
    live.dns_banner.set_revealed(s.dns_leak_hint.is_some());
    if let Some(ref ips) = s.dns_leak_hint {
        live.dns_banner
            .set_title(&format!("Possible DNS leak: {}", ips.join(", ")));
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
) -> (adw::ActionRow, gtk4::Switch) {
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

    (row, sw)
}
