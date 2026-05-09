//! First-run onboarding wizard — shown when no credentials exist.
//!
//! A 5-page `adw::Carousel` wizard presented as a standalone `adw::Window`
//! (not a child of the main window, since that window is not shown yet).
//! Navigation is button-driven only (swipe/wheel disabled).
//!
//! Pages:
//!   0 — Welcome
//!   1 — Sign In (authenticates against PIA, saves credentials to state)
//!   2 — Privacy Notice
//!   3 — Kill Switch
//!   4 — Done (calls on_complete)

use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::pia;
use crate::secrets::Credentials;
use crate::state::AppState;

/// Show the onboarding wizard. Calls `on_complete()` when the user finishes.
/// The main application window should be built inside `on_complete`.
pub fn show_onboarding(
    app: &adw::Application,
    state: Arc<RwLock<AppState>>,
    pia_client: Arc<pia::PiaClient>,
    on_complete: impl Fn() + 'static,
) {
    let win = adw::Window::builder()
        .application(app)
        .title("vex-vpn — Setup")
        .default_width(480)
        .default_height(520)
        .resizable(false)
        .modal(true)
        .deletable(false)
        .build();

    // Prevent closing via Escape / close button.
    win.connect_close_request(|_| glib::Propagation::Stop);

    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    // Carousel — button-driven only.
    let carousel = adw::Carousel::builder()
        .allow_scroll_wheel(false)
        .interactive(false)
        .vexpand(true)
        .build();

    // ── Page 0: Welcome ──────────────────────────────────────────────────
    let welcome_page = build_welcome_page();
    carousel.append(&welcome_page);

    // ── Page 1: Sign In ──────────────────────────────────────────────────
    let (signin_page, username_row, password_row, error_label, spinner) = build_signin_page();
    carousel.append(&signin_page);

    // ── Page 2: Privacy ──────────────────────────────────────────────────
    let privacy_page = build_privacy_page();
    carousel.append(&privacy_page);

    // ── Page 3: Kill Switch ──────────────────────────────────────────────
    let (ks_page, ks_switch) = build_kill_switch_page();
    carousel.append(&ks_page);

    // ── Page 4: Done ─────────────────────────────────────────────────────
    let done_page = build_done_page();
    carousel.append(&done_page);

    // Indicator dots.
    let dots = adw::CarouselIndicatorDots::builder()
        .carousel(&carousel)
        .build();

    // ── Navigation bar ───────────────────────────────────────────────────
    let nav_bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    nav_bar.set_margin_top(4);
    nav_bar.set_margin_bottom(12);
    nav_bar.set_margin_start(16);
    nav_bar.set_margin_end(16);

    let back_btn = gtk4::Button::with_label("← Back");
    back_btn.set_visible(false);

    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);

    let next_btn = gtk4::Button::with_label("Get started →");
    next_btn.add_css_class("suggested-action");

    nav_bar.append(&back_btn);
    nav_bar.append(&spacer);
    nav_bar.append(&next_btn);

    outer.append(&carousel);
    outer.append(&dots);
    outer.append(&nav_bar);

    win.set_content(Some(&outer));

    // ── Page-change handler: update button labels/visibility ─────────────
    {
        let back_btn_c = back_btn.clone();
        let next_btn_c = next_btn.clone();
        carousel.connect_page_changed(move |_, page_idx| {
            update_nav_buttons(&back_btn_c, &next_btn_c, page_idx);
        });
    }

    // ── Back button ──────────────────────────────────────────────────────
    {
        let carousel_c = carousel.clone();
        back_btn.connect_clicked(move |_| {
            let page_idx = carousel_c.position() as u32;
            if page_idx > 0 {
                let target = carousel_c.nth_page(page_idx - 1);
                carousel_c.scroll_to(&target, true);
            }
        });
    }

    // ── Next button ──────────────────────────────────────────────────────
    {
        let carousel_c = carousel.clone();
        let win_c = win.clone();
        let username_row_c = username_row.clone();
        let password_row_c = password_row.clone();
        let error_label_c = error_label.clone();
        let spinner_c = spinner.clone();
        let ks_switch_c = ks_switch.clone();
        let state_c = state.clone();
        let client_c = pia_client.clone();
        let on_complete = Arc::new(on_complete);

        next_btn.connect_clicked(move |btn| {
            let page_idx = carousel_c.position() as u32;
            let n_pages = carousel_c.n_pages();

            match page_idx {
                0 => {
                    // Welcome → Sign In
                    scroll_to_next(&carousel_c, page_idx, n_pages);
                }
                1 => {
                    // Sign In: attempt auth
                    let username = username_row_c.text().to_string();
                    let password = password_row_c.text().to_string();

                    if username.trim().is_empty() || password.is_empty() {
                        error_label_c.set_label("Username and password are required.");
                        error_label_c.set_visible(true);
                        return;
                    }

                    spinner_c.set_visible(true);
                    spinner_c.set_spinning(true);
                    btn.set_sensitive(false);
                    error_label_c.set_visible(false);

                    let carousel_inner = carousel_c.clone();
                    let error_inner = error_label_c.clone();
                    let spinner_inner = spinner_c.clone();
                    let btn_inner = btn.clone();
                    let state_inner = state_c.clone();
                    let client_inner = client_c.clone();
                    let n_pages_inner = n_pages;

                    glib::spawn_future_local(async move {
                        match client_inner.generate_token(&username, &password).await {
                            Ok(token) => {
                                // Save credentials
                                let creds = Credentials {
                                    username: username.clone(),
                                    password: password.clone(),
                                };
                                if let Err(e) = crate::secrets::save(&creds).await {
                                    tracing::error!("save credentials: {}", e);
                                }

                                // Store token in shared state
                                state_inner.write().await.auth_token = Some(token);

                                // Fetch server list in background
                                match client_inner.fetch_server_list().await {
                                    Ok(server_list) => {
                                        tracing::info!(
                                            "Loaded {} PIA regions",
                                            server_list.regions.len()
                                        );
                                        state_inner.write().await.regions = server_list.regions;
                                    }
                                    Err(e) => {
                                        tracing::warn!("Failed to fetch server list: {}", e);
                                    }
                                }

                                spinner_inner.set_spinning(false);
                                spinner_inner.set_visible(false);
                                btn_inner.set_sensitive(true);
                                scroll_to_next(&carousel_inner, 1, n_pages_inner);
                            }
                            Err(pia::PiaError::AuthFailed) => {
                                error_inner.set_label("Invalid username or password.");
                                error_inner.set_visible(true);
                                spinner_inner.set_spinning(false);
                                spinner_inner.set_visible(false);
                                btn_inner.set_sensitive(true);
                            }
                            Err(e) => {
                                error_inner.set_label(&format!("Connection error: {}", e));
                                error_inner.set_visible(true);
                                spinner_inner.set_spinning(false);
                                spinner_inner.set_visible(false);
                                btn_inner.set_sensitive(true);
                            }
                        }
                    });
                }
                2 => {
                    // Privacy → Kill Switch
                    scroll_to_next(&carousel_c, page_idx, n_pages);
                }
                3 => {
                    // Kill Switch → Done: save kill switch choice
                    let ks_active = ks_switch_c.is_active();
                    let mut cfg = crate::config::Config::load();
                    cfg.kill_switch_enabled = ks_active;
                    if let Err(e) = cfg.save() {
                        tracing::error!("save config (kill switch): {}", e);
                    }

                    if ks_active {
                        let iface = cfg.interface.clone();
                        glib::spawn_future_local(async move {
                            if let Err(e) = crate::helper::apply_kill_switch(&iface).await {
                                tracing::warn!("apply kill switch (onboarding): {}", e);
                            }
                        });
                    }

                    scroll_to_next(&carousel_c, page_idx, n_pages);
                }
                4 => {
                    // Done: close wizard, present main window
                    win_c.close();
                    (on_complete)();
                }
                _ => {}
            }
        });
    }

    win.present();
}

