//! Region page: searchable list from `vexos-vpn regions --json`, with
//! "Automatic (fastest)" pinned at the top and the current choice marked.

use crate::dbus;
use crate::state::AppState;
use crate::ui::{region_display, Ui};
use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use vex_vpn::vexos::{is_valid_unit_arg, Region, AUTO_REGION};

pub struct RegionsPage {
    pub root: gtk4::Box,
    list: gtk4::ListBox,
    content: gtk4::Stack,
    empty: adw::StatusPage,
    /// (regions_version, region_setting) last rendered.
    rendered: Rc<RefCell<Option<(u64, String)>>>,
    busy: Rc<Cell<bool>>,
}

impl RegionsPage {
    pub fn new(ui: &Ui) -> Self {
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
        root.set_margin_top(6);
        root.set_margin_bottom(18);
        root.set_margin_start(18);
        root.set_margin_end(18);

        let search = gtk4::SearchEntry::builder()
            .placeholder_text("Search regions")
            .build();
        root.append(
            &adw::Clamp::builder()
                .maximum_size(560)
                .child(&search)
                .build(),
        );

        let list = gtk4::ListBox::new();
        list.set_selection_mode(gtk4::SelectionMode::None);
        list.add_css_class("boxed-list");
        {
            let search = search.clone();
            list.set_filter_func(move |row| {
                let needle = search.text().to_lowercase();
                if needle.is_empty() || row.widget_name() == AUTO_REGION {
                    return true;
                }
                row.downcast_ref::<adw::ActionRow>().is_some_and(|r| {
                    r.title().to_lowercase().contains(&needle)
                        || r.subtitle()
                            .is_some_and(|s| s.to_lowercase().contains(&needle))
                        || r.widget_name().contains(&needle)
                })
            });
        }
        {
            let list = list.clone();
            search.connect_search_changed(move |_| list.invalidate_filter());
        }

        let clamp = adw::Clamp::builder().maximum_size(560).child(&list).build();
        let scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vexpand(true)
            .child(&clamp)
            .build();

        let refresh_btn = gtk4::Button::with_label("Refresh server list");
        refresh_btn.add_css_class("pill");
        refresh_btn.add_css_class("suggested-action");
        refresh_btn.set_halign(gtk4::Align::Center);
        {
            let ui = ui.clone();
            refresh_btn.connect_clicked(move |_| crate::ui_login::refresh_servers(&ui));
        }
        let empty = adw::StatusPage::builder()
            .icon_name("network-server-symbolic")
            .title("No server list yet")
            .child(&refresh_btn)
            .vexpand(true)
            .build();

        let loading = gtk4::Spinner::builder()
            .spinning(true)
            .halign(gtk4::Align::Center)
            .valign(gtk4::Align::Center)
            .width_request(32)
            .height_request(32)
            .build();

        let content = gtk4::Stack::new();
        content.add_named(&scroll, Some("list"));
        content.add_named(&empty, Some("empty"));
        content.add_named(&loading, Some("loading"));
        content.set_visible_child_name("loading");
        root.append(&content);

        let page = Self {
            root,
            list,
            content,
            empty,
            rendered: Rc::default(),
            busy: Rc::new(Cell::new(false)),
        };
        {
            let ui = ui.clone();
            let busy = page.busy.clone();
            let list = page.list.clone();
            let rendered = page.rendered.clone();
            page.list.connect_row_activated(move |_, row| {
                let id = row.widget_name().to_string();
                let current = rendered
                    .borrow()
                    .as_ref()
                    .is_some_and(|(_, cur)| *cur == id);
                // Re-selecting the current region would only restart the tunnel.
                if current || busy.get() || !is_valid_unit_arg(&id) {
                    return;
                }
                busy.set(true);
                list.set_sensitive(false);
                let title = row
                    .downcast_ref::<adw::ActionRow>()
                    .map(|r| r.title().to_string())
                    .unwrap_or_else(|| id.clone());
                let busy = busy.clone();
                let list = list.clone();
                ui.run_unit(
                    async move { dbus::run_oneshot(&dbus::region_unit(&id)).await },
                    Some(format!("Region set to {}", title)),
                    move || {
                        busy.set(false);
                        list.set_sensitive(true);
                    },
                );
            });
        }
        page
    }

    pub fn update(&self, snap: &AppState) {
        let Some(status) = &snap.status else {
            return;
        };
        match &snap.regions {
            None => self.content.set_visible_child_name("loading"),
            Some(Err(err)) => {
                self.empty
                    .set_description(Some(&glib::markup_escape_text(err)));
                self.content.set_visible_child_name("empty");
            }
            Some(Ok(regions)) => {
                let key = (snap.regions_version, status.region_setting.clone());
                if self.rendered.borrow().as_ref() != Some(&key) {
                    self.render(regions, &status.region_setting, snap);
                    self.rendered.replace(Some(key));
                }
                self.content.set_visible_child_name("list");
            }
        }
    }

    fn render(&self, regions: &[Region], current: &str, snap: &AppState) {
        while let Some(child) = self.list.first_child() {
            self.list.remove(&child);
        }
        self.list.append(&region_row(
            AUTO_REGION,
            &region_display(snap, AUTO_REGION),
            "Lowest-latency region at connect time",
            None,
            current == AUTO_REGION,
        ));
        for r in regions {
            let latency = r.latency_s.map(|s| format!("{:.0} ms", s * 1000.0));
            self.list.append(&region_row(
                &r.id,
                &r.name,
                &r.country,
                latency.as_deref(),
                r.id == current,
            ));
        }
    }
}

fn region_row(
    id: &str,
    name: &str,
    subtitle: &str,
    latency: Option<&str>,
    selected: bool,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(name))
        .activatable(true)
        .build();
    if !subtitle.is_empty() {
        row.set_subtitle(&glib::markup_escape_text(subtitle));
    }
    row.set_widget_name(id);
    if let Some(ms) = latency {
        let lbl = gtk4::Label::new(Some(ms));
        lbl.add_css_class("dim-label");
        lbl.add_css_class("numeric");
        row.add_suffix(&lbl);
    }
    let check = gtk4::Image::from_icon_name("object-select-symbolic");
    check.add_css_class("accent");
    check.set_visible(selected);
    row.add_suffix(&check);
    row
}
