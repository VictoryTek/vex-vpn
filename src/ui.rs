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

/// Only libadwaita colour variables, so the app follows the system light/dark
/// style and accent colour.
const APP_CSS: &str = r#"
.connect-btn {
    min-width: 120px;
    min-height: 120px;
    padding: 0;
    border-radius: 9999px;
}
.connect-btn.state-connected {
    background-color: var(--accent-bg-color);
    color: var(--accent-fg-color);
    box-shadow: 0 0 0 9px color-mix(in srgb, var(--accent-bg-color) 22%, transparent);
}
.connect-btn.state-connecting {
    color: var(--warning-color);
    box-shadow: 0 0 0 9px color-mix(in srgb, var(--warning-bg-color) 22%, transparent);
}
.connect-btn.state-error {
    background-color: var(--error-bg-color);
    color: var(--error-fg-color);
}
"#;

/// The app logo, bundled in the GResource.
const LOGO_RESOURCE: &str = "/com/vex/vpn/branding/vex-vpn.png";

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
        .title("Vex VPN")
        .default_width(900)
        .default_height(640)
        .build();

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

    let dashboard = Dashboard::new(&ui);
    let regions = crate::ui_regions::RegionsPage::new(&ui);
    let settings = crate::ui_settings::SettingsPage::new(&ui);

    // ── Status pane | Regions ────────────────────────────────────────────
    let status_header = adw::HeaderBar::new();
    status_header.set_title_widget(Some(&logo_title()));
    status_header.pack_end(&menu_button());
    let status_view = adw::ToolbarView::new();
    status_view.add_top_bar(&status_header);
    status_view.set_content(Some(&dashboard.root));

    let regions_view = adw::ToolbarView::new();
    regions_view.add_top_bar(&adw::HeaderBar::new());
    regions_view.set_content(Some(&regions.root));

    let split = adw::NavigationSplitView::new();
    split.set_min_sidebar_width(320.0);
    split.set_max_sidebar_width(360.0);
    split.set_sidebar_width_fraction(0.4);
    split.set_sidebar(Some(&adw::NavigationPage::new(&status_view, "Vex VPN")));
    split.set_content(Some(&adw::NavigationPage::new(&regions_view, "Regions")));

    // ── Backend missing ──────────────────────────────────────────────────
    let missing_header = adw::HeaderBar::new();
    missing_header.pack_end(&menu_button());
    let missing_view = adw::ToolbarView::new();
    missing_view.add_top_bar(&missing_header);
    missing_view.set_content(Some(&build_missing_page(&ctx)));

    let stack = gtk4::Stack::new();
    stack.set_transition_type(gtk4::StackTransitionType::Crossfade);
    stack.add_named(&split, Some("main"));
    stack.add_named(&missing_view, Some("missing"));

    // ── Preferences (built once, hidden on close) ────────────────────────
    let prefs = adw::PreferencesWindow::builder()
        .title("Preferences")
        .transient_for(&window)
        .modal(true)
        .hide_on_close(true)
        .search_enabled(false)
        .build();
    prefs.add(&settings.root);

    let show_prefs = gio::SimpleAction::new("preferences", None);
    {
        let prefs = prefs.clone();
        show_prefs.connect_activate(move |_, _| prefs.present());
    }
    window.add_action(&show_prefs);

    // `win.show-page('<name>')` — the tray uses this. Regions are always on
    // screen, so only "settings" needs to do anything.
    let show_page = gio::SimpleAction::new("show-page", Some(glib::VariantTy::STRING));
    {
        let prefs = prefs.clone();
        show_page.connect_activate(move |_, param| {
            if param.and_then(|p| p.str()) == Some("settings") {
                prefs.present();
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

    toast_overlay.set_child(Some(&stack));
    window.set_content(Some(&toast_overlay));

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
            stack.set_visible_child_name(if present { "main" } else { "missing" });
            show_prefs.set_enabled(present);
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

/// Header title: the app logo next to the app name.
fn logo_title() -> gtk4::Box {
    let title = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    let logo = gtk4::Image::from_resource(LOGO_RESOURCE);
    logo.set_pixel_size(22);
    let name = gtk4::Label::new(Some("Vex VPN"));
    name.add_css_class("heading");
    title.append(&logo);
    title.append(&name);
    title
}

fn menu_button() -> gtk4::MenuButton {
    gtk4::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .tooltip_text("Main menu")
        .primary(true)
        .menu_model(&build_primary_menu())
        .build()
}

// ---------------------------------------------------------------------------
// Primary menu
// ---------------------------------------------------------------------------

pub fn build_primary_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let view_section = gio::Menu::new();
    view_section.append(Some("Preferences"), Some("win.preferences"));
    view_section.append(Some("Keyboard Shortcuts"), Some("app.show-shortcuts"));
    menu.append_section(None, &view_section);

    let app_section = gio::Menu::new();
    app_section.append(Some("About Vex VPN"), Some("app.about"));
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
        .application_name("Vex VPN")
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
        .paintable(&gtk4::gdk::Texture::from_resource(LOGO_RESOURCE))
        .title("vexos-vpn backend not installed")
        .description(
            "Vex VPN is the desktop app for vexos-vpn, the VPN service that ships \
             with vexos. Enable it in your vexos NixOS config (modules/vpn.nix) \
             and rebuild.",
        )
        .child(&check)
        .build()
}

// ---------------------------------------------------------------------------
// Dashboard (status pane)
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
    connect_btn: gtk4::Button,
    state_label: gtk4::Label,
    region_label: gtk4::Label,
    detail_label: gtk4::Label,
    reconnect_btn: gtk4::Button,
    details: gtk4::ListBox,
    ks_row: adw::SwitchRow,
    ks_off_row: adw::ActionRow,
    dl_value: gtk4::Label,
    ul_value: gtk4::Label,
    /// Set while the kill switch row is updated from status, so its change
    /// handler does not fire an action.
    syncing: Rc<Cell<bool>>,
    /// A kill switch change is in flight; don't overwrite the user's choice
    /// with the not-yet-updated status.
    ks_busy: Rc<Cell<bool>>,
}

impl Dashboard {
    fn new(ui: &Ui) -> Self {
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

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
        page.set_margin_top(24);
        page.set_margin_bottom(24);
        page.set_margin_start(20);
        page.set_margin_end(20);
        page.set_valign(gtk4::Align::Center);

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
            .paintable(&gtk4::gdk::Texture::from_resource(LOGO_RESOURCE))
            .title("Sign in to PIA")
            .description("Enter your Private Internet Access username and password to use the VPN.")
            .child(&sign_in_btn)
            .visible(false)
            .build();
        sign_in_page.add_css_class("compact");
        page.append(&sign_in_page);

        // ── Hero ─────────────────────────────────────────────────────────
        let hero = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
        hero.set_halign(gtk4::Align::Center);
        hero.set_margin_bottom(24);

        let connect_btn = gtk4::Button::from_icon_name("system-shutdown-symbolic");
        connect_btn.set_css_classes(&["connect-btn", "state-disconnected"]);
        connect_btn.set_halign(gtk4::Align::Center);
        connect_btn.set_margin_top(12);
        connect_btn.set_margin_bottom(18);
        if let Some(icon) = connect_btn.child() {
            if let Ok(img) = icon.downcast::<gtk4::Image>() {
                img.set_pixel_size(44);
            }
        }
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

        let state_label = gtk4::Label::new(None);
        state_label.add_css_class("title-1");
        let region_label = gtk4::Label::new(None);
        region_label.set_wrap(true);
        region_label.set_justify(gtk4::Justification::Center);
        let detail_label = gtk4::Label::new(None);
        detail_label.add_css_class("dim-label");
        detail_label.set_wrap(true);
        detail_label.set_justify(gtk4::Justification::Center);
        hero.append(&state_label);
        hero.append(&region_label);
        hero.append(&detail_label);

        let reconnect_btn = gtk4::Button::with_label("Reconnect");
        reconnect_btn.add_css_class("pill");
        reconnect_btn.set_halign(gtk4::Align::Center);
        reconnect_btn.set_margin_top(10);
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
        hero.append(&reconnect_btn);
        page.append(&hero);

        // ── Kill switch + live rates ─────────────────────────────────────
        let details = gtk4::ListBox::new();
        details.set_selection_mode(gtk4::SelectionMode::None);
        details.add_css_class("boxed-list");

        let syncing = Rc::new(Cell::new(false));
        let ks_busy = Rc::new(Cell::new(false));
        let ks_row = adw::SwitchRow::builder().title("Kill switch").build();
        {
            let ui = ui.clone();
            let syncing = syncing.clone();
            let busy = ks_busy.clone();
            ks_row.connect_active_notify(move |row| {
                if syncing.get() {
                    return;
                }
                busy.set(true);
                row.set_sensitive(false);
                let r = row.clone();
                let b = busy.clone();
                let done = move || {
                    b.set(false);
                    r.set_sensitive(true);
                };
                if row.is_active() {
                    crate::ui_settings::enable_kill_switch(&ui, done);
                } else {
                    ui.run_unit(
                        dbus::stop_unit(dbus::KILLSWITCH_UNIT),
                        Some("Kill switch off".to_string()),
                        done,
                    );
                }
            });
        }
        details.append(&ks_row);
        let ks_off_row = adw::ActionRow::builder()
            .title("Kill switch not available")
            .subtitle("Kill switch mode is \u{201c}off\u{201d} in your vexos NixOS config")
            .visible(false)
            .build();
        details.append(&ks_off_row);

        let (dl_row, dl_value) = rate_row("Download");
        let (ul_row, ul_value) = rate_row("Upload");
        details.append(&dl_row);
        details.append(&ul_row);
        page.append(&details);

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
            connect_btn,
            state_label,
            region_label,
            detail_label,
            reconnect_btn,
            details,
            ks_row,
            ks_off_row,
            dl_value,
            ul_value,
            syncing,
            ks_busy,
        }
    }

    fn update(&self, snap: &AppState) {
        let Some(status) = &snap.status else {
            self.sign_in_page.set_visible(false);
            self.hero.set_visible(true);
            self.details.set_visible(false);
            match &snap.poll_error {
                Some(err) => self.show_banner(
                    &format!("Couldn't read the VPN status: {}", err),
                    BannerAction::None,
                ),
                None => self.banner.set_revealed(false),
            }
            self.state_label.set_label("Loading\u{2026}");
            set_state_class(&self.connect_btn, "state-disconnected");
            self.connect_btn.set_sensitive(false);
            self.region_label.set_label("");
            self.detail_label.set_label("");
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
        self.details.set_visible(!signed_out);
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
        let (title, class, action) = match (snap.unit_active, status.state) {
            (true, VpnState::Connected) => ("Connected", "state-connected", "Disconnect"),
            (true, VpnState::Error) => ("Error \u{2014} retrying", "state-error", "Stop"),
            (true, _) => ("Connecting\u{2026}", "state-connecting", "Cancel"),
            (false, VpnState::Error) => ("Error", "state-error", "Connect"),
            (false, _) => ("Disconnected", "state-disconnected", "Connect"),
        };
        self.state_label.set_label(title);
        set_state_class(&self.connect_btn, class);
        self.connect_btn.set_tooltip_text(Some(action));

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
        } else {
            "Your traffic is not protected".to_string()
        };
        self.detail_label.set_label(&detail);
        self.reconnect_btn
            .set_visible(snap.unit_active && connected);

        // ── Kill switch ──────────────────────────────────────────────────
        if !self.ks_busy.get() {
            self.syncing.set(true);
            self.ks_row.set_active(status.killswitch);
            self.syncing.set(false);
        }
        let ks_available = status.killswitch_mode != KillSwitchMode::Off;
        self.ks_row.set_visible(ks_available);
        self.ks_off_row.set_visible(!ks_available);
        self.ks_row.set_subtitle(match status.killswitch_mode {
            KillSwitchMode::Always => {
                "Blocks all traffic outside the VPN tunnel. Always on at boot; \
                 turning it off asks for your password and lasts until reboot."
            }
            _ => "Blocks all traffic outside the VPN tunnel",
        });

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

fn rate_row(title: &str) -> (adw::ActionRow, gtk4::Label) {
    let row = adw::ActionRow::builder().title(title).build();
    let value = gtk4::Label::new(Some("0 B/s"));
    value.add_css_class("numeric");
    row.add_suffix(&value);
    (row, value)
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