// ---------------------------------------------------------------------------
// Page builders
// ---------------------------------------------------------------------------

fn build_welcome_page() -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 18);
    page.set_halign(gtk4::Align::Center);
    page.set_valign(gtk4::Align::Center);
    page.set_margin_top(32);
    page.set_margin_bottom(32);
    page.set_margin_start(32);
    page.set_margin_end(32);

    let icon = gtk4::Image::from_icon_name("network-vpn-symbolic");
    icon.set_pixel_size(96);
    page.append(&icon);

    let title = gtk4::Label::new(Some("vex-vpn"));
    title.add_css_class("title-1");
    title.set_halign(gtk4::Align::Center);
    page.append(&title);

    let subtitle = gtk4::Label::new(Some("A secure PIA VPN client for NixOS"));
    subtitle.add_css_class("dim-label");
    subtitle.set_halign(gtk4::Align::Center);
    subtitle.set_wrap(true);
    page.append(&subtitle);

    let spacer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    spacer.set_vexpand(true);
    page.append(&spacer);

    page
}

fn build_signin_page() -> (
    gtk4::Box,
    adw::EntryRow,
    adw::PasswordEntryRow,
    gtk4::Label,
    gtk4::Spinner,
) {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    page.set_margin_top(24);
    page.set_margin_bottom(24);
    page.set_margin_start(24);
    page.set_margin_end(24);

    let heading = gtk4::Label::new(Some("Sign in to PIA"));
    heading.add_css_class("title-2");
    heading.set_halign(gtk4::Align::Start);
    page.append(&heading);

    let info = gtk4::Label::new(Some(
        "Enter your Private Internet Access credentials.\n\
         They are stored locally in ~/.config/vex-vpn/credentials.toml (mode 0600).",
    ));
    info.set_wrap(true);
    info.set_xalign(0.0);
    info.add_css_class("dim-label");
    page.append(&info);

    let group = adw::PreferencesGroup::new();
    let username_row = adw::EntryRow::builder().title("Username").build();
    let password_row = adw::PasswordEntryRow::builder().title("Password").build();
    group.add(&username_row);
    group.add(&password_row);
    page.append(&group);

    // Error label — hidden by default.
    let error_label = gtk4::Label::new(None);
    error_label.set_wrap(true);
    error_label.set_xalign(0.0);
    error_label.add_css_class("error");
    error_label.set_visible(false);
    page.append(&error_label);

    let spinner = gtk4::Spinner::new();
    spinner.set_visible(false);
    spinner.set_halign(gtk4::Align::Center);
    page.append(&spinner);

    (page, username_row, password_row, error_label, spinner)
}

