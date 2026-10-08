//! The search filter chips: Kind, Date modified and Size.
//!
//! Each criterion is a small enum with a stable nick (the state of its
//! `search.*` action) and a translated label. Matching is plain Rust on the
//! values a `FileItem` exposes, so it's testable without a display.

use gettextrs::gettext;
use gtk::gio;

const DAY: i64 = 24 * 60 * 60;
const KB: u64 = 1000;
const MB: u64 = 1000 * KB;
const GB: u64 = 1000 * MB;

/// The kind of file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Any,
    Folder,
    Document,
    Image,
    Video,
    Audio,
    Archive,
}

impl Kind {
    pub const ALL: [Self; 7] = [
        Self::Any,
        Self::Folder,
        Self::Document,
        Self::Image,
        Self::Video,
        Self::Audio,
        Self::Archive,
    ];

    pub fn nick(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Folder => "folder",
            Self::Document => "document",
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Archive => "archive",
        }
    }

    pub fn from_nick(nick: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|k| k.nick() == nick)
            .unwrap_or_default()
    }

    pub fn label(self) -> String {
        match self {
            Self::Any => gettext("Any Kind"),
            Self::Folder => gettext("Folders"),
            Self::Document => gettext("Documents"),
            Self::Image => gettext("Pictures"),
            Self::Video => gettext("Videos"),
            Self::Audio => gettext("Music"),
            Self::Archive => gettext("Archives"),
        }
    }

    /// Whether a file of `content_type` (a MIME type) matches.
    pub fn matches(self, content_type: &str, is_dir: bool) -> bool {
        match self {
            Self::Any => true,
            Self::Folder => is_dir,
            _ if is_dir => false,
            Self::Image => content_type.starts_with("image/"),
            Self::Video => content_type.starts_with("video/"),
            Self::Audio => content_type.starts_with("audio/"),
            Self::Archive => is_archive(content_type),
            Self::Document => is_document(content_type),
        }
    }
}

fn is_archive(content_type: &str) -> bool {
    const ARCHIVES: [&str; 16] = [
        "application/zip",
        "application/x-tar",
        "application/x-7z-compressed",
        "application/vnd.rar",
        "application/x-rar",
        "application/x-rar-compressed",
        "application/gzip",
        "application/x-gzip",
        "application/x-bzip",
        "application/x-bzip2",
        "application/x-xz",
        "application/zstd",
        "application/x-zstd",
        "application/x-lzma",
        "application/x-cpio",
        "application/java-archive",
    ];
    ARCHIVES.contains(&content_type) || content_type.ends_with("-compressed-tar")
}

fn is_document(content_type: &str) -> bool {
    const DOCUMENTS: [&str; 6] = [
        "application/pdf",
        "application/rtf",
        "application/msword",
        "application/epub+zip",
        "application/x-abiword",
        "application/vnd.apple.pages",
    ];
    const DOCUMENT_PREFIXES: [&str; 3] = [
        "application/vnd.oasis.opendocument.",
        "application/vnd.openxmlformats-officedocument.",
        "application/vnd.ms-",
    ];
    DOCUMENTS.contains(&content_type)
        || DOCUMENT_PREFIXES.iter().any(|p| content_type.starts_with(p))
        // Text and everything derived from it (source code, JSON, XML…).
        || gio::content_type_is_a(content_type, "text/plain")
}

/// How recently the file was modified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DateRange {
    #[default]
    Any,
    Today,
    Week,
    Month,
    Year,
}

impl DateRange {
    pub const ALL: [Self; 5] = [Self::Any, Self::Today, Self::Week, Self::Month, Self::Year];

    pub fn nick(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Today => "today",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }

    pub fn from_nick(nick: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|d| d.nick() == nick)
            .unwrap_or_default()
    }

    pub fn label(self) -> String {
        match self {
            Self::Any => gettext("Any Date"),
            Self::Today => gettext("Today"),
            Self::Week => gettext("Last 7 Days"),
            Self::Month => gettext("Last 30 Days"),
            Self::Year => gettext("Last Year"),
        }
    }

    /// The earliest modification time (UNIX seconds) that matches, given
    /// the current time and the start of the local day.
    pub fn earliest(self, now: i64, today_start: i64) -> Option<i64> {
        match self {
            Self::Any => None,
            Self::Today => Some(today_start),
            Self::Week => Some(now - 7 * DAY),
            Self::Month => Some(now - 30 * DAY),
            Self::Year => Some(now - 365 * DAY),
        }
    }
}

/// How big the file is. Folders never match a size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeRange {
    #[default]
    Any,
    Empty,
    Tiny,
    Small,
    Medium,
    Large,
    Huge,
}

impl SizeRange {
    pub const ALL: [Self; 7] = [
        Self::Any,
        Self::Empty,
        Self::Tiny,
        Self::Small,
        Self::Medium,
        Self::Large,
        Self::Huge,
    ];

    pub fn nick(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Empty => "empty",
            Self::Tiny => "tiny",
            Self::Small => "small",
            Self::Medium => "medium",
            Self::Large => "large",
            Self::Huge => "huge",
        }
    }

    pub fn from_nick(nick: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|s| s.nick() == nick)
            .unwrap_or_default()
    }

    pub fn label(self) -> String {
        match self {
            Self::Any => gettext("Any Size"),
            Self::Empty => gettext("Empty (0 KB)"),
            Self::Tiny => gettext("Tiny (under 16 KB)"),
            Self::Small => gettext("Small (16 KB – 1 MB)"),
            Self::Medium => gettext("Medium (1 – 128 MB)"),
            Self::Large => gettext("Large (128 MB – 1 GB)"),
            Self::Huge => gettext("Huge (over 1 GB)"),
        }
    }

    /// A short label for the chip once chosen.
    pub fn short_label(self) -> String {
        match self {
            Self::Any => gettext("Size"),
            Self::Empty => gettext("Empty"),
            Self::Tiny => gettext("Tiny"),
            Self::Small => gettext("Small"),
            Self::Medium => gettext("Medium"),
            Self::Large => gettext("Large"),
            Self::Huge => gettext("Huge"),
        }
    }

    pub fn matches(self, size: u64, is_dir: bool) -> bool {
        let (min, max) = match self {
            Self::Any => return true,
            _ if is_dir => return false,
            Self::Empty => (0, 0),
            Self::Tiny => (1, 16 * KB - 1),
            Self::Small => (16 * KB, MB - 1),
            Self::Medium => (MB, 128 * MB - 1),
            Self::Large => (128 * MB, GB - 1),
            Self::Huge => (GB, u64::MAX),
        };
        (min..=max).contains(&size)
    }
}

/// The chosen value of every chip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Filters {
    pub kind: Kind,
    pub date: DateRange,
    pub size: SizeRange,
}

impl Filters {
    pub fn is_active(&self) -> bool {
        *self != Self::default()
    }

    /// `earliest` is [`DateRange::earliest`], computed once per filter
    /// change rather than per item.
    pub fn matches(
        &self,
        content_type: &str,
        is_dir: bool,
        size: u64,
        modified: i64,
        earliest: Option<i64>,
    ) -> bool {
        self.kind.matches(content_type, is_dir)
            && self.size.matches(size, is_dir)
            && earliest.is_none_or(|earliest| modified >= earliest)
    }
}
