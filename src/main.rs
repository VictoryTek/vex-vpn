mod backend;
mod config;
mod dbus;
mod helper;
mod history;
mod parser;
mod profile;
mod state;
mod tray;
mod ui;
mod ui_import;
mod ui_prefs;
mod ui_profiles;

use anyhow::Result;
use gio::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::state::AppState;
use crate::tray::TrayMessage;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    info!("Starting vex-vpn");

    // Embed and register compiled icon resources.
    gio::resources_register_include!("icons.gresource")
        .expect("failed to register bundled GResources");

    let cfg = config::Config::load().unwrap_or_else(|e| {
        warn!("Failed to load config: {e:#}");
        config::Config::default()
    });

    let rt = tokio::runtime::Runtime::new()?;

    let app_state = Arc::new(RwLock::new(AppState::new_with_config(&cfg)));

    let (state_change_tx, _dummy_rx) = tokio::sync::broadcast::channel::<()>(16);

    // Spawn background poll loop.
    let state_for_poll = app_state.clone();
    let poll_tx = state_change_tx.clone();
    rt.spawn(async move {
        state::poll_loop(state_for_poll, poll_tx).await;
    });

    // Spawn VPN unit state watcher.
    let state_for_vpn_watch = app_state.clone();
    let vpn_watch_tx = state_change_tx.clone();
    rt.spawn(async move {
        state::watch_vpn_unit_state(state_for_vpn_watch, vpn_watch_tx).await;
    });

    // Spawn NetworkManager state watcher.
    let state_for_nm_watch = app_state.clone();
    rt.spawn(async move {
        state::watch_network_manager(state_for_nm_watch).await;
    });

    let (tray_tx, tray_rx) = async_channel::bounded::<TrayMessage>(8);

    let state_for_tray = app_state.clone();
    let tray_handle = rt.handle().clone();
    let state_rx = state_change_tx.subscribe();
    std::thread::spawn(move || {
        tray::run_tray(state_for_tray, tray_tx, tray_handle, state_rx);
    });

    let _guard = rt.enter();

    let app = adw::Application::builder()
        .application_id("com.vex.vpn.nixos")
        .build();

    register_app_actions(&app, app_state.clone());

    let state_for_ui = app_state.clone();
    app.connect_activate(move |app| {
        if !app.windows().is_empty() {
            if let Some(win) = app.active_window() {
                win.present();
            }
            return;
        }

        if let Some(display) = gtk4::gdk::Display::default() {
            gtk4::IconTheme::for_display(&display).add_resource_path("/com/vex/vpn/icons");
        }

        let rx = tray_rx.clone();
        build_and_show_main_window(app, state_for_ui.clone(), Some(rx));
    });

    std::process::exit(app.run().into());
}

fn build_and_show_main_window(
    app: &adw::Application,
    state: Arc<RwLock<AppState>>,
    rx: Option<async_channel::Receiver<TrayMessage>>,
) {
    let window = ui::build_ui(app, state, rx);
    window.present();
}

fn register_app_actions(app: &adw::Application, state: Arc<RwLock<AppState>>) {
    // Preferences action.
    let prefs_action = gio::SimpleAction::new("preferences", None);
    {
        let app_ref = app.clone();
        let state_ref = state.clone();
        prefs_action.connect_activate(move |_, _| {
            if let Some(win) = app_ref.active_window() {
                if let Ok(adw_win) = win.downcast::<adw::ApplicationWindow>() {
                    let prefs_win = ui_prefs::build_preferences_window(&adw_win, state_ref.clone());
                    prefs_win.present();
                }
            }
        });
    }
    app.add_action(&prefs_action);

    // Keyboard shortcuts action.
    let shortcuts_action = gio::SimpleAction::new("show-shortcuts", None);
    {
        let app_ref = app.clone();
        shortcuts_action.connect_activate(move |_, _| {
            if let Some(win) = app_ref.active_window() {
                if let Ok(adw_win) = win.downcast::<adw::ApplicationWindow>() {
                    ui::show_shortcuts_window(&adw_win);
                }
            }
        });
    }
    app.add_action(&shortcuts_action);

    // About action.
    let about_action = gio::SimpleAction::new("about", None);
    {
        let app_ref = app.clone();
        about_action.connect_activate(move |_, _| {
            if let Some(win) = app_ref.active_window() {
                if let Ok(adw_win) = win.downcast::<adw::ApplicationWindow>() {
                    ui::show_about_window(&adw_win);
                }
            }
        });
    }
    app.add_action(&about_action);

    // Quit action.
    let quit_action = gio::SimpleAction::new("quit", None);
    {
        let app_ref = app.clone();
        quit_action.connect_activate(move |_, _| {
            app_ref.quit();
        });
    }
    app.add_action(&quit_action);
    app.set_accels_for_action("app.quit", &["<Primary>Q"]);
}
