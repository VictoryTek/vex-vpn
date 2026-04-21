mod app;
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

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    info!("Starting PIA GUI");

    let rt = tokio::runtime::Runtime::new()?;
    let _guard = rt.enter();

    let app_state = Arc::new(RwLock::new(AppState::default()));

    let app = adw::Application::builder()
        .application_id("com.pia.gui.nixos")
        .flags(gio::ApplicationFlags::FLAGS_NONE)
        .build();

    let state_clone = app_state.clone();
    app.connect_activate(move |app| {
        ui::build_ui(app, state_clone.clone());
    });

    // Start background state polling
    let state_for_poll = app_state.clone();
    rt.spawn(async move {
        state::poll_loop(state_for_poll).await;
    });

    // Start tray icon
    let state_for_tray = app_state.clone();
    std::thread::spawn(move || {
        tray::run_tray(state_for_tray);
    });

    let exit_code = app.run();
    std::process::exit(exit_code.into());
}
