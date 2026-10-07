use std::cell::OnceCell;

use gtk::gio;

use crate::config::{APP_ID, DEV_SCHEMA_DIR};

thread_local! {
    static SETTINGS: OnceCell<gio::Settings> = const { OnceCell::new() };
}

/// The application's `GSettings`.
///
/// Uses the installed schema when available; otherwise (or always in debug
/// builds) falls back to the schema compiled into the build directory, so the
/// app can be run straight from `cargo run`.
pub fn settings() -> gio::Settings {
    SETTINGS.with(|cell| cell.get_or_init(load).clone())
}

fn load() -> gio::Settings {
    let default_source = gio::SettingsSchemaSource::default();
    let dev_source = || {
        gio::SettingsSchemaSource::from_directory(DEV_SCHEMA_DIR, default_source.as_ref(), false)
            .ok()
            .and_then(|source| source.lookup(APP_ID, false))
    };
    let installed = || default_source.as_ref().and_then(|s| s.lookup(APP_ID, true));

    let schema = if cfg!(debug_assertions) {
        dev_source().or_else(installed)
    } else {
        installed().or_else(dev_source)
    }
    .unwrap_or_else(|| panic!("GSettings schema {APP_ID} not found"));

    gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None)
}
