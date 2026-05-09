//! Sign-in dialog for entering PIA credentials.
//!
//! Validates credentials against the PIA token API before saving.
//! Shows a spinner during validation and error messages on failure.

use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::pia;
use crate::secrets::Credentials;
use crate::state::AppState;

/// Show the modal sign-in dialog. Validates credentials against PIA before
/// saving. On success, stores auth token and fetches the server list.
pub fn show_login_dialog(
    parent: &adw::ApplicationWindow,
    state: Arc<RwLock<AppState>>,
    pia_client: Arc<pia::PiaClient>,
) {
    let dialog = adw::Window::builder()
        .transient_for(parent)
        .modal(true)
        .default_width(400)
        .default_height(320)
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

    // Error label — hidden by default, shown on auth failure.
    let error_label = gtk4::Label::new(None);
    error_label.set_wrap(true);
    error_label.set_xalign(0.0);
    error_label.add_css_class("error");
    error_label.set_visible(false);
    content.append(&error_label);

    let button_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    button_row.set_halign(gtk4::Align::End);
    button_row.set_margin_top(4);

    let cancel_btn = gtk4::Button::with_label("Cancel");
    let signin_btn = gtk4::Button::with_label("Sign in");
    signin_btn.add_css_class("suggested-action");

    // Spinner shown during validation
    let spinner = gtk4::Spinner::new();
    spinner.set_visible(false);

    button_row.append(&spinner);
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
        let error_label_c = error_label.clone();
        let spinner_c = spinner.clone();
        let signin_btn_c = signin_btn.clone();

        signin_btn.connect_clicked(move |_| {
            let username = username_row_c.text().to_string();
            let password = password_row_c.text().to_string();
            if username.trim().is_empty() || password.is_empty() {
                error_label_c.set_label("Username and password are required.");
                error_label_c.set_visible(true);
                return;
            }

            // Show spinner, disable button
            spinner_c.set_visible(true);
            spinner_c.set_spinning(true);
            signin_btn_c.set_sensitive(false);
            error_label_c.set_visible(false);

            let client = pia_client.clone();
            let state = state.clone();
            let dialog = dialog_c.clone();
            let error_label = error_label_c.clone();
            let spinner = spinner_c.clone();
            let signin_btn = signin_btn_c.clone();

            glib::spawn_future_local(async move {
                match client.generate_token(&username, &password).await {
                    Ok(token) => {
                        // Save credentials locally
                        let creds = Credentials {
                            username: username.clone(),
                            password: password.clone(),
                        };
                        if let Err(e) = crate::secrets::save(&creds).await {
                            tracing::error!("save credentials: {}", e);
                        }

                        // Store token in state
                        state.write().await.auth_token = Some(token);

                        // Fetch server list in the background
                        match client.fetch_server_list().await {
                            Ok(server_list) => {
                                tracing::info!("Loaded {} PIA regions", server_list.regions.len());
                                state.write().await.regions = server_list.regions;
                            }
                            Err(e) => {
                                tracing::warn!("Failed to fetch server list: {}", e);
                            }
                        }

                        dialog.close();
                    }
                    Err(pia::PiaError::AuthFailed) => {
                        error_label.set_label("Invalid username or password.");
                        error_label.set_visible(true);
                        spinner.set_spinning(false);
                        spinner.set_visible(false);
                        signin_btn.set_sensitive(true);
                    }
                    Err(e) => {
                        error_label.set_label(&format!("Connection error: {}", e));
                        error_label.set_visible(true);
                        spinner.set_spinning(false);
                        spinner.set_visible(false);
                        signin_btn.set_sensitive(true);
                    }
                }
            });
        });
    }

    dialog.present();
}
