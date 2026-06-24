//! Profile list and management UI page.

use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::{Config, VpnProfile, VpnType};
use crate::state::AppState;

/// Build the profiles navigation page.
pub fn build_profiles_page(
    state: Arc<RwLock<AppState>>,
    nav_view: adw::NavigationView,
) -> adw::NavigationPage {
    let page_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    // Top action bar with Import button.
    let action_bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    action_bar.set_margin_top(12);
    action_bar.set_margin_bottom(8);
    action_bar.set_margin_start(16);
    action_bar.set_margin_end(16);

    let spacer = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    action_bar.append(&spacer);

    let import_btn = gtk4::Button::builder()
        .label("+ Import")
        .css_classes(["suggested-action"])
        .build();

    {
        let state_c = state.clone();
        let nav_c = nav_view.clone();
        import_btn.connect_clicked(move |_| {
            let import_page = crate::ui_import::build_import_page(state_c.clone(), nav_c.clone());
            nav_c.push(&import_page);
        });
    }
    action_bar.append(&import_btn);
    page_box.append(&action_bar);

    // Profile list.
    let list_box = gtk4::ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    let scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .vexpand(true)
        .child(&list_box)
        .build();

    let clamp = adw::Clamp::new();
    clamp.set_child(Some(&scroll));
    clamp.set_maximum_size(600);
    clamp.set_margin_start(12);
    clamp.set_margin_end(12);
    clamp.set_margin_bottom(12);
    page_box.append(&clamp);

    // Populate list from config.
    populate_profile_list(&list_box, state.clone(), nav_view.clone());

    adw::NavigationPage::builder()
        .title("VPN Profiles")
        .child(&page_box)
        .build()
}

fn populate_profile_list(
    list_box: &gtk4::ListBox,
    state: Arc<RwLock<AppState>>,
    nav_view: adw::NavigationView,
) {
    let cfg = Config::load().unwrap_or_default();

    if cfg.profiles.is_empty() {
        let row = adw::ActionRow::new();
        row.set_title("No profiles yet");
        row.set_subtitle("Click \u{201c}+ Import\u{201d} to add a VPN profile");
        list_box.append(&row);
        return;
    }

    for profile in &cfg.profiles {
        let row = build_profile_row(profile, state.clone(), nav_view.clone());
        list_box.append(&row);
    }
}

fn build_profile_row(
    profile: &VpnProfile,
    state: Arc<RwLock<AppState>>,
    _nav_view: adw::NavigationView,
) -> adw::ActionRow {
    let type_label = match profile.vpn_type {
        VpnType::WireGuard => "WireGuard",
        VpnType::OpenVpn => "OpenVPN",
    };

    let row = adw::ActionRow::new();
    row.set_title(&profile.name);
    row.set_subtitle(type_label);
    row.set_activatable(true);

    let vpn_icon = gtk4::Image::from_icon_name("network-vpn-symbolic");
    vpn_icon.set_pixel_size(16);
    row.add_prefix(&vpn_icon);

    let chevron = gtk4::Image::from_icon_name("go-next-symbolic");
    chevron.add_css_class("dim-label");
    row.add_suffix(&chevron);

    // Connect button in the row.
    let connect_btn = gtk4::Button::builder()
        .label("Connect")
        .valign(gtk4::Align::Center)
        .css_classes(["pill"])
        .build();

    let profile_id = profile.id.clone();
    let profile_iface = profile.effective_interface().to_string();
    {
        let state_c = state.clone();
        connect_btn.connect_clicked(move |_btn| {
            let id = profile_id.clone();
            let iface = profile_iface.clone();
            let state = state_c.clone();
            glib::spawn_future_local(async move {
                // Set as active profile.
                {
                    let mut s = state.write().await;
                    s.active_profile_id = Some(id.clone());
                }
                // Save to config.
                let mut cfg = Config::load().unwrap_or_default();
                cfg.active_profile_id = Some(id);
                let _ = cfg.save();

                if let Err(e) = crate::dbus::start_wireguard_unit(&iface).await {
                    tracing::error!("connect: {}", e);
                }
            });
        });
    }
    row.add_suffix(&connect_btn);

    row
}

