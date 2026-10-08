//! Whether the app runs inside a Flatpak sandbox, where the host's
//! applications and terminals can't be listed or started directly.

use std::sync::OnceLock;

pub fn is_flatpak() -> bool {
    static FLATPAK: OnceLock<bool> = OnceLock::new();
    *FLATPAK.get_or_init(|| std::path::Path::new("/.flatpak-info").exists())
}
