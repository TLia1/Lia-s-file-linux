//! The Preferences dialog: appearance, behaviour, startup, views,
//! thumbnails and file operations. Every row is bound to its GSettings
//! key, so changes apply right away (and from anywhere else, like the
//! Ctrl+H toggle, show up here).

use adw::prelude::*;
use gettextrs::gettext;
use std::cell::RefCell;

use gtk::{gio, glib, glib::clone};

use crate::util::settings;
use crate::widgets::file_view::ICON_SIZES;

pub fn present(parent: Option<&impl IsA<gtk::Widget>>) {
    let settings = settings();

    // ── General ────────────────────────────────────────────────────────────
    let appearance = adw::PreferencesGroup::builder()
        .title(gettext("Appearance"))
        .build();
    appearance.add(&choice_row(
        &settings,
        "color-scheme",
        &gettext("Style"),
        None,
        &[
            ("system", gettext("Follow System")),
            ("light", gettext("Light")),
            ("dark", gettext("Dark")),
        ],
    ));

    appearance.add(&choice_row(
        &settings,
        "corners",
        &gettext("Corners"),
        None,
        &[("rounded", gettext("Rounded")), ("sharp", gettext("Sharp"))],
    ));
    appearance.add(&choice_row(
        &settings,
        "density",
        &gettext("Density"),
        None,
        &[
            ("compact", gettext("Compact")),
            ("normal", gettext("Normal")),
            ("comfortable", gettext("Comfortable")),
        ],
    ));
    appearance.add(&choice_row(
        &settings,
        "selection-style",
        &gettext("Selection"),
        None,
        &[
            ("marker", gettext("Marker")),
            ("fill", gettext("Fill")),
            ("outline", gettext("Outline")),
        ],
    ));
    let opacity = adw::SpinRow::with_range(0.6, 1.0, 0.02);
    opacity.set_title(&gettext("Window Opacity"));
    opacity.set_digits(2);
    settings.bind("window-opacity", &opacity, "value").build();
    appearance.add(&opacity);
    let accent = adw::EntryRow::builder()
        .title(gettext("Accent Colour (#rrggbb, empty for system)"))
        .build();
    settings
        .bind("accent-override", &accent, "text")
        .flags(gio::SettingsBindFlags::GET)
        .build();
    accent.connect_changed(clone!(
        #[strong]
        settings,
        move |row| {
            let value = row.text();
            if value.is_empty() || crate::util::appearance::valid_accent(&value) {
                row.remove_css_class("error");
                if settings.string("accent-override") != value {
                    let _ = settings.set_string("accent-override", &value);
                }
            } else {
                row.add_css_class("error");
            }
        }
    ));
    appearance.add(&accent);
    for (key, title) in [
        ("text-scheme-dark", "Dark Preview Scheme"),
        ("text-scheme-light", "Light Preview Scheme"),
    ] {
        let row = adw::EntryRow::builder().title(gettext(title)).build();
        settings.bind(key, &row, "text").build();
        appearance.add(&row);
    }

    let behaviour = adw::PreferencesGroup::builder()
        .title(gettext("Behavior"))
        .build();
    behaviour.add(&switch_row(
        &settings,
        "single-click",
        &gettext("Open Items With a Single Click"),
        None,
    ));
    behaviour.add(&switch_row(
        &settings,
        "confirm-trash",
        &gettext("Ask Before Moving to the Trash"),
        Some(&gettext("Items in the Trash can always be restored")),
    ));

    let startup = adw::PreferencesGroup::builder()
        .title(gettext("Startup"))
        .build();
    startup.add(&switch_row(
        &settings,
        "restore-session",
        &gettext("Reopen Tabs From Last Time"),
        None,
    ));
    let start = choice_row(
        &settings,
        "start-location",
        &gettext("Otherwise Open"),
        None,
        &[("home", gettext("Home")), ("recent", gettext("Recent"))],
    );
    settings
        .bind("restore-session", &start, "sensitive")
        .flags(gio::SettingsBindFlags::GET | gio::SettingsBindFlags::INVERT_BOOLEAN)
        .build();
    startup.add(&start);

    let general = adw::PreferencesPage::builder()
        .title(gettext("General"))
        .icon_name("preferences-system-symbolic")
        .build();
    general.add(&appearance);
    general.add(&behaviour);
    general.add(&startup);

    // ── Views ──────────────────────────────────────────────────────────────
    let views = adw::PreferencesGroup::builder()
        .title(gettext("Default View"))
        .description(gettext(
            "Changes made in a folder (view, sorting, grouping) are remembered for that folder",
        ))
        .build();
    views.add(&choice_row(
        &settings,
        "default-view",
        &gettext("View"),
        None,
        &[("details", gettext("Details")), ("grid", gettext("Grid"))],
    ));
    views.add(&icon_size_row(&settings));
    views.add(&switch_row(
        &settings,
        "sort-folders-first",
        &gettext("Sort Folders Before Files"),
        None,
    ));
    views.add(&switch_row(
        &settings,
        "show-hidden",
        &gettext("Show Hidden Files"),
        None,
    ));

    let thumbnails = adw::PreferencesGroup::builder()
        .title(gettext("Thumbnails"))
        .build();
    thumbnails.add(&choice_row(
        &settings,
        "thumbnail-policy",
        &gettext("Show Thumbnails"),
        Some(&gettext("Thumbnails already made are always used")),
        &[
            ("always", gettext("For All Files")),
            ("local-only", gettext("For Files on This Computer")),
            ("never", gettext("Never")),
        ],
    ));
    let max_size = adw::SpinRow::with_range(1.0, 4096.0, 16.0);
    max_size.set_title(&gettext("Largest File (MB)"));
    max_size.set_subtitle(&gettext("Larger files get no thumbnail"));
    max_size.set_value(settings.uint("thumbnail-max-size") as f64);
    max_size.connect_value_notify(clone!(
        #[strong]
        settings,
        move |row| {
            let _ = settings.set_uint("thumbnail-max-size", row.value() as u32);
        }
    ));
    thumbnails.add(&max_size);

    let views_page = adw::PreferencesPage::builder()
        .title(gettext("Views"))
        .icon_name("view-grid-symbolic")
        .build();
    views_page.add(&views);
    views_page.add(&thumbnails);

    // ── File operations ────────────────────────────────────────────────────
    let operations = adw::PreferencesGroup::builder()
        .title(gettext("File Operations"))
        .build();
    let jobs = adw::SpinRow::with_range(1.0, 8.0, 1.0);
    jobs.set_title(&gettext("Operations at Once per Drive"));
    jobs.set_subtitle(&gettext(
        "One at a time is fastest on most disks. Different drives always work in parallel.",
    ));
    jobs.set_value(settings.int("jobs-per-device") as f64);
    jobs.connect_value_notify(clone!(
        #[strong]
        settings,
        move |row| {
            let _ = settings.set_int("jobs-per-device", row.value() as i32);
        }
    ));
    operations.add(&jobs);
    views_page.add(&operations);

    let dialog = adw::PreferencesDialog::builder()
        .search_enabled(true)
        .build();
    dialog.add(&general);
    dialog.add(&views_page);
    dialog.present(parent);
}

