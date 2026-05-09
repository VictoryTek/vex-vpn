mod config;
mod dbus;
mod pia;
mod secrets;
mod state;
mod tray;
mod ui;
mod ui_login;

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

    // Load persisted user config (falls back to defaults on any error).
    let cfg = config::Config::load();

    // Build the Tokio runtime and keep it alive for the duration of main.
    let rt = tokio::runtime::Runtime::new()?;

    let app_state = Arc::new(RwLock::new(AppState::new_with_config(&cfg)));

    // Spawn background poll loop inside the runtime.
    let state_for_poll = app_state.clone();
    rt.spawn(async move {
        state::poll_loop(state_for_poll).await;
    });

    // Channel for tray→main-window messages.
    let (tray_tx, tray_rx) = std::sync::mpsc::sync_channel::<TrayMessage>(8);

    // Spawn system tray on its own thread with its own single-threaded runtime.
    let state_for_tray = app_state.clone();
    let tray_handle = rt.handle().clone();
    std::thread::spawn(move || {
        tray::run_tray(state_for_tray, tray_tx, tray_handle);
    });

    // The _guard keeps the Tokio context alive so that glib::spawn_future_local
    // closures can await Tokio futures.  Must NOT be assigned to _ (drops immediately).
    let _guard = rt.enter();

    let app = adw::Application::builder()
        .application_id("com.vex.vpn.nixos")
        .build();

    // Register application-level actions for the headerbar menu.
    register_app_actions(&app, app_state.clone());

    // Wrap the receiver so it can be moved into the Send closure below.
    let tray_rx = Arc::new(std::sync::Mutex::new(Some(tray_rx)));

    let state_for_ui = app_state.clone();
    app.connect_activate(move |app| {
        let rx = tray_rx.lock().unwrap().take();
        let window = ui::build_ui(app, state_for_ui.clone(), rx);

        // Build the PIA client once and share it.
        let pia_client = match pia::PiaClient::new() {
            Ok(c) => Arc::new(c),
            Err(e) => {
                warn!("Failed to create PIA client: {}", e);
                return;
            }
        };

        // First-run login: if no credentials are stored, prompt for them.
        // If credentials exist, auto-login in the background.
        match secrets::load() {
            Ok(Some(creds)) => {
                // Auto-login with existing credentials
                let state = state_for_ui.clone();
                let client = pia_client.clone();
                glib::spawn_future_local(async move {
                    auto_login(client, state, &creds.username, &creds.password).await;
                });
            }
            Ok(None) => {
                let state = state_for_ui.clone();
                let client = pia_client.clone();
                ui_login::show_login_dialog(&window, state, client);
            }
            Err(e) => warn!("load credentials: {}", e),
        }
    });

    let exit_code = app.run();
    std::process::exit(exit_code.into());
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

/// Register `app.about`, `app.quit`, and `app.switch-account` actions used by
/// the headerbar primary menu.
fn register_app_actions(app: &adw::Application, state: Arc<RwLock<AppState>>) {
    // Quit
    let quit_action = gio::SimpleAction::new("quit", None);
    {
        let app = app.clone();
        quit_action.connect_activate(move |_, _| app.quit());
    }
    app.add_action(&quit_action);

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
}
