//! Preferences window — General and Advanced pages.

use adw::prelude::*;
use libadwaita as adw;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::Config;
use crate::state::AppState;

/// Build and return the `adw::PreferencesWindow`.
pub fn build_preferences_window(
    parent: &adw::ApplicationWindow,
    _state: Arc<RwLock<AppState>>,
) -> adw::PreferencesWindow {
    let win = adw::PreferencesWindow::builder()
        .transient_for(parent)
        .modal(true)
        .title("Preferences")
        .build();

    win.add(&build_general_page());
    win.add(&build_advanced_page());

    win
}

// ---------------------------------------------------------------------------
// Page 1 — General
// ---------------------------------------------------------------------------

fn build_general_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();

    let cfg = Config::load().unwrap_or_default();
    let group = adw::PreferencesGroup::builder().title("Startup").build();

    // Start minimized.
    let minimized_row = adw::SwitchRow::builder()
        .title("Start Minimized")
        .subtitle("Launch to the system tray without showing the main window")
        .active(cfg.start_minimized)
        .build();
    minimized_row.connect_active_notify(move |row| {
        let mut c = Config::load().unwrap_or_default();
        c.start_minimized = row.is_active();
        let _ = c.save();
    });
    group.add(&minimized_row);

    // Show tray icon.
    let tray_row = adw::SwitchRow::builder()
        .title("Show Tray Icon")
        .subtitle("Display vex-vpn in the system notification area")
        .active(cfg.show_tray_icon)
        .build();
    tray_row.connect_active_notify(move |row| {
        let mut c = Config::load().unwrap_or_default();
        c.show_tray_icon = row.is_active();
        let _ = c.save();
    });
    group.add(&tray_row);

    // Auto-reconnect.
    let reconnect_row = adw::SwitchRow::builder()
        .title("Auto-Reconnect")
        .subtitle("Automatically reconnect when network is restored")
        .active(cfg.auto_reconnect)
        .build();
    reconnect_row.connect_active_notify(move |row| {
        let mut c = Config::load().unwrap_or_default();
        c.auto_reconnect = row.is_active();
        let _ = c.save();
    });
    group.add(&reconnect_row);

    page.add(&group);
    page
}

// ---------------------------------------------------------------------------
// Page 2 — Advanced
// ---------------------------------------------------------------------------

fn build_advanced_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Advanced")
        .icon_name("preferences-other-symbolic")
        .build();

    let group = adw::PreferencesGroup::builder()
        .title("Diagnostics")
        .build();

    let log_model = gtk4::StringList::new(&["info", "debug", "trace"]);
    let log_row = adw::ComboRow::builder()
        .title("Log Verbosity")
        .model(&log_model)
        .selected(0)
        .build();
    {
        log_row.connect_selected_notify(|row| {
            let level = match row.selected() {
                1 => "debug",
                2 => "trace",
                _ => "info",
            };
            // Apply at runtime via RUST_LOG env override (best-effort).
            unsafe {
                std::env::set_var("RUST_LOG", level);
            }
        });
    }
    group.add(&log_row);

    page.add(&group);
    page
}
