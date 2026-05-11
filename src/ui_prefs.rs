//! Preferences window — three-page `adw::PreferencesWindow`.
//!
//! Pages:
//!   - Connection: interface, max latency, DNS provider
//!   - Privacy:    kill switch toggle, allowed interfaces
//!   - Advanced:   auto-connect, log verbosity

use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::config::Config;
use crate::state::AppState;

/// Build and return the `adw::PreferencesWindow`.
/// The caller is responsible for calling `set_transient_for` and `present()`.
pub fn build_preferences_window(
    parent: &adw::ApplicationWindow,
    state: Arc<RwLock<AppState>>,
) -> adw::PreferencesWindow {
    let win = adw::PreferencesWindow::builder()
        .transient_for(parent)
        .modal(true)
        .title("Preferences")
        .build();

    win.add(&build_connection_page());
    win.add(&build_privacy_page());
    win.add(&build_advanced_page(state));

    win
}

// ---------------------------------------------------------------------------
// Page 1 — Connection
// ---------------------------------------------------------------------------

fn build_connection_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Connection")
        .icon_name("network-server-symbolic")
        .build();

    let group = adw::PreferencesGroup::builder().title("Network").build();

    let cfg = Config::load().unwrap_or_else(|e| {
        tracing::warn!("Failed to load config: {e:#}");
        Config::default()
    });

    // Interface name
    let iface_row = adw::EntryRow::builder()
        .title("Interface name")
        .text(&cfg.interface)
        .build();
    {
        let row = iface_row.clone();
        iface_row.connect_apply(move |_| {
            let text = row.text().to_string();
            if crate::config::validate_interface(&text) {
                let mut c = Config::load().unwrap_or_else(|e| {
                    tracing::warn!("Failed to load config: {e:#}");
                    Config::default()
                });
                c.interface = text;
                if let Err(e) = c.save() {
                    tracing::error!("save config (interface): {}", e);
                }
            }
        });
    }
    group.add(&iface_row);

    // Max latency
    let lat_row = adw::EntryRow::builder()
        .title("Max latency (ms)")
        .text(cfg.max_latency_ms.to_string())
        .build();
    {
        let row = lat_row.clone();
        lat_row.connect_apply(move |_| {
            if let Ok(ms) = row.text().parse::<u32>() {
                let mut c = Config::load().unwrap_or_else(|e| {
                    tracing::warn!("Failed to load config: {e:#}");
                    Config::default()
                });
                c.max_latency_ms = ms;
                if let Err(e) = c.save() {
                    tracing::error!("save config (max_latency_ms): {}", e);
                }
            }
        });
    }
    group.add(&lat_row);

    // DNS provider
    let dns_model = gtk4::StringList::new(&["pia", "google", "cloudflare", "custom"]);
    let dns_selected: u32 = match cfg.dns_provider.as_str() {
        "google" => 1,
        "cloudflare" => 2,
        "custom" => 3,
        _ => 0,
    };
    let dns_row = adw::ComboRow::builder()
        .title("DNS provider")
        .model(&dns_model)
        .selected(dns_selected)
        .build();
    {
        dns_row.connect_selected_notify(move |row| {
            let provider = match row.selected() {
                1 => "google",
                2 => "cloudflare",
                3 => "custom",
                _ => "pia",
            };
            let mut c = Config::load().unwrap_or_else(|e| {
                tracing::warn!("Failed to load config: {e:#}");
                Config::default()
            });
            c.dns_provider = provider.to_string();
            if let Err(e) = c.save() {
                tracing::error!("save config (dns_provider): {}", e);
            }
        });
    }
    group.add(&dns_row);

    page.add(&group);
    page
}

// ---------------------------------------------------------------------------
// Page 2 — Privacy
// ---------------------------------------------------------------------------

fn build_privacy_page() -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Privacy")
        .icon_name("security-symbolic")
        .build();

    let cfg = Config::load().unwrap_or_else(|e| {
        tracing::warn!("Failed to load config: {e:#}");
        Config::default()
    });

    // Kill switch group
    let ks_group = adw::PreferencesGroup::builder()
        .title("Kill Switch")
        .build();

    let ks_row = adw::SwitchRow::builder()
        .title("Enable Kill Switch")
        .subtitle("Block all traffic if VPN tunnel drops")
        .active(cfg.kill_switch_enabled)
        .build();
    {
        ks_row.connect_active_notify(move |row| {
            let active = row.is_active();
            let mut c = Config::load().unwrap_or_else(|e| {
                tracing::warn!("Failed to load config: {e:#}");
                Config::default()
            });
            c.kill_switch_enabled = active;
            if let Err(e) = c.save() {
                tracing::error!("save config (kill_switch_enabled): {}", e);
            }
            // Apply or remove kill switch at runtime (best-effort, log on failure).
            let iface = c.interface.clone();
            glib::spawn_future_local(async move {
                let res = if active {
                    crate::helper::apply_kill_switch(&iface).await
                } else {
                    crate::helper::remove_kill_switch().await
                };
                if let Err(e) = res {
                    tracing::warn!("kill switch (prefs): {}", e);
                }
            });
        });
    }
    ks_group.add(&ks_row);
    page.add(&ks_group);

    // Allowed interfaces group
    let ai_group = adw::PreferencesGroup::builder()
        .title("Allowed Interfaces")
        .description(
            "Comma-separated list of interfaces allowed through the kill switch (e.g. eth0,lo)",
        )
        .build();

    let ai_row = adw::EntryRow::builder()
        .title("Additional allowed interfaces")
        .text(cfg.kill_switch_allowed_ifaces.join(","))
        .build();
    {
        let row = ai_row.clone();
        ai_row.connect_apply(move |_| {
            let text = row.text().to_string();
            let ifaces: Vec<String> = text
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let mut c = Config::load().unwrap_or_else(|e| {
                tracing::warn!("Failed to load config: {e:#}");
                Config::default()
            });
            c.kill_switch_allowed_ifaces = ifaces;
            if let Err(e) = c.save() {
                tracing::error!("save config (kill_switch_allowed_ifaces): {}", e);
            }
        });
    }
    ai_group.add(&ai_row);
    page.add(&ai_group);

    page
}

