//! Archives: which files can be extracted, the formats archives can be
//! created in, and the naming rules of both. The work itself is done by
//! file-operation jobs (`Operation::Extract` / `Operation::Compress`, see
//! `file_ops::engine`), through libarchive ([`libarchive`]).

mod ffi;
pub mod libarchive;

use std::path::{Component, Path, PathBuf};

use gettextrs::gettext;

use crate::services::file_ops::naming;

/// Formats archives can be created in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArchiveFormat {
    #[default]
    Zip,
    TarGz,
    TarXz,
    SevenZip,
}

impl ArchiveFormat {
    pub const ALL: [Self; 4] = [Self::Zip, Self::TarGz, Self::TarXz, Self::SevenZip];

    /// The GSettings nick.
    pub fn nick(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::TarGz => "tar-gz",
            Self::TarXz => "tar-xz",
            Self::SevenZip => "7z",
        }
    }

    pub fn from_nick(nick: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|f| f.nick() == nick)
            .unwrap_or_default()
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Zip => ".zip",
            Self::TarGz => ".tar.gz",
            Self::TarXz => ".tar.xz",
            Self::SevenZip => ".7z",
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Zip => gettext(".zip — works everywhere"),
            Self::TarGz => gettext(".tar.gz — common on Linux"),
            Self::TarXz => gettext(".tar.xz — smaller, slower"),
            Self::SevenZip => gettext(".7z — smallest"),
        }
    }
}

/// Archive types that can be extracted.
const ARCHIVE_TYPES: &[&str] = &[
    "application/zip",
    "application/x-zip-compressed",
    "application/x-7z-compressed",
    "application/x-tar",
    "application/x-compressed-tar",
    "application/x-bzip-compressed-tar",
    "application/x-bzip2-compressed-tar",
    "application/x-xz-compressed-tar",
    "application/x-zstd-compressed-tar",
    "application/x-lzma-compressed-tar",
    "application/x-lz4-compressed-tar",
    "application/x-lzip-compressed-tar",
    "application/x-tarz",
    "application/vnd.rar",
    "application/x-rar",
    "application/x-rar-compressed",
    "application/x-cpio",
    "application/x-xar",
    "application/x-lha",
    "application/vnd.ms-cab-compressed",
    "application/x-cd-image",
    "application/x-iso9660-image",
];

/// Single compressed files (not archives of several files).
const COMPRESSED_FILE_TYPES: &[&str] = &[
    "application/gzip",
    "application/x-gzip",
    "application/x-bzip",
    "application/x-bzip2",
    "application/x-xz",
    "application/zstd",
    "application/x-zstd",
    "application/x-lzma",
    "application/x-lz4",
    "application/x-lzip",
    "application/x-compress",
];

/// Whether files of this type can be extracted. Exact types only: office
/// documents and Java archives are zip files too, but aren't "archives" to
/// the user.
pub fn is_extractable(content_type: &str) -> bool {
    ARCHIVE_TYPES.contains(&content_type) || is_compressed_file(content_type)
}

/// A single compressed file, extracted as one file (see `Reader::open`).
pub fn is_compressed_file(content_type: &str) -> bool {
    COMPRESSED_FILE_TYPES.contains(&content_type)
}

/// The name of an archive without its extension: "photos.tar.gz" →
/// "photos", "notes.txt.gz" → "notes.txt".
pub fn archive_stem(name: &str) -> String {
    let (stem, extension) = naming::split_extension(name, false);
    if extension.eq_ignore_ascii_case(".tgz") || extension.eq_ignore_ascii_case(".tbz2") {
        return stem.to_owned();
    }
    if stem.is_empty() {
        name.to_owned()
    } else {
        stem.to_owned()
    }
}

/// The name suggested for a new archive of `names` (without extension):
/// the item's name for one item, "Archive" for several.
pub fn suggested_name(names: &[(String, bool)]) -> String {
    match names {
        [(name, is_dir)] => naming::split_extension(name, *is_dir).0.to_owned(),
        _ => gettext("Archive"),
    }
}

/// Makes an entry path from an archive safe to extract: relative, without
/// `..`. Leading `/` and `./` are dropped, like `bsdtar` does. `None` for
/// paths that would escape (or are empty).
pub fn sanitize_entry_path(path: &[u8]) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let path = Path::new(std::ffi::OsStr::from_bytes(path));
    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir | Component::Prefix(_) => return None,
        }
    }
    (!clean.as_os_str().is_empty()).then_some(clean)
}
