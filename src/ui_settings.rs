//! Settings / Account page: protocol, kill switch, PIA account, server list,
//! and the read-only options that live in the vexos NixOS config.

use crate::dbus;
use crate::state::AppState;
use crate::ui::Ui;
use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;
use std::cell::Cell;
use std::rc::Rc;
use vex_vpn::vexos::KillSwitchMode;

const PROTOCOLS: [&str; 2] = ["wireguard", "openvpn"];

pub struct SettingsPage {
    pub root: adw::PreferencesPage,
    protocol_row: adw::ComboRow,
    ks_row: adw::SwitchRow,
    ks_off_row: adw::ActionRow,
    account_row: adw::ActionRow,
    sign_in_btn: gtk4::Button,
    change_btn: gtk4::Button,
    sign_out_btn: gtk4::Button,
    test_row: adw::ActionRow,
    autoconnect_row: adw::ActionRow,
    mode_row: adw::ActionRow,
    /// Set while widgets are updated from status, so their change handlers
    /// do not fire actions.
    syncing: Rc<Cell<bool>>,
    /// A protocol / kill-switch change is in flight; don't overwrite the
    /// user's choice with the not-yet-updated status.
    busy: Rc<Cell<bool>>,
}

impl SettingsPage {
    pub fn new(ui: &Ui) -> Self {
        let root = adw::PreferencesPage::new();
        let syncing = Rc::new(Cell::new(false));
        let busy = Rc::new(Cell::new(false));

        // ── Connection ───────────────────────────────────────────────────
        let conn_group = adw::PreferencesGroup::builder().title("Connection").build();
        let protocol_row = adw::ComboRow::builder()
            .title("Protocol")
            .subtitle("WireGuard is recommended; OpenVPN is the backup")
            .model(&gtk4::StringList::new(&["WireGuard", "OpenVPN"]))
            .build();
        {
            let ui = ui.clone();
            let syncing = syncing.clone();
            let busy = busy.clone();
            protocol_row.connect_selected_notify(move |row| {
                if syncing.get() {
                    return;
                }
                let Some(proto) = PROTOCOLS.get(row.selected() as usize) else {
                    return;
                };
                busy.set(true);
                row.set_sensitive(false);
                let r = row.clone();
                let b = busy.clone();
                let unit = dbus::protocol_unit(proto);
                ui.run_unit(
                    async move { dbus::run_oneshot(&unit).await },
                    Some(format!(
                        "Protocol set to {}",
                        vex_vpn::vexos::protocol_label(proto)
                    )),
                    move || {
                        b.set(false);
                        r.set_sensitive(true);
                    },
                );
            });
        }
        conn_group.add(&protocol_row);
        root.add(&conn_group);

        // ── Kill switch ──────────────────────────────────────────────────
        let ks_group = adw::PreferencesGroup::builder().title("Kill switch").build();
        let ks_row = adw::SwitchRow::builder().title("Kill switch").build();
        {
            let ui = ui.clone();
            let syncing = syncing.clone();
            let busy = busy.clone();
            ks_row.connect_active_notify(move |row| {
                if syncing.get() {
                    return;
                }
                busy.set(true);
                row.set_sensitive(false);
                let r = row.clone();
                let b = busy.clone();
                let done = move || {
                    b.set(false);
                    r.set_sensitive(true);
                };
                if row.is_active() {
                    ui.run_unit(
                        dbus::start_unit(dbus::KILLSWITCH_UNIT),
                        Some("Kill switch on".to_string()),
                        done,
                    );
                } else {
                    ui.run_unit(
                        dbus::stop_unit(dbus::KILLSWITCH_UNIT),
                        Some("Kill switch off".to_string()),
                        done,
                    );
                }
            });
        }
        ks_group.add(&ks_row);
        let ks_off_row = adw::ActionRow::builder()
            .title("Kill switch not available")
            .subtitle("Kill switch mode is \u{201c}off\u{201d} in your vexos NixOS config")
            .visible(false)
            .build();
        ks_group.add(&ks_off_row);
        root.add(&ks_group);

        // ── Account ──────────────────────────────────────────────────────
        let account_group = adw::PreferencesGroup::builder()
            .title("PIA account")
            .description("Changing the account asks for your administrator password")
            .build();
        let account_row = adw::ActionRow::builder().title("Account").build();
        let sign_in_btn = row_button("Sign in", Some("suggested-action"));
        let change_btn = row_button("Change", None);
        let sign_out_btn = row_button("Sign out", Some("destructive-action"));
        {
            let ui = ui.clone();
            sign_in_btn.connect_clicked(move |_| crate::ui_login::sign_in(&ui, false));
            let ui2 = ui.clone();
            change_btn.connect_clicked(move |_| crate::ui_login::sign_in(&ui2, true));
            let ui3 = ui.clone();
            sign_out_btn.connect_clicked(move |_| crate::ui_login::sign_out(&ui3));
        }
        account_row.add_suffix(&sign_in_btn);
        account_row.add_suffix(&change_btn);
        account_row.add_suffix(&sign_out_btn);
        account_group.add(&account_row);

        let test_row = adw::ActionRow::builder()
            .title("Test login")
            .subtitle("Check the stored credentials with PIA")
            .build();
        let test_btn = row_button("Test", None);
        {
            let ui = ui.clone();
            test_btn.connect_clicked(move |_| crate::ui_login::test_login(&ui));
        }
        test_row.add_suffix(&test_btn);
        account_group.add(&test_row);
        root.add(&account_group);

        // ── Server list ──────────────────────────────────────────────────
        let servers_group = adw::PreferencesGroup::builder().title("Servers").build();
        let servers_row = adw::ActionRow::builder()
            .title("PIA server list")
            .subtitle("Download the latest list of regions (asks for your password)")
            .build();
        let refresh_btn = row_button("Refresh", None);
        {
            let ui = ui.clone();
            refresh_btn.connect_clicked(move |_| crate::ui_login::refresh_servers(&ui));
        }
        servers_row.add_suffix(&refresh_btn);
        servers_group.add(&servers_row);
        root.add(&servers_group);

        // ── Read-only system settings ────────────────────────────────────
        let system_group = adw::PreferencesGroup::builder()
            .title("System")
            .description("Set in your vexos NixOS config (vexos.vpn)")
            .build();
        let autoconnect_row = adw::ActionRow::builder().title("Connect at boot").build();
        let mode_row = adw::ActionRow::builder().title("Kill switch mode").build();
        system_group.add(&autoconnect_row);
        system_group.add(&mode_row);
        root.add(&system_group);

        Self {
            root,
            protocol_row,
            ks_row,
            ks_off_row,
            account_row,
            sign_in_btn,
            change_btn,
            sign_out_btn,
            test_row,
            autoconnect_row,
            mode_row,
            syncing,
            busy,
        }
    }