fn build_privacy_page() -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    page.set_margin_top(24);
    page.set_margin_bottom(24);
    page.set_margin_start(24);
    page.set_margin_end(24);

    let icon = gtk4::Image::from_icon_name("security-symbolic");
    icon.set_pixel_size(48);
    icon.set_halign(gtk4::Align::Center);
    page.append(&icon);

    let heading = gtk4::Label::new(Some("What we store"));
    heading.add_css_class("title-2");
    heading.set_halign(gtk4::Align::Center);
    page.append(&heading);

    let bullets = gtk4::Label::new(Some(
        "• Your credentials are stored only on this device (mode 0600).\n\
         • Your IP address is never logged by this app.\n\
         • Credentials are never sent to any server other than PIA's official API.\n\
         • Only PIA's servers are contacted for VPN operations.",
    ));
    bullets.set_wrap(true);
    bullets.set_xalign(0.0);
    page.append(&bullets);

    page
}

fn build_kill_switch_page() -> (gtk4::Box, adw::SwitchRow) {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    page.set_margin_top(24);
    page.set_margin_bottom(24);
    page.set_margin_start(24);
    page.set_margin_end(24);

    let icon = gtk4::Image::from_icon_name("network-vpn-no-route-symbolic");
    icon.set_pixel_size(48);
    icon.set_halign(gtk4::Align::Center);
    page.append(&icon);

    let heading = gtk4::Label::new(Some("Kill Switch"));
    heading.add_css_class("title-2");
    heading.set_halign(gtk4::Align::Center);
    page.append(&heading);

    let desc = gtk4::Label::new(Some(
        "When enabled, all internet traffic is blocked if the VPN tunnel drops unexpectedly. \
         This prevents your real IP from leaking. You can change this later in Preferences.",
    ));
    desc.set_wrap(true);
    desc.set_xalign(0.0);
    desc.add_css_class("dim-label");
    page.append(&desc);

    let list = gtk4::ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::None);
    list.add_css_class("boxed-list");

    let ks_row = adw::SwitchRow::builder()
        .title("Enable Kill Switch")
        .subtitle("Block all traffic if VPN drops")
        .active(false)
        .build();
    list.append(&ks_row);
    page.append(&list);

    (page, ks_row)
}

fn build_done_page() -> gtk4::Box {
    let page = gtk4::Box::new(gtk4::Orientation::Vertical, 18);
    page.set_halign(gtk4::Align::Center);
    page.set_valign(gtk4::Align::Center);
    page.set_margin_top(32);
    page.set_margin_bottom(32);
    page.set_margin_start(32);
    page.set_margin_end(32);

    let icon = gtk4::Image::from_icon_name("emblem-ok-symbolic");
    icon.set_pixel_size(80);
    page.append(&icon);

    let title = gtk4::Label::new(Some("You're all set!"));
    title.add_css_class("title-1");
    title.set_halign(gtk4::Align::Center);
    page.append(&title);

    let subtitle = gtk4::Label::new(Some("Connect to any region and browse securely."));
    subtitle.add_css_class("dim-label");
    subtitle.set_halign(gtk4::Align::Center);
    subtitle.set_wrap(true);
    page.append(&subtitle);

    page
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn scroll_to_next(carousel: &adw::Carousel, current: u32, total: u32) {
    if current + 1 < total {
        let page = carousel.nth_page(current + 1);
        carousel.scroll_to(&page, true);
    }
}

fn update_nav_buttons(back_btn: &gtk4::Button, next_btn: &gtk4::Button, page_idx: u32) {
    back_btn.set_visible(page_idx > 0 && page_idx < 4);
    let label = match page_idx {
        0 => "Get started →",
        1 => "Sign in →",
        2 => "I understand →",
        3 => "Next →",
        _ => "Start browsing →",
    };
    next_btn.set_label(label);
}