// ---------------------------------------------------------------------------
// Page 3 — Advanced
// ---------------------------------------------------------------------------

fn build_advanced_page(state: Arc<RwLock<AppState>>) -> adw::PreferencesPage {
    let page = adw::PreferencesPage::builder()
        .title("Advanced")
        .icon_name("preferences-system-symbolic")
        .build();

    let cfg = Config::load().unwrap_or_else(|e| {
        tracing::warn!("Failed to load config: {e:#}");
        Config::default()
    });

    let group = adw::PreferencesGroup::new();

    // Auto-connect
    let ac_row = adw::SwitchRow::builder()
        .title("Auto Connect on Login")
        .subtitle("Automatically connect when a graphical session starts")
        .active(cfg.auto_connect)
        .build();
    {
        ac_row.connect_active_notify(move |row| {
            let mut c = Config::load().unwrap_or_else(|e| {
                tracing::warn!("Failed to load config: {e:#}");
                Config::default()
            });
            c.auto_connect = row.is_active();
            if let Err(e) = c.save() {
                tracing::error!("save config (auto_connect): {}", e);
            }
        });
    }
    group.add(&ac_row);

    // Auto-reconnect
    let ar_row = adw::SwitchRow::builder()
        .title("Auto-reconnect")
        .subtitle("Reconnect automatically when network connectivity is restored")
        .active(cfg.auto_reconnect)
        .build();
    {
        let state = state.clone();
        ar_row.connect_active_notify(move |row| {
            let active = row.is_active();
            let mut c = Config::load().unwrap_or_else(|e| {
                tracing::warn!("Failed to load config: {e:#}");
                Config::default()
            });
            c.auto_reconnect = active;
            if let Err(e) = c.save() {
                tracing::error!("save config (auto_reconnect): {}", e);
            }
            // Propagate immediately to AppState so the NM watcher respects the new value.
            let state = state.clone();
            glib::spawn_future_local(async move {
                state.write().await.auto_reconnect = active;
            });
        });
    }
    group.add(&ar_row);

    // Log verbosity
    let log_model = gtk4::StringList::new(&["info", "debug", "trace"]);
    let log_row = adw::ComboRow::builder()
        .title("Log level")
        .subtitle("Requires restart to take effect")
        .model(&log_model)
        .selected(0)
        .build();
    // Log level is informational only for now (env-filter based, set at startup).
    group.add(&log_row);

    page.add(&group);

    // ── VPN Backend Service ──────────────────────────────────────────────────
    let backend_group = adw::PreferencesGroup::builder()
        .title("VPN Backend Service")
        .description("The pia-vpn system service manages the WireGuard tunnel.")
        .build();

    let backend_status_row = adw::ActionRow::builder()
        .title("Service status")
        .subtitle("Checking\u{2026}")
        .build();
    backend_group.add(&backend_status_row);

    // Check install status asynchronously and update the subtitle.
    {
        let row = backend_status_row.clone();
        glib::spawn_future_local(async move {
            let installed = crate::dbus::is_service_unit_installed("pia-vpn.service").await;
            row.set_subtitle(if installed {
                "Installed"
            } else {
                "Not installed"
            });
        });
    }

    // Uninstall button — always shown; user triggers uninstall from here.
    let uninstall_btn = gtk4::Button::builder()
        .label("Remove VPN backend service")
        .css_classes(["destructive-action"])
        .margin_top(6)
        .margin_bottom(6)
        .halign(gtk4::Align::End)
        .build();

    {
        let status_row = backend_status_row.clone();
        uninstall_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            let row = status_row.clone();
            let btn_ref = btn.clone();
            glib::spawn_future_local(async move {
                match crate::helper::uninstall_backend().await {
                    Ok(()) => {
                        row.set_subtitle("Not installed");
                    }
                    Err(e) => {
                        tracing::error!("uninstall_backend: {}", e);
                        btn_ref.set_sensitive(true);
                        row.set_subtitle(&format!("Error: {e:#}"));
                    }
                }
            });
        });
    }

    backend_group.add(&uninstall_btn);
    page.add(&backend_group);

    page
}
