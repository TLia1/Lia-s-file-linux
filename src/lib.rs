//! Lia Files — a clean, modern file manager for Linux.

pub mod application;
pub mod config;
pub mod dialogs;
pub mod model;
pub mod services;
pub mod util;
pub mod widgets;
pub mod window;

use gettextrs::LocaleCategory;
use gtk::gio;

/// Registers the compiled GResource bundle (UI files, stylesheet).
pub fn register_resources() {
    gio::resources_register_include!("lia-files.gresource")
        .expect("failed to register the application resources");
}

/// Sets up gettext for translations.
pub fn init_i18n() {
    // SAFETY: called once at startup, before any other thread exists.
    unsafe {
        gettextrs::setlocale(LocaleCategory::LcAll, "");
    }
    if let Err(err) = gettextrs::bindtextdomain(config::GETTEXT_PACKAGE, config::LOCALEDIR) {
        tracing::warn!("unable to bind the text domain: {err}");
    }
    let _ = gettextrs::bind_textdomain_codeset(config::GETTEXT_PACKAGE, "UTF-8");
    let _ = gettextrs::textdomain(config::GETTEXT_PACKAGE);
}