fn switch_row(
    settings: &gio::Settings,
    key: &str,
    title: &str,
    subtitle: Option<&str>,
) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    settings.bind(key, &row, "active").build();
    row
}

/// A combo row for a string or enum key; `choices` are (nick, label).
fn choice_row(
    settings: &gio::Settings,
    key: &'static str,
    title: &str,
    subtitle: Option<&str>,
    choices: &[(&'static str, String)],
) -> adw::ComboRow {
    let labels: Vec<&str> = choices.iter().map(|(_, label)| label.as_str()).collect();
    let row = adw::ComboRow::builder()
        .title(title)
        .model(&gtk::StringList::new(&labels))
        .build();
    if let Some(subtitle) = subtitle {
        row.set_subtitle(subtitle);
    }
    let nicks: Vec<&'static str> = choices.iter().map(|(nick, _)| *nick).collect();
    let sync = {
        let nicks = nicks.clone();
        move |row: &adw::ComboRow, settings: &gio::Settings| {
            let current = settings.string(key);
            if let Some(index) = nicks.iter().position(|n| *n == current.as_str())
                && row.selected() != index as u32
            {
                row.set_selected(index as u32);
            }
        }
    };
    sync(&row, settings);
    row.connect_selected_notify(clone!(
        #[strong]
        settings,
        move |row| {
            if let Some(nick) = nicks.get(row.selected() as usize)
                && settings.string(key).as_str() != *nick
            {
                let _ = settings.set_string(key, nick);
            }
        }
    ));
    let handler = settings.connect_changed(
        Some(key),
        clone!(
            #[weak]
            row,
            move |settings, _| sync(&row, settings)
        ),
    );
    disconnect_on_destroy(&row, settings, handler);
    row
}

/// The grid icon size, as Small / Medium / Large.
fn icon_size_row(settings: &gio::Settings) -> adw::ComboRow {
    let labels = [gettext("Small"), gettext("Medium"), gettext("Large")];
    let row = adw::ComboRow::builder()
        .title(gettext("Grid Icon Size"))
        .model(&gtk::StringList::new(
            &labels.iter().map(String::as_str).collect::<Vec<_>>(),
        ))
        .build();
    let sync = |row: &adw::ComboRow, settings: &gio::Settings| {
        let size = settings.int("icon-size");
        let index = ICON_SIZES
            .iter()
            .position(|&s| s >= size)
            .unwrap_or(ICON_SIZES.len() - 1);
        if row.selected() != index as u32 {
            row.set_selected(index as u32);
        }
    };
    sync(&row, settings);
    row.connect_selected_notify(clone!(
        #[strong]
        settings,
        move |row| {
            if let Some(&size) = ICON_SIZES.get(row.selected() as usize)
                && settings.int("icon-size") != size
            {
                let _ = settings.set_int("icon-size", size);
            }
        }
    ));
    let handler = settings.connect_changed(
        Some("icon-size"),
        clone!(
            #[weak]
            row,
            move |settings, _| sync(&row, settings)
        ),
    );
    disconnect_on_destroy(&row, settings, handler);
    row
}

/// Rows listen to their key until they're destroyed.
fn disconnect_on_destroy(
    row: &impl IsA<gtk::Widget>,
    settings: &gio::Settings,
    handler: glib::SignalHandlerId,
) {
    let handler = RefCell::new(Some(handler));
    row.connect_destroy(clone!(
        #[strong]
        settings,
        move |_| {
            if let Some(handler) = handler.take() {
                settings.disconnect(handler);
            }
        }
    ));
}
