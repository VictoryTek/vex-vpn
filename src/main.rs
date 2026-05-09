mod config;
mod dbus;
mod helper;
mod history;
mod pia;
mod secrets;
mod state;
mod tray;
mod ui;
mod ui_login;
mod ui_onboarding;
mod ui_prefs;

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

    // Embed and register compiled icon resources (must happen before GTK init).
    gio::resources_register_include!("icons.gresource")
        .expect("failed to register bundled GResources");

    // Load persisted user config (falls back to defaults on any error).
    let cfg = config::Config::load().unwrap_or_else(|e| {
        warn!("Failed to load config: {e:#}");
        config::Config::default()
    });

    // Build the Tokio runtime and keep it alive for the duration of main.
    let rt = tokio::runtime::Runtime::new()?;

    let app_state = Arc::new(RwLock::new(AppState::new_with_config(&cfg)));

    // Create state-change broadcast channel (capacity 16 — infrequent status changes).
    let (state_change_tx, _dummy_rx) = tokio::sync::broadcast::channel::<()>(16);

    // Spawn background poll loop inside the runtime.
    let state_for_poll = app_state.clone();
    let poll_tx = state_change_tx.clone();
    rt.spawn(async move {
        state::poll_loop(state_for_poll, poll_tx).await;
    });

    // Spawn VPN unit state watcher — triggers an immediate poll on ActiveState changes.
    let state_for_vpn_watch = app_state.clone();
    let vpn_watch_tx = state_change_tx.clone();
    rt.spawn(async move {
        state::watch_vpn_unit_state(state_for_vpn_watch, vpn_watch_tx).await;
    });

    // Spawn NetworkManager state watcher — auto-reconnects when network is restored.
    let state_for_nm_watch = app_state.clone();
    rt.spawn(async move {
        state::watch_network_manager(state_for_nm_watch).await;
    });

    // Channel for tray→main-window messages.
    let (tray_tx, tray_rx) = async_channel::bounded::<TrayMessage>(8);

    // Spawn system tray on its own thread; pass a fresh broadcast receiver.
    let state_for_tray = app_state.clone();
    let tray_handle = rt.handle().clone();
    let state_rx = state_change_tx.subscribe();
    std::thread::spawn(move || {
        tray::run_tray(state_for_tray, tray_tx, tray_handle, state_rx);
    });

    // The _guard keeps the Tokio context alive so that glib::spawn_future_local
    // closures can await Tokio futures.  Must NOT be assigned to _ (drops immediately).
    let _guard = rt.enter();

    let app = adw::Application::builder()
        .application_id("com.vex.vpn.nixos")
        .build();

    // Register application-level actions for the headerbar menu.
    register_app_actions(&app, app_state.clone());

    let state_for_ui = app_state.clone();
    app.connect_activate(move |app| {
        // Register the bundled icon resource path with the default icon theme
        // so that GTK can find our fallback symbolic icons.
        if let Some(display) = gtk4::gdk::Display::default() {
            gtk4::IconTheme::for_display(&display).add_resource_path("/com/vex/vpn/icons");
        }

        // async_channel::Receiver is Clone — each activation gets a fresh clone
        // so re-activation (GNOME session restore) always receives the channel.
        let rx = tray_rx.clone();

        // Build the PIA client once and share it.
        let pia_client = match pia::PiaClient::new() {
            Ok(c) => Arc::new(c),
            Err(e) => {
                warn!("Failed to create PIA client: {}", e);
                return;
            }
        };

        match secrets::load_sync_hint() {
            Ok(Some(creds)) => {
                // Credentials exist — show main window immediately, auto-login in background.
                build_and_show_main_window(app, state_for_ui.clone(), Some(rx));
                let state = state_for_ui.clone();
                let client = pia_client;
                glib::spawn_future_local(async move {
                    auto_login(client, state, &creds.username, &creds.password).await;
                });
            }
            Ok(None) => {
                // First run — show onboarding wizard; main window built on completion.
                let app_clone = app.clone();
                let state_clone = state_for_ui.clone();
                ui_onboarding::show_onboarding(app, state_for_ui.clone(), pia_client, move || {
                    build_and_show_main_window(&app_clone, state_clone.clone(), Some(rx.clone()));
                });
            }
            Err(e) => warn!("load credentials: {}", e),
        }
    });

    let _exit_code = app.run();
    // Drop _guard before rt so the Tokio context exits cleanly.
    drop(_guard);
    Ok(())
}

