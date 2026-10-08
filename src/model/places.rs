//! Places shown in the sidebar: XDG user folders, the GTK bookmarks file
//! (`~/.config/gtk-3.0/bookmarks`, shared with Nautilus and the GTK file
//! chooser), the "This PC" page and icons for well-known locations.
//!
//! The bookmarks format is one entry per line: `<uri>[ <label>]`.

use std::path::PathBuf;

use gtk::{gio, glib, prelude::*};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    pub uri: String,
    pub label: Option<String>,
}

impl Bookmark {
    pub fn new(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            label: None,
        }
    }
}

pub fn parse_bookmarks(contents: &str) -> Vec<Bookmark> {
    contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| match line.split_once(' ') {
            Some((uri, label)) if !label.trim().is_empty() => Bookmark {
                uri: uri.to_owned(),
                label: Some(label.trim().to_owned()),
            },
            Some((uri, _)) => Bookmark::new(uri),
            None => Bookmark::new(line),
        })
        .collect()
}

pub fn serialize_bookmarks(bookmarks: &[Bookmark]) -> String {
    bookmarks
        .iter()
        .map(|b| match &b.label {
            Some(label) => format!("{} {label}\n", b.uri),
            None => format!("{}\n", b.uri),
        })
        .collect()
}

/// Appends `uris` that aren't bookmarked yet. Returns whether anything changed.
pub fn add_bookmarks(bookmarks: &mut Vec<Bookmark>, uris: &[String]) -> bool {
    let before = bookmarks.len();
    for uri in uris {
        if !bookmarks.iter().any(|b| &b.uri == uri) {
            bookmarks.push(Bookmark::new(uri.clone()));
        }
    }
    bookmarks.len() != before
}

/// Removes every entry for `uri`. Returns whether anything changed.
pub fn remove_bookmark(bookmarks: &mut Vec<Bookmark>, uri: &str) -> bool {
    let before = bookmarks.len();
    bookmarks.retain(|b| b.uri != uri);
    bookmarks.len() != before
}

/// The "This PC" page: drives and network locations.
pub const THIS_PC_URI: &str = "computer:///";

pub fn is_this_pc(file: &gio::File) -> bool {
    file.uri_scheme().as_deref() == Some("computer")
}

pub fn bookmarks_file() -> gio::File {
    gio::File::for_path(
        crate::util::sandbox::host_config_dir()
            .join("gtk-3.0")
            .join("bookmarks"),
    )
}

/// Reads the bookmarks file; a missing file means no bookmarks.
pub async fn load_bookmarks() -> Vec<Bookmark> {
    match bookmarks_file().load_contents_future().await {
        Ok((bytes, _)) => parse_bookmarks(&String::from_utf8_lossy(&bytes)),
        Err(err) => {
            if !err.matches(gio::IOErrorEnum::NotFound) {
                tracing::warn!("cannot read bookmarks: {err}");
            }
            Vec::new()
        }
    }
}

pub async fn save_bookmarks(bookmarks: &[Bookmark]) -> Result<(), glib::Error> {
    let file = bookmarks_file();
    if let Some(parent) = file.parent()
        && let Err(err) = parent.make_directory_with_parents(gio::Cancellable::NONE)
        && !err.matches(gio::IOErrorEnum::Exists)
    {
        return Err(err);
    }
    file.replace_contents_future(
        serialize_bookmarks(bookmarks).into_bytes(),
        None,
        false,
        gio::FileCreateFlags::NONE,
    )
    .await
    .map(|_| ())
    .map_err(|(_, err)| err)
}

/// Re-reads the bookmarks file, applies `edit` and writes it back if it
/// changed. Reading first keeps edits made by other apps meanwhile.
pub async fn edit_bookmarks(edit: impl FnOnce(&mut Vec<Bookmark>) -> bool) {
    let mut bookmarks = load_bookmarks().await;
    if edit(&mut bookmarks)
        && let Err(err) = save_bookmarks(&bookmarks).await
    {
        tracing::warn!("cannot save bookmarks: {err}");
    }
}

/// The XDG user folders that exist and aren't the home folder itself
/// (unset folders default to `$HOME`), with their symbolic icons.
pub fn user_folders() -> Vec<(PathBuf, &'static str)> {
    let home = glib::home_dir();
    USER_FOLDERS
        .iter()
        .filter_map(|&(dir, icon)| {
            glib::user_special_dir(dir)
                .filter(|path| *path != home && path.is_dir())
                .map(|path| (path, icon))
        })
        .collect()
}

const USER_FOLDERS: [(glib::UserDirectory, &str); 6] = [
    (glib::UserDirectory::Desktop, "user-desktop-symbolic"),
    (glib::UserDirectory::Documents, "folder-documents-symbolic"),
    (glib::UserDirectory::Downloads, "folder-download-symbolic"),
    (glib::UserDirectory::Music, "folder-music-symbolic"),
    (glib::UserDirectory::Pictures, "folder-pictures-symbolic"),
    (glib::UserDirectory::Videos, "folder-videos-symbolic"),
];

/// A symbolic icon name for a location (used by tabs and the sidebar).
pub fn icon_name_for(file: &gio::File) -> &'static str {
    match file.uri_scheme().as_deref() {
        Some("trash") => return "user-trash-symbolic",
        Some("recent") => return "document-open-recent-symbolic",
        Some("computer") => return "computer-symbolic",
        Some("file") => {}
        Some(_) => return "folder-remote-symbolic",
        None => {}
    }
    let Some(path) = file.path() else {
        return "folder-symbolic";
    };
    if path == glib::home_dir() {
        return "user-home-symbolic";
    }
    if path.parent().is_none() {
        return "drive-harddisk-symbolic";
    }
    USER_FOLDERS
        .iter()
        .find(|&&(dir, _)| glib::user_special_dir(dir).as_deref() == Some(path.as_path()))
        .map_or("folder-symbolic", |&(_, icon)| icon)
}
