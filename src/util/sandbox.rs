//! Whether the app runs inside a Flatpak sandbox, where the host's
//! applications and terminals can't be listed or started directly.

use std::path::PathBuf;
use std::sync::OnceLock;

use gtk::glib;

pub fn is_flatpak() -> bool {
    static FLATPAK: OnceLock<bool> = OnceLock::new();
    *FLATPAK.get_or_init(|| std::path::Path::new("/.flatpak-info").exists())
}

/// Flatpak points the XDG base directories into `~/.var/app`, but data
/// shared with other apps (GTK bookmarks, the Trash, the thumbnail cache)
/// lives in the host's. Flatpak passes those as `HOST_XDG_*_HOME`.
fn host_dir(variable: &str, default: &str, own: PathBuf) -> PathBuf {
    if !is_flatpak() {
        return own;
    }
    std::env::var_os(variable)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| glib::home_dir().join(default))
}

/// The host's `$XDG_CONFIG_HOME`.
pub fn host_config_dir() -> PathBuf {
    host_dir("HOST_XDG_CONFIG_HOME", ".config", glib::user_config_dir())
}

/// The host's `$XDG_DATA_HOME`.
pub fn host_data_dir() -> PathBuf {
    host_dir("HOST_XDG_DATA_HOME", ".local/share", glib::user_data_dir())
}

/// The host's `$XDG_CACHE_HOME`.
pub fn host_cache_dir() -> PathBuf {
    host_dir("HOST_XDG_CACHE_HOME", ".cache", glib::user_cache_dir())
}
