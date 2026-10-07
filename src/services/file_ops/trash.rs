//! Direct access to freedesktop.org trash directories, for when `trash:///`
//! isn't available (no GVfs, e.g. on many KDE systems) and to restore local
//! files without going through a daemon.
//!
//! A trash directory has `files/<name>` (the trashed item) and
//! `info/<name>.trashinfo`:
//!
//! ```text
//! [Trash Info]
//! Path=/home/lia/Documents/report%20final.pdf
//! DeletionDate=2026-10-08T14:02:11
//! ```
//!
//! `Path` is percent-encoded, and relative to the drive's top directory in
//! per-drive trashes (`$topdir/.Trash-$uid`, `$topdir/.Trash/$uid`).

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use gtk::{gio, glib};

/// A trashed item found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashedItem {
    /// The item, in `files/`.
    pub file: PathBuf,
    /// Its `.trashinfo` file, to delete once restored.
    pub info: PathBuf,
    /// Where it was deleted from.
    pub original: PathBuf,
    /// ISO 8601, local time.
    pub deletion_date: String,
}

/// Parses a `.trashinfo` file: (original path, deletion date). Relative
/// paths are resolved against `topdir`.
pub fn parse_trashinfo(contents: &str, topdir: Option<&Path>) -> Option<(PathBuf, String)> {
    let mut in_section = false;
    let mut path = None;
    let mut date = String::new();
    for line in contents.lines().map(str::trim) {
        if line.starts_with('[') {
            in_section = line == "[Trash Info]";
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(value) = line.strip_prefix("Path=") {
            path = Some(PathBuf::from(OsString::from_vec(percent_decode(value))));
        } else if let Some(value) = line.strip_prefix("DeletionDate=") {
            date = value.to_owned();
        }
    }
    let path = path?;
    let path = if path.is_absolute() {
        path
    } else {
        topdir?.join(path)
    };
    Some((path, date))
}

fn percent_decode(text: &str) -> Vec<u8> {
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(high << 4 | low);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// The user's home trash.
pub fn home_trash() -> PathBuf {
    glib::user_data_dir().join("Trash")
}

/// The trash directories that may hold an item deleted from `original`:
/// the home trash if it's on the home's device, otherwise the drive's.
pub fn trash_dirs_for(original: &Path) -> Vec<(PathBuf, Option<PathBuf>)> {
    let device = |path: &Path| {
        path.ancestors()
            .find_map(|p| std::fs::metadata(p).ok())
            .map(|m| m.dev())
    };
    let home = glib::home_dir();
    if device(original).is_some() && device(original) == device(&home) {
        return vec![(home_trash(), None)];
    }
    let Some(topdir) = mount_root(original) else {
        return vec![(home_trash(), None)];
    };
    // SAFETY: `getuid` can't fail and has no preconditions.
    let uid = unsafe { getuid() };
    vec![
        (topdir.join(format!(".Trash-{uid}")), Some(topdir.clone())),
        (topdir.join(".Trash").join(uid.to_string()), Some(topdir)),
        (home_trash(), None),
    ]
}

/// The top directory of the file system holding `path` (its mount point):
/// the highest ancestor on the same device.
fn mount_root(path: &Path) -> Option<PathBuf> {
    let (existing, meta) = path
        .ancestors()
        .find_map(|p| std::fs::metadata(p).ok().map(|m| (p, m)))?;
    let mut root = existing;
    for parent in existing.ancestors().skip(1) {
        match std::fs::metadata(parent) {
            Ok(m) if m.dev() == meta.dev() => root = parent,
            _ => break,
        }
    }
    Some(root.to_path_buf())
}

unsafe extern "C" {
    fn getuid() -> u32;
}

/// Finds the most recently trashed item that came from `original`.
pub fn find_trashed(original: &Path) -> Option<TrashedItem> {
    let mut best: Option<TrashedItem> = None;
    for (dir, topdir) in trash_dirs_for(original) {
        let Ok(entries) = std::fs::read_dir(dir.join("info")) else {
            continue;
        };
        let name = original.file_name()?;
        for entry in entries.flatten() {
            let info = entry.path();
            let Some(stem) = info
                .file_name()
                .and_then(|n| n.as_bytes().strip_suffix(b".trashinfo"))
            else {
                continue;
            };
            // Trashed names are the original name, possibly with a suffix
            // ("name.2.ext"); check the cheap prefix before reading.
            let base_stem = name
                .as_bytes()
                .split(|&b| b == b'.')
                .next()
                .unwrap_or_default();
            if !stem.starts_with(base_stem) {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&info) else {
                continue;
            };
            let Some((path, date)) = parse_trashinfo(&contents, topdir.as_deref()) else {
                continue;
            };
            if path != original || best.as_ref().is_some_and(|b| b.deletion_date > date) {
                continue;
            }
            let file = dir.join("files").join(std::ffi::OsStr::from_bytes(stem));
            if std::fs::symlink_metadata(&file).is_ok() {
                best = Some(TrashedItem {
                    file,
                    info,
                    original: path,
                    deletion_date: date,
                });
            }
        }
    }
    best
}

/// Everything in the home trash (`files/` and `info/` entries), for
/// emptying it without GVfs.
pub fn home_trash_contents() -> Vec<gio::File> {
    let trash = home_trash();
    ["files", "info", "expunged"]
        .iter()
        .filter_map(|sub| std::fs::read_dir(trash.join(sub)).ok())
        .flat_map(|entries| entries.flatten().map(|e| gio::File::for_path(e.path())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_trashinfo() {
        let info = "[Trash Info]\nPath=/home/a/My%20Docs/r%C3%A9sum%C3%A9.pdf\nDeletionDate=2026-10-08T14:02:11\n";
        assert_eq!(
            parse_trashinfo(info, None),
            Some((
                PathBuf::from("/home/a/My Docs/résumé.pdf"),
                "2026-10-08T14:02:11".into()
            ))
        );
        // Per-drive trashes store paths relative to the drive.
        let info = "[Trash Info]\nPath=photos/a.jpg\nDeletionDate=2026-01-01T00:00:00";
        assert_eq!(
            parse_trashinfo(info, Some(Path::new("/media/usb")))
                .unwrap()
                .0,
            PathBuf::from("/media/usb/photos/a.jpg")
        );
        assert_eq!(parse_trashinfo("[Other]\nPath=/x", None), None);
        assert_eq!(percent_decode("100%"), b"100%");
        assert_eq!(percent_decode("%é%4"), "%é%4".as_bytes());
    }
}
