//! Sign-in dialog for entering PIA credentials on first run / "Switch account".
//!
//! Phase-1 MVP: only validates that fields are non-empty. Server-side
//! verification via the PIA API is deferred to a later milestone.

use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;

use crate::secrets::Credentials;

/// Show the modal sign-in dialog. Calls `on_success(creds)` exactly once
/// when the user confirms with non-empty fields. The dialog closes itself
/// on Sign in / Cancel.
pub fn show_login_dialog<F>(parent: &adw::ApplicationWindow, on_success: F)
where
    F: Fn(Credentials) + 'static,
{
    let dialog = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .default_width(400)
        .default_height(280)
        .resizable(false)
        .title("Sign in to PIA")
        .build();

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(false);
    header.set_show_start_title_buttons(false);
    toolbar.add_top_bar(&header);

    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 18);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.set_margin_start(18);
    content.set_margin_end(18);

    let intro = gtk4::Label::new(Some(
        "Enter your Private Internet Access account credentials.\n\
         They are stored locally in ~/.config/vex-vpn/credentials.toml (mode 0600).",
    ));
    intro.set_wrap(true);
    intro.set_justify(gtk4::Justification::Left);
    intro.set_xalign(0.0);
    intro.add_css_class("dim-label");
    content.append(&intro);

    let group = adw::PreferencesGroup::new();
    let username_row = adw::EntryRow::builder().title("Username").build();
    let password_row = adw::PasswordEntryRow::builder().title("Password").build();
    group.add(&username_row);
    group.add(&password_row);
    content.append(&group);

    let button_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    button_row.set_halign(gtk4::Align::End);
    button_row.set_margin_top(4);

    let cancel_btn = gtk4::Button::with_label("Cancel");
    let signin_btn = gtk4::Button::with_label("Sign in");
    signin_btn.add_css_class("suggested-action");

    button_row.append(&cancel_btn);
    button_row.append(&signin_btn);
    content.append(&button_row);

    toolbar.set_content(Some(&content));
    dialog.set_content(Some(&toolbar));

    {
        let dialog_c = dialog.clone();
        cancel_btn.connect_clicked(move |_| dialog_c.close());
    }

    {
        let dialog_c = dialog.clone();
        let username_row_c = username_row.clone();
        let password_row_c = password_row.clone();
        signin_btn.connect_clicked(move |_| {
            let username = username_row_c.text().to_string();
            let password = password_row_c.text().to_string();
            if username.trim().is_empty() || password.is_empty() {
                tracing::warn!("login: empty username or password");
                return;
            }
            on_success(Credentials { username, password });
            dialog_c.close();
        });
    }

    dialog.present();
}
