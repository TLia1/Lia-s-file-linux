//! Build-time configuration. Meson overrides these through environment
//! variables when it invokes Cargo; a plain `cargo build` uses the defaults.

pub const APP_ID: &str = "io.github.lia.Files";
pub const APP_NAME: &str = "Lia Files";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const GETTEXT_PACKAGE: &str = "lia-files";
pub const RESOURCE_BASE: &str = "/io/github/lia/Files";

pub const LOCALEDIR: &str = match option_env!("LIA_LOCALEDIR") {
    Some(dir) => dir,
    None => "/usr/share/locale",
};

/// Schemas compiled by `build.rs`, used when the app is run uninstalled.
pub(crate) const DEV_SCHEMA_DIR: &str = concat!(env!("OUT_DIR"), "/schemas");
