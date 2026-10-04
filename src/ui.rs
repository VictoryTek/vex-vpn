use crate::dbus::{self, UnitError};
use crate::state::{AppState, Ctx};
use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;
use std::cell::Cell;
use std::future::Future;
use std::rc::Rc;
use vex_vpn::vexos::{format_bytes, format_duration, protocol_label, KillSwitchMode, VpnState};

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
.connect-btn.state-error {
    background: #1f0d0d;
    border: 2px solid rgba(255,80,80,0.7);
    color: #ff7878;
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
// Shared UI handle for the pages and dialogs
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Ui {
    pub ctx: Ctx,
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    /// A pkexec action is waiting (only one admin prompt at a time).
    pub priv_busy: Rc<Cell<bool>>,
}

impl Ui {
    pub fn toast(&self, msg: &str) {
        self.toasts.add_toast(adw::Toast::new(msg));
    }

    /// Run a systemd unit action off the main thread; report the outcome as
    /// a toast and re-poll. Calls `done` on the main thread afterwards.
    pub fn run_unit<F>(&self, fut: F, ok_msg: Option<String>, done: impl FnOnce() + 'static)
    where
        F: Future<Output = Result<(), UnitError>> + Send + 'static,
    {
        let ui = self.clone();
        glib::spawn_future_local(async move {
            match ui.ctx.run(fut).await {
                Ok(()) => {
                    if let Some(msg) = ok_msg {
                        ui.toast(&msg);
                    }
                }
                Err(UnitError::Cancelled) => ui.toast("Authentication cancelled"),
                Err(e) => ui.toast(&e.to_string()),
            }
            ui.ctx.poke();
            done();
        });
    }
}

/// Region id → display name, using the loaded region list when available.
pub fn region_display(snap: &AppState, id: &str) -> String {
    if id == vex_vpn::vexos::AUTO_REGION {
        return "Automatic (fastest)".to_string();
    }
    snap.regions
        .as_ref()
        .and_then(|r| r.as_ref().ok())
        .and_then(|list| list.iter().find(|r| r.id == id))
        .map(|r| r.name.clone())
        .unwrap_or_else(|| id.to_string())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub fn build_ui(app: &adw::Application, ctx: Ctx) -> adw::ApplicationWindow {
    static CSS: std::sync::Once = std::sync::Once::new();
    CSS.call_once(|| {
        let provider = gtk4::CssProvider::new();
        provider.load_from_data(APP_CSS);
        gtk4::style_context_add_provider_for_display(
            &gtk4::gdk::Display::default().expect("no display"),
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    });

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("vex-vpn")
        .default_width(820)
        .default_height(620)
        .build();
    window.add_css_class("vex-window");

    let toast_overlay = adw::ToastOverlay::new();
    let ui = Ui {
        ctx: ctx.clone(),
        window: window.clone(),
        toasts: toast_overlay.clone(),
        priv_busy: Rc::new(Cell::new(false)),
    };

    // Poll faster while the window is on screen.
    {
        let c = ctx.clone();
        window.connect_map(move |_| c.set_window_visible(true));
        let c = ctx.clone();
        window.connect_unmap(move |_| c.set_window_visible(false));
    }

    let root = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    let (sidebar, nav) = build_sidebar();
    root.append(&sidebar);

    let dashboard = Dashboard::new(&ui);
    let regions = crate::ui_regions::RegionsPage::new(&ui);
    let settings = crate::ui_settings::SettingsPage::new(&ui);
    let missing = build_missing_page(&ctx);

    let stack = gtk4::Stack::new();
    stack.set_hexpand(true);
    stack.set_transition_type(gtk4::StackTransitionType::Crossfade);
    stack.add_named(&dashboard.root, Some("dashboard"));
    stack.add_named(&regions.root, Some("regions"));
    stack.add_named(&settings.root, Some("settings"));
    stack.add_named(&missing, Some("missing"));
    root.append(&stack);

    // `win.show-page('<name>')` — sidebar buttons and the tray use this.
    let show_page = gio::SimpleAction::new("show-page", Some(glib::VariantTy::STRING));
    {
        let stack = stack.clone();
        let nav = nav.clone();
        show_page.connect_activate(move |_, param| {
            let Some(name) = param.and_then(|p| p.str()) else {
                return;
            };
            if stack.visible_child_name().as_deref() == Some("missing") {
                return;
            }
            stack.set_visible_child_name(name);
            for (page, btn) in nav.iter() {
                if *page == name {
                    btn.add_css_class("active");
                } else {
                    btn.remove_css_class("active");
                }
            }
        });
    }
    window.add_action(&show_page);

    // `win.enable-kill-switch` — the tray uses this to get a confirmation.
    let enable_ks = gio::SimpleAction::new("enable-kill-switch", None);
    {
        let ui = ui.clone();
        enable_ks.connect_activate(move |_, _| crate::ui_settings::enable_kill_switch(&ui, || {}));
    }
    window.add_action(&enable_ks);
    for (page, btn) in nav.iter() {
        btn.set_action_name(Some("win.show-page"));
        btn.set_action_target_value(Some(&page.to_variant()));
    }

    let header = adw::HeaderBar::new();
    header.set_show_title(false);
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

    // Re-render after every status poll until the window is closed.
    let closed = Rc::new(Cell::new(false));
    {
        let closed = closed.clone();
        window.connect_close_request(move |_| {
            closed.set(true);
            glib::Propagation::Proceed
        });
    }
    let mut changed = ctx.changed.subscribe();
    ctx.poke();
    glib::spawn_future_local(async move {
        use tokio::sync::broadcast::error::RecvError;
        loop {
            let snap = ctx.snapshot().await;
            if closed.get() {
                break;
            }
            let present = snap.backend.is_some();
            sidebar_nav_sensitive(&nav, present);
            if !present {
                stack.set_visible_child_name("missing");
            } else if stack.visible_child_name().as_deref() == Some("missing") {
                stack.set_visible_child_name("dashboard");
            }
            dashboard.update(&snap);
            regions.update(&snap);
            settings.update(&snap);
            match changed.recv().await {
                Ok(()) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => break,
            }
        }
    });

    window
}

// ---------------------------------------------------------------------------
// Primary menu
// ---------------------------------------------------------------------------

pub fn build_primary_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let view_section = gio::Menu::new();
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
        .application_icon("vex-vpn")
        .developer_name("vex-vpn contributors")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("Desktop app for vexos-vpn — PIA VPN and kill switch")
        .website("https://github.com/victorytek/vex-vpn")
        .license_type(gtk4::License::MitX11)
        .build();
    about.present();
}

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

type Nav = Vec<(&'static str, gtk4::Button)>;

fn build_sidebar() -> (gtk4::Box, Rc<Nav>) {
    let sidebar = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    sidebar.add_css_class("vex-sidebar");
    sidebar.set_size_request(192, -1);

    let logo_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    logo_row.set_margin_top(22);
    logo_row.set_margin_start(18);
    logo_row.set_margin_bottom(20);

    let logo_img = gtk4::Image::from_resource("/com/vex/vpn/branding/vpn2.png");
    logo_img.set_pixel_size(28);

    logo_row.append(&logo_img);
    sidebar.append(&logo_row);

    let nav = vec![
        (
            "dashboard",
            nav_button("go-home-symbolic", "Dashboard", true),
        ),
        (
            "regions",
            nav_button("network-server-symbolic", "Regions", false),
        ),
        (
            "settings",
            nav_button("emblem-system-symbolic", "Settings", false),
        ),
    ];
    for (_, btn) in &nav {
        sidebar.append(btn);
    }

    (sidebar, Rc::new(nav))
}

fn sidebar_nav_sensitive(nav: &Nav, sensitive: bool) {
    for (_, btn) in nav {
        btn.set_sensitive(sensitive);
    }
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
// Backend missing
// ---------------------------------------------------------------------------

fn build_missing_page(ctx: &Ctx) -> adw::StatusPage {
    let check = gtk4::Button::with_label("Check again");
    check.add_css_class("pill");
    check.add_css_class("suggested-action");
    check.set_halign(gtk4::Align::Center);
    let c = ctx.clone();
    check.connect_clicked(move |_| c.poke());

    adw::StatusPage::builder()
        .icon_name("network-vpn-disabled-symbolic")
        .title("vexos-vpn backend not installed")
        .description(
            "vex-vpn is the desktop app for vexos-vpn, the VPN service that ships \
             with vexos. Enable it in your vexos NixOS config (modules/vpn.nix) \
             and rebuild.",
        )
        .child(&check)
        .build()
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum BannerAction {
    None,
    SignIn,
    Retry,
}

struct Dashboard {
    root: gtk4::Box,
    banner: adw::Banner,
    banner_action: Rc<Cell<BannerAction>>,
    sign_in_page: adw::StatusPage,
    sign_in_btn: gtk4::Button,
    hero: gtk4::Box,
    status_pill: gtk4::Label,
    connect_btn: gtk4::Button,
    btn_icon: gtk4::Image,
    btn_label: gtk4::Label,
    region_label: gtk4::Label,
    detail_label: gtk4::Label,
    ks_chip: gtk4::Label,
    reconnect_btn: gtk4::Button,
    stats: gtk4::Grid,
    dl_value: gtk4::Label,
    ul_value: gtk4::Label,
}

impl Dashboard {
    fn new(ui: &Ui) -> Self {
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        root.set_hexpand(true);

        // ── Error banner ─────────────────────────────────────────────────
        let banner = adw::Banner::new("");
        let banner_action = Rc::new(Cell::new(BannerAction::None));
        {
            let ui = ui.clone();
            let action = banner_action.clone();
            banner.connect_button_clicked(move |_| match action.get() {
                BannerAction::SignIn => crate::ui_login::sign_in(&ui, false),
                BannerAction::Retry => ui.run_unit(
                    dbus::restart_unit(dbus::VPN_UNIT),
                    Some("Retrying\u{2026}".to_string()),
                    || {},
                ),
                BannerAction::None => {}
            });
        }
        root.append(&banner);

        let page = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        page.set_margin_top(28);
        page.set_margin_bottom(28);
        page.set_margin_start(28);
        page.set_margin_end(28);
        page.set_vexpand(true);

        // ── Sign-in call to action ───────────────────────────────────────
        let sign_in_btn = gtk4::Button::with_label("Sign in to PIA");
        sign_in_btn.add_css_class("pill");
        sign_in_btn.add_css_class("suggested-action");
        sign_in_btn.set_halign(gtk4::Align::Center);
        {
            let ui = ui.clone();
            sign_in_btn.connect_clicked(move |_| crate::ui_login::sign_in(&ui, false));
        }
        let sign_in_page = adw::StatusPage::builder()
            .icon_name("dialog-password-symbolic")
            .title("Sign in to PIA")
            .description("Enter your Private Internet Access username and password to use the VPN.")
            .child(&sign_in_btn)
            .vexpand(true)
            .visible(false)
            .build();
        page.append(&sign_in_page);

        // ── Hero ─────────────────────────────────────────────────────────
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

        {
            let ui = ui.clone();
            connect_btn.connect_clicked(move |btn| {
                let ui = ui.clone();
                let btn = btn.clone();
                glib::spawn_future_local(async move {
                    let snap = ui.ctx.snapshot().await;
                    if !snap.unit_active && snap.can_sign_in() {
                        crate::ui_login::sign_in(&ui, false);
                        return;
                    }
                    btn.set_sensitive(false);
                    let b = btn.clone();
                    let done = move || b.set_sensitive(true);
                    if snap.unit_active {
                        ui.run_unit(dbus::stop_unit(dbus::VPN_UNIT), None, done);
                    } else {
                        ui.run_unit(dbus::start_unit(dbus::VPN_UNIT), None, done);
                    }
                });
            });
        }
        hero.append(&connect_btn);

        let region_label = gtk4::Label::new(None);
        region_label.set_css_classes(&["hero-profile"]);
        region_label.set_halign(gtk4::Align::Center);
        let detail_label = gtk4::Label::new(None);
        detail_label.set_css_classes(&["hero-ip"]);
        detail_label.set_halign(gtk4::Align::Center);
        detail_label.set_wrap(true);
        detail_label.set_justify(gtk4::Justification::Center);
        hero.append(&region_label);
        hero.append(&detail_label);

        let chips = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        chips.set_halign(gtk4::Align::Center);
        let ks_chip = gtk4::Label::new(None);
        ks_chip.add_css_class("status-pill");
        chips.append(&ks_chip);
        let reconnect_btn = gtk4::Button::with_label("Reconnect");
        reconnect_btn.add_css_class("pill");
        {
            let ui = ui.clone();
            reconnect_btn.connect_clicked(move |btn| {
                btn.set_sensitive(false);
                let b = btn.clone();
                ui.run_unit(
                    dbus::restart_unit(dbus::VPN_UNIT),
                    Some("Reconnecting\u{2026}".to_string()),
                    move || b.set_sensitive(true),
                );
            });
        }
        chips.append(&reconnect_btn);
        hero.append(&chips);
        page.append(&hero);

        // ── Live rates ───────────────────────────────────────────────────
        let stats = gtk4::Grid::new();
        stats.set_column_spacing(8);
        stats.set_row_spacing(8);
        stats.set_column_homogeneous(true);
        let (dl_card, dl_value) = make_stat_card("DOWNLOAD", "0 B/s");
        let (ul_card, ul_value) = make_stat_card("UPLOAD", "0 B/s");
        stats.attach(&dl_card, 0, 0, 1, 1);
        stats.attach(&ul_card, 1, 0, 1, 1);
        page.append(&stats);

        let scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vexpand(true)
            .child(&page)
            .build();
        root.append(&scroll);

        Self {
            root,
            banner,
            banner_action,
            sign_in_page,
            sign_in_btn,
            hero,
            status_pill,
            connect_btn,
            btn_icon,
            btn_label,
            region_label,
            detail_label,
            ks_chip,
            reconnect_btn,
            stats,
            dl_value,
            ul_value,
        }
    }

    fn update(&self, snap: &AppState) {
        let Some(status) = &snap.status else {
            self.sign_in_page.set_visible(false);
            self.hero.set_visible(true);
            self.stats.set_visible(false);
            match &snap.poll_error {
                Some(err) => self.show_banner(
                    &format!("Couldn't read the VPN status: {}", err),
                    BannerAction::None,
                ),
                None => self.banner.set_revealed(false),
            }
            self.status_pill.set_label("● LOADING");
            set_state_class(&self.status_pill, "state-disconnected");
            self.connect_btn.set_sensitive(false);
            self.region_label.set_label("");
            self.detail_label.set_label("");
            self.ks_chip.set_visible(false);
            self.reconnect_btn.set_visible(false);
            return;
        };
        self.connect_btn.set_sensitive(true);

        // ── Banner ───────────────────────────────────────────────────────
        if status.state == VpnState::Error && !status.last_error.is_empty() {
            let action = if status.needs_sign_in() && status.credentials_editable {
                BannerAction::SignIn
            } else {
                BannerAction::Retry
            };
            self.show_banner(&status.last_error, action);
        } else if let Some(err) = &snap.poll_error {
            self.show_banner(
                &format!("Couldn't read the VPN status: {}", err),
                BannerAction::None,
            );
        } else {
            self.banner.set_revealed(false);
        }

        // ── Sign-in call to action ───────────────────────────────────────
        let signed_out = !status.logged_in;
        self.sign_in_page.set_visible(signed_out);
        self.hero.set_visible(!signed_out);
        self.stats.set_visible(!signed_out);
        if signed_out {
            if status.credentials_editable {
                self.sign_in_page.set_description(Some(
                    "Enter your Private Internet Access username and password to use the VPN.",
                ));
                self.sign_in_btn.set_visible(true);
            } else {
                self.sign_in_page.set_description(Some(
                    "Your PIA credentials are managed by your vexos NixOS config (sops), \
                     and the configured credentials file is missing.",
                ));
                self.sign_in_btn.set_visible(false);
            }
            return;
        }

        // ── Connect toggle + state ───────────────────────────────────────
        let (pill, class, label, icon) = match (snap.unit_active, status.state) {
            (true, VpnState::Connected) => (
                "● CONNECTED",
                "state-connected",
                "DISCONNECT",
                "network-vpn-symbolic",
            ),
            (true, VpnState::Error) => (
                "● ERROR — RETRYING",
                "state-error",
                "STOP",
                "network-vpn-no-route-symbolic",
            ),
            (true, _) => (
                "● CONNECTING\u{2026}",
                "state-connecting",
                "CANCEL",
                "network-vpn-acquiring-symbolic",
            ),
            (false, VpnState::Error) => (
                "● ERROR",
                "state-error",
                "CONNECT",
                "network-vpn-disabled-symbolic",
            ),
            (false, _) => (
                "● DISCONNECTED",
                "state-disconnected",
                "CONNECT",
                "network-vpn-disabled-symbolic",
            ),
        };
        self.status_pill.set_label(pill);
        set_state_class(&self.status_pill, class);
        set_state_class(&self.connect_btn, class);
        self.btn_label.set_label(label);
        self.btn_icon.set_icon_name(Some(icon));

        // ── Region / protocol / since ────────────────────────────────────
        let connected = status.state == VpnState::Connected;
        if status.state.is_active() && !status.region_name.is_empty() {
            let proto = if status.protocol.is_empty() {
                &status.protocol_setting
            } else {
                &status.protocol
            };
            self.region_label.set_label(&format!(
                "{} \u{00b7} {}",
                status.region_name,
                protocol_label(proto)
            ));
        } else {
            self.region_label.set_label(&format!(
                "{} \u{00b7} {}",
                region_display(snap, &status.region_setting),
                protocol_label(&status.protocol_setting)
            ));
        }
        let detail = if connected {
            let mut parts = Vec::new();
            if let Some(since) = connected_since(&status.since) {
                parts.push(since);
            }
            if !status.server_cn.is_empty() {
                parts.push(status.server_cn.clone());
            }
            parts.join(" \u{00b7} ")
        } else if status.region_setting == vex_vpn::vexos::AUTO_REGION {
            "Region: automatic (fastest at connect time)".to_string()
        } else {
            String::new()
        };
        self.detail_label.set_label(&detail);

        // ── Kill switch chip ─────────────────────────────────────────────
        if status.killswitch_mode == KillSwitchMode::Off {
            self.ks_chip.set_visible(false);
        } else {
            self.ks_chip.set_visible(true);
            if status.killswitch {
                self.ks_chip.set_label("KILL SWITCH ON");
                set_state_class(&self.ks_chip, "state-connected");
            } else {
                self.ks_chip.set_label("KILL SWITCH OFF");
                set_state_class(&self.ks_chip, "state-disconnected");
            }
        }
        self.reconnect_btn
            .set_visible(snap.unit_active && connected);

        // ── Rates ────────────────────────────────────────────────────────
        let (rx, tx) = if connected {
            (snap.rates.rx_per_s, snap.rates.tx_per_s)
        } else {
            (0.0, 0.0)
        };
        self.dl_value
            .set_label(&format!("{}/s", format_bytes(rx.round() as u64)));
        self.ul_value
            .set_label(&format!("{}/s", format_bytes(tx.round() as u64)));
    }

    fn show_banner(&self, title: &str, action: BannerAction) {
        self.banner.set_title(&glib::markup_escape_text(title));
        self.banner.set_button_label(match action {
            BannerAction::SignIn => Some("Sign in"),
            BannerAction::Retry => Some("Retry"),
            BannerAction::None => None,
        });
        self.banner_action.set(action);
        self.banner.set_revealed(true);
    }
}

/// "Connected since 14:05 · 1h 5m" from an ISO 8601 timestamp.
fn connected_since(since: &str) -> Option<String> {
    let start = glib::DateTime::from_iso8601(since, None).ok()?;
    let local = start.to_local().ok()?;
    let clock = local.format("%H:%M").ok()?;
    let now = glib::DateTime::now_local().ok()?;
    let elapsed = (now.to_unix() - start.to_unix()).max(0) as u64;
    Some(format!(
        "Connected since {} \u{00b7} {}",
        clock,
        format_duration(elapsed)
    ))
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
