mod dbus;
mod state;
mod tray;
mod ui;
mod ui_login;
mod ui_regions;
mod ui_settings;

use anyhow::Result;
use gio::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tracing::info;

use crate::state::Ctx;
use crate::tray::TrayMessage;

const APP_ID: &str = "com.vex.vpn.nixos";

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

    // `--tray` (used by the session autostart service): start without a
    // window and keep running in the tray. Stripped before GApplication sees
    // the arguments, which would otherwise reject the unknown option.
    let mut args: Vec<String> = std::env::args().collect();
    let tray_mode = args.iter().skip(1).any(|a| a == "--tray");
    args.retain(|a| a != "--tray");

    let rt = tokio::runtime::Runtime::new()?;
    let ctx = Ctx::new(rt.handle().clone());
    rt.spawn(state::poll_loop(ctx.clone()));

    let _guard = rt.enter();

    let app = adw::Application::builder().application_id(APP_ID).build();
    register_app_actions(&app);

    // Tray and its message pump live only in the primary instance; a second
    // launch is forwarded here by GApplication and just activates.
    {
        let ctx = ctx.clone();
        app.connect_startup(move |app| {
            if let Some(display) = gtk4::gdk::Display::default() {
                gtk4::IconTheme::for_display(&display).add_resource_path("/com/vex/vpn/icons");
            }
            let (tray_tx, tray_rx) = async_channel::bounded::<TrayMessage>(8);
            tray::spawn(ctx.clone(), tray_tx);
            let app = app.clone();
            glib::spawn_future_local(async move {
                while let Ok(msg) = tray_rx.recv().await {
                    match msg {
                        TrayMessage::ShowWindow => app.activate(),
                        TrayMessage::ShowRegions => {
                            app.activate();
                            if let Some(win) =
                                app.active_window().and_downcast::<adw::ApplicationWindow>()
                            {
                                ActionGroupExt::activate_action(
                                    &win,
                                    "show-page",
                                    Some(&"regions".to_variant()),
                                );
                            }
                        }
                        TrayMessage::Quit => app.quit(),
                    }
                }
            });
        });
    }

    let skip_first_window = Rc::new(Cell::new(tray_mode));
    let hold = Rc::new(RefCell::new(None));
    app.connect_activate(move |app| {
        if let Some(win) = app.windows().first() {
            win.present();
            return;
        }
        if skip_first_window.replace(false) {
            // Stay alive with no window; closing a window later does not quit.
            hold.replace(Some(app.hold()));
            return;
        }
        ui::build_ui(app, ctx.clone()).present();
    });

    let code = app.run_with_args(&args);
    std::process::exit(code.into());
}

fn register_app_actions(app: &adw::Application) {
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