/// Build the profile detail page (pushed when a profile row is activated).
#[allow(dead_code)]
pub fn build_profile_detail_page(
    profile: VpnProfile,
    _state: Arc<RwLock<AppState>>,
) -> adw::NavigationPage {
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    let clamp = adw::Clamp::new();
    clamp.set_maximum_size(540);
    clamp.set_margin_top(12);
    clamp.set_margin_bottom(12);
    clamp.set_margin_start(12);
    clamp.set_margin_end(12);

    let group = adw::PreferencesGroup::builder()
        .title("Profile Settings")
        .build();

    // Profile name (read-only for now).
    let name_row = adw::ActionRow::new();
    name_row.set_title("Name");
    name_row.set_subtitle(&profile.name);
    group.add(&name_row);

    // VPN type (read-only).
    let type_row = adw::ActionRow::new();
    type_row.set_title("Protocol");
    type_row.set_subtitle(&profile.vpn_type.to_string());
    group.add(&type_row);

    // Auto-connect toggle.
    let auto_row = adw::SwitchRow::builder()
        .title("Auto-connect")
        .subtitle("Connect automatically at startup")
        .active(profile.auto_connect)
        .build();
    {
        let id = profile.id.clone();
        auto_row.connect_active_notify(move |row| {
            let active = row.is_active();
            let mut cfg = Config::load().unwrap_or_default();
            if let Some(p) = cfg.find_profile_mut(&id) {
                p.auto_connect = active;
            }
            let _ = cfg.save();
        });
    }
    group.add(&auto_row);

    // Kill switch toggle.
    let ks_row = adw::SwitchRow::builder()
        .title("Kill Switch")
        .subtitle("Block all traffic if VPN drops")
        .active(profile.kill_switch)
        .build();
    {
        let id = profile.id.clone();
        ks_row.connect_active_notify(move |row| {
            let active = row.is_active();
            let mut cfg = Config::load().unwrap_or_default();
            if let Some(p) = cfg.find_profile_mut(&id) {
                p.kill_switch = active;
            }
            let _ = cfg.save();
            glib::spawn_future_local(async move {
                let res = if active {
                    crate::helper::apply_kill_switch().await
                } else {
                    crate::helper::remove_kill_switch().await
                };
                if let Err(e) = res {
                    tracing::warn!("kill switch toggle: {}", e);
                }
            });
        });
    }
    group.add(&ks_row);

    // DNS override entry.
    let dns_row = adw::EntryRow::builder()
        .title("DNS Override")
        .text(profile.dns_override.as_deref().unwrap_or(""))
        .build();
    {
        let id = profile.id.clone();
        let row = dns_row.clone();
        dns_row.connect_apply(move |_| {
            let text = row.text().to_string();
            let mut cfg = Config::load().unwrap_or_default();
            if let Some(p) = cfg.find_profile_mut(&id) {
                p.dns_override = if text.is_empty() { None } else { Some(text) };
            }
            let _ = cfg.save();
        });
    }
    group.add(&dns_row);

    // Delete button.
    let delete_group = adw::PreferencesGroup::new();
    let delete_btn = gtk4::Button::builder()
        .label("Delete Profile")
        .css_classes(["destructive-action"])
        .halign(gtk4::Align::Center)
        .build();
    {
        let id = profile.id.clone();
        let profile_clone = profile.clone();
        delete_btn.connect_clicked(move |_| {
            let mut cfg = Config::load().unwrap_or_default();
            cfg.profiles.retain(|p| p.id != id);
            if cfg.active_profile_id.as_deref() == Some(&id) {
                cfg.active_profile_id = None;
            }
            let _ = cfg.save();
            let _ = crate::profile::delete_profile_dir(&profile_clone);
        });
    }
    delete_group.add(&delete_btn);

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    vbox.append(&group);
    vbox.append(&delete_group);
    clamp.set_child(Some(&vbox));
    content.append(&clamp);

    adw::NavigationPage::builder()
        .title(&profile.name)
        .child(&content)
        .build()
}
