//! File import dialog — import a WireGuard .conf or OpenVPN .ovpn profile.

use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::Config;
use crate::parser::detect_vpn_type;
use crate::profile::{import_profile, VpnType};
use crate::state::AppState;

/// Build the import dialog navigation page.
pub fn build_import_page(
    state: Arc<RwLock<AppState>>,
    nav_view: adw::NavigationView,
) -> adw::NavigationPage {
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 16);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(480);

    let group = adw::PreferencesGroup::builder()
        .title("Import VPN Profile")
        .description("Select a WireGuard (.conf) or OpenVPN (.ovpn) configuration file")
        .build();

    // Profile name entry.
    let name_row = adw::EntryRow::builder()
        .title("Profile Name")
        .text("My VPN")
        .build();
    group.add(&name_row);

    // File path display row.
    let file_row = adw::ActionRow::new();
    file_row.set_title("Config File");
    file_row.set_subtitle("No file selected");
    file_row.set_activatable(true);

    // Store selected path in a Rc<RefCell> for access in the import button handler.
    let selected_path: std::rc::Rc<std::cell::RefCell<Option<std::path::PathBuf>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));

    {
        let file_row_c = file_row.clone();
        let selected_path_c = selected_path.clone();
        file_row.connect_activated(move |_| {
            let file_row_inner = file_row_c.clone();
            let selected_inner = selected_path_c.clone();
            glib::spawn_future_local(async move {
                let dialog = gtk4::FileDialog::builder()
                    .title("Select VPN Config File")
                    .build();

                let filter = gtk4::FileFilter::new();
                filter.add_pattern("*.conf");
                filter.add_pattern("*.ovpn");
                filter.set_name(Some("VPN Config Files"));
                let filters = gtk4::gio::ListStore::new::<gtk4::FileFilter>();
                filters.append(&filter);
                dialog.set_filters(Some(&filters));

                let window: Option<gtk4::Window> = None;
                match dialog.open_future(window.as_ref()).await {
                    Ok(file) => {
                        if let Some(path) = file.path() {
                            let name = path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("selected")
                                .to_string();
                            file_row_inner.set_subtitle(&name);
                            *selected_inner.borrow_mut() = Some(path);
                        }
                    }
                    Err(_) => {
                        // User cancelled.
                    }
                }
            });
        });
    }
    group.add(&file_row);

    // Import button.
    let import_btn = gtk4::Button::builder()
        .label("Import Profile")
        .css_classes(["suggested-action", "pill"])
        .halign(gtk4::Align::Center)
        .build();

    {
        let name_row_c = name_row.clone();
        let selected_c = selected_path.clone();
        let state_c = state.clone();
        let nav_c = nav_view.clone();

        import_btn.connect_clicked(move |btn| {
            let name = name_row_c.text().to_string();
            let path = selected_c.borrow().clone();

            let Some(path) = path else {
                // No file selected — show a subtle error by disabling button briefly.
                btn.set_sensitive(false);
                let btn_c = btn.clone();
                glib::timeout_add_seconds_local(1, move || {
                    btn_c.set_sensitive(true);
                    glib::ControlFlow::Break
                });
                return;
            };

            if name.trim().is_empty() {
                return;
            }

            let vpn_type = detect_vpn_type(&path).unwrap_or(VpnType::WireGuard);

            match import_profile(name.trim().to_string(), &path, vpn_type) {
                Ok(profile) => {
                    // Save profile to config.
                    let mut cfg = Config::load().unwrap_or_default();
                    cfg.profiles.push(profile);
                    let _ = cfg.save();

                    // Update in-memory state.
                    let state = state_c.clone();
                    glib::spawn_future_local(async move {
                        let cfg = Config::load().unwrap_or_default();
                        let mut s = state.write().await;
                        s.profiles = cfg.profiles;
                    });

                    // Navigate back.
                    nav_c.pop();
                }
                Err(e) => {
                    tracing::error!("import profile: {}", e);
                }
            }
        });
    }

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    vbox.append(&group);
    vbox.append(&import_btn);
    clamp.set_child(Some(&vbox));
    content.append(&clamp);

    adw::NavigationPage::builder()
        .title("Import Profile")
        .child(&content)
        .build()
}