    pub fn update(&self, snap: &AppState) {
        let Some(status) = &snap.status else {
            return;
        };

        if !self.busy.get() {
            self.syncing.set(true);
            if let Some(i) = PROTOCOLS
                .iter()
                .position(|p| *p == status.protocol_setting)
            {
                self.protocol_row.set_selected(i as u32);
            }
            self.ks_row.set_active(status.killswitch);
            self.syncing.set(false);
        }

        // Kill switch
        let ks_available = status.killswitch_mode != KillSwitchMode::Off;
        self.ks_row.set_visible(ks_available);
        self.ks_off_row.set_visible(!ks_available);
        self.ks_row.set_subtitle(match status.killswitch_mode {
            KillSwitchMode::Always => {
                "Blocks all traffic outside the VPN tunnel. Always on at boot; \
                 turning it off asks for your password and lasts until reboot."
            }
            _ => "Blocks all traffic outside the VPN tunnel",
        });

        // Account
        let editable = status.credentials_editable;
        let signed_in = status.logged_in;
        self.account_row.set_subtitle(match (editable, signed_in) {
            (false, true) => "Managed by your vexos NixOS config (sops)",
            (false, false) => {
                "Managed by your vexos NixOS config (sops) \u{2014} credentials file missing"
            }
            (true, true) => "Signed in",
            (true, false) => "Signed out",
        });
        self.sign_in_btn.set_visible(!signed_in);
        self.change_btn.set_visible(signed_in);
        self.sign_out_btn.set_visible(signed_in);
        for btn in [&self.sign_in_btn, &self.change_btn, &self.sign_out_btn] {
            btn.set_sensitive(editable);
        }
        self.test_row.set_sensitive(signed_in);

        // Read-only
        self.autoconnect_row
            .set_subtitle(if status.autoconnect { "Yes" } else { "No" });
        self.mode_row.set_subtitle(match status.killswitch_mode {
            KillSwitchMode::Off => "Off \u{2014} no kill switch",
            KillSwitchMode::Manual => "Manual \u{2014} off at boot, toggle it here",
            KillSwitchMode::Always => "Always \u{2014} on at boot, before any network",
        });
    }
}

fn row_button(label: &str, class: Option<&str>) -> gtk4::Button {
    let btn = gtk4::Button::with_label(label);
    btn.set_valign(gtk4::Align::Center);
    if let Some(c) = class {
        btn.add_css_class(c);
    }
    btn
}
