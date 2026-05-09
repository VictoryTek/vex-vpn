mod app;
mod config;
mod dbus;
mod state;
mod tray;
mod ui;

use anyhow::Result;
use gtk4::prelude::*;
use libadwaita as adw;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

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

    // Wrap the receiver so it can be moved into the Send closure below.
    let tray_rx = Arc::new(std::sync::Mutex::new(Some(tray_rx)));

    let state_for_ui = app_state.clone();
    app.connect_activate(move |app| {
        let rx = tray_rx.lock().unwrap().take();
        ui::build_ui(app, state_for_ui.clone(), rx);
    });

    let exit_code = app.run();
    std::process::exit(exit_code.into());
}
