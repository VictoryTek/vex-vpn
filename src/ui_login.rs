//! Account dialogs and other root-only actions that go through
//! `pkexec vexos-vpn …` (polkit admin prompt).
//!
//! The PIA password is read from the entry, the entry is cleared at once,
//! and the value is handed to `cli::Credentials`, which writes it to pkexec's
//! stdin and wipes it. GTK/GLib's own internal copies (the entry buffer and
//! the returned GString) are outside our control; we drop them immediately.

use crate::dbus;
use crate::ui::Ui;
use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;
use vex_vpn::cli::{self, Credentials, PrivOutcome, Privileged};

/// Claim the single pkexec slot, or tell the user one is already pending.
fn begin(ui: &Ui) -> bool {
    if ui.priv_busy.replace(true) {
        ui.toast("Another action is waiting for authentication");
        return false;
    }
    true
}

/// Report a pkexec outcome. Returns the stdout on success.
fn report(ui: &Ui, what: &str, result: anyhow::Result<PrivOutcome>) -> Option<String> {
    match result {
        Ok(PrivOutcome::Ok { stdout }) => Some(stdout),
        Ok(PrivOutcome::Cancelled { no_agent: false }) => {
            ui.toast("Authentication cancelled");
            None
        }
        Ok(PrivOutcome::Cancelled { no_agent: true }) => {
            show_message(
                ui,
                &format!("{} failed", what),
                "No polkit authentication agent is running, so the administrator \
                 password prompt cannot be shown.",
            );
            None
        }
        Ok(PrivOutcome::Failed { stdout, stderr, .. }) => {
            let body = [stdout.trim(), stderr.trim()]
                .iter()
                .filter(|s| !s.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            show_message(ui, &format!("{} failed", what), &body);
            None
        }
        Err(e) => {
            show_message(ui, &format!("{} failed", what), &format!("{e:#}"));
            None
        }
    }
}

pub fn show_message(ui: &Ui, heading: &str, body: &str) {
    let dialog = adw::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .heading(heading)
        .body(body)
        .build();
    dialog.add_response("close", "Close");
    dialog.present();
}

// ---------------------------------------------------------------------------
// Sign in / change account
// ---------------------------------------------------------------------------

pub fn sign_in(ui: &Ui, change: bool) {
    let dialog = adw::MessageDialog::builder()
        .transient_for(&ui.window)
        .modal(true)
        .heading(if change {
            "Change PIA account"
        } else {
            "Sign in to PIA"
        })
        .body(
            "Your Private Internet Access username and password are stored by \
             vexos-vpn in a file only root can read. You will be asked for your \
             administrator password.",
        )
        .build();

    let user_row = adw::EntryRow::builder().title("Username").build();
    let pass_row = adw::PasswordEntryRow::builder().title("Password").build();
    let group = adw::PreferencesGroup::new();
    group.add(&user_row);
    group.add(&pass_row);
    dialog.set_extra_child(Some(&group));

    dialog.add_response("cancel", "Cancel");
    dialog.add_response("sign-in", if change { "Save" } else { "Sign in" });
    dialog.set_response_appearance("sign-in", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("sign-in"));
    dialog.set_close_response("cancel");
    dialog.set_response_enabled("sign-in", false);

    let refresh = {
        let dialog = dialog.clone();
        let user_row = user_row.clone();
        let pass_row = pass_row.clone();
        move || {
            let ok = cli::validate(&user_row.text(), &pass_row.text()).is_ok();
            dialog.set_response_enabled("sign-in", ok);
        }
    };
    {
        let r = refresh.clone();
        user_row.connect_changed(move |_| r());
        pass_row.connect_changed(move |_| refresh());
    }
    {
        let dialog = dialog.clone();
        pass_row.connect_entry_activated(move |_| {
            if dialog.is_response_enabled("sign-in") {
                dialog.response("sign-in");
            }
        });
    }

    let ui = ui.clone();
    dialog.connect_response(None, move |_, response| {
        let user = user_row.text().to_string();
        let pass = pass_row.text().to_string();
        user_row.set_text("");
        pass_row.set_text("");
        if response != "sign-in" {
            drop(Credentials::new(user, pass));
            return;
        }
        let creds = match Credentials::new(user, pass) {
            Ok(c) => c,
            Err(msg) => {
                ui.toast(msg);
                return;
            }
        };
        if !begin(&ui) {
            return;
        }
        let ui = ui.clone();
        glib::spawn_future_local(async move {
            let was_signed_in = ui.ctx.snapshot().await.status.is_some_and(|s| s.logged_in);
            let result = ui
                .ctx
                .run(async move { Privileged::default().login(creds).await })
                .await;
            ui.priv_busy.set(false);
            if report(&ui, "Sign in", result).is_some() {
                if was_signed_in {
                    ui.toast("PIA account updated");
                    ui.ctx.poke();
                } else {
                    // First sign-in: connect right away.
                    ui.run_unit(
                        dbus::start_unit(dbus::VPN_UNIT),
                        Some("Signed in \u{2014} connecting\u{2026}".to_string()),
                        || {},
                    );
                }
            }
        });
    });

    dialog.present();
}

// ---------------------------------------------------------------------------
// Sign out
// ---------------------------------------------------------------------------

pub fn sign_out(ui: &Ui) {
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let ks_on = ui.ctx.snapshot().await.status.is_some_and(|s| s.killswitch);
        let mut body = String::from(
            "This removes your PIA credentials from this computer and disconnects the VPN.",
        );
        if ks_on {
            body.push_str(
                " The kill switch is on, so this computer will have no internet \
                 access until you sign in again or turn the kill switch off.",
            );
        }
        let dialog = adw::MessageDialog::builder()
            .transient_for(&ui.window)
            .modal(true)
            .heading("Sign out of PIA?")
            .body(body)
            .build();
        dialog.add_response("cancel", "Cancel");
        dialog.add_response("sign-out", "Sign out");
        dialog.set_response_appearance("sign-out", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let ui2 = ui.clone();
        dialog.connect_response(Some("sign-out"), move |_, _| {
            if !begin(&ui2) {
                return;
            }
            let ui = ui2.clone();
            glib::spawn_future_local(async move {
                let result = ui
                    .ctx
                    .run(async { Privileged::default().logout().await })
                    .await;
                ui.priv_busy.set(false);
                if report(&ui, "Sign out", result).is_some() {
                    ui.toast("Signed out");
                }
                ui.ctx.poke();
            });
        });
        dialog.present();
    });
}

// ---------------------------------------------------------------------------
// Test login / refresh server list
// ---------------------------------------------------------------------------

pub fn test_login(ui: &Ui) {
    if !begin(ui) {
        return;
    }
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let result = ui
            .ctx
            .run(async { Privileged::default().selftest().await })
            .await;
        ui.priv_busy.set(false);
        if let Some(stdout) = report(&ui, "Login test", result) {
            show_message(&ui, "Login test passed", stdout.trim());
        }
    });
}

pub fn refresh_servers(ui: &Ui) {
    if !begin(ui) {
        return;
    }
    let ui = ui.clone();
    glib::spawn_future_local(async move {
        let result = ui
            .ctx
            .run(async { Privileged::default().refresh().await })
            .await;
        ui.priv_busy.set(false);
        if report(&ui, "Refreshing the server list", result).is_some() {
            ui.toast("Server list refreshed");
        }
        ui.ctx.reload_regions();
    });
}