/// Build the main window and make it visible.
fn build_and_show_main_window(
    app: &adw::Application,
    state: Arc<RwLock<AppState>>,
    rx: Option<async_channel::Receiver<TrayMessage>>,
) {
    ui::build_ui(app, state, rx);
}

/// Background auto-login: generate token and fetch server list.
async fn auto_login(
    client: Arc<pia::PiaClient>,
    state: Arc<RwLock<AppState>>,
    username: &str,
    password: &str,
) {
    match client.generate_token(username, password).await {
        Ok(token) => {
            info!("PIA token obtained");
            state.write().await.auth_token = Some(token);
        }
        Err(e) => {
            warn!("Auto-login failed: {}", e);
            return;
        }
    }

    match client.fetch_server_list().await {
        Ok(server_list) => {
            info!("Loaded {} PIA regions", server_list.regions.len());
            state.write().await.regions = server_list.regions;
        }
        Err(e) => {
            warn!("Failed to fetch server list: {}", e);
        }
    }
}

/// Register `app.about`, `app.quit`, `app.switch-account`, `app.preferences`,
/// and `app.show-shortcuts` actions used by the headerbar primary menu.
fn register_app_actions(app: &adw::Application, state: Arc<RwLock<AppState>>) {
    // Quit
    let quit_action = gio::SimpleAction::new("quit", None);
    {
        let app = app.clone();
        quit_action.connect_activate(move |_, _| app.quit());
    }
    app.add_action(&quit_action);
    app.set_accels_for_action("app.quit", &["<Control>q"]);

    // About
    let about_action = gio::SimpleAction::new("about", None);
    {
        let app = app.clone();
        about_action.connect_activate(move |_, _| {
            if let Some(window) = app
                .active_window()
                .and_then(|w| w.downcast::<adw::ApplicationWindow>().ok())
            {
                ui::show_about_window(&window);
            }
        });
    }
    app.add_action(&about_action);

    // Switch account — re-prompt and overwrite stored credentials.
    let switch_action = gio::SimpleAction::new("switch-account", None);
    {
        let app = app.clone();
        let state = state.clone();
        switch_action.connect_activate(move |_, _| {
            if let Some(window) = app
                .active_window()
                .and_then(|w| w.downcast::<adw::ApplicationWindow>().ok())
            {
                match pia::PiaClient::new() {
                    Ok(client) => {
                        ui_login::show_login_dialog(&window, state.clone(), Arc::new(client));
                    }
                    Err(e) => {
                        tracing::error!("Failed to create PIA client: {}", e);
                    }
                }
            }
        });
    }
    app.add_action(&switch_action);

    // Preferences — Ctrl+,
    let prefs_action = gio::SimpleAction::new("preferences", None);
    {
        let app = app.clone();
        let state_c = state.clone();
        prefs_action.connect_activate(move |_, _| {
            if let Some(window) = app
                .active_window()
                .and_then(|w| w.downcast::<adw::ApplicationWindow>().ok())
            {
                let prefs = ui_prefs::build_preferences_window(&window, state_c.clone());
                prefs.present();
            }
        });
    }
    app.add_action(&prefs_action);
    app.set_accels_for_action("app.preferences", &["<Control>comma"]);

    // Keyboard shortcuts — Ctrl+?
    let shortcuts_action = gio::SimpleAction::new("show-shortcuts", None);
    {
        let app = app.clone();
        shortcuts_action.connect_activate(move |_, _| {
            if let Some(window) = app
                .active_window()
                .and_then(|w| w.downcast::<adw::ApplicationWindow>().ok())
            {
                ui::show_shortcuts_window(&window);
            }
        });
    }
    app.add_action(&shortcuts_action);
    app.set_accels_for_action("app.show-shortcuts", &["<Control>question"]);
}
