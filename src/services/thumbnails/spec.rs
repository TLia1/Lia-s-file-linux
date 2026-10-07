//! The freedesktop thumbnail spec: where thumbnails live, when one is
//! valid, and how to write one.
//! <https://specifications.freedesktop.org/thumbnail-spec/latest/>
//!
//! The cache is shared with other apps (Nautilus, Dolphin…), so a
//! thumbnail made by one is reused by the others.

use std::path::{Path, PathBuf};

use gtk::{gdk_pixbuf, glib};

/// Our directory under `fail/`, where files we couldn't make a thumbnail
/// for are recorded.
pub const FAIL_DIR: &str = "lia-files";

/// The spec's thumbnail sizes (the longest side, in pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ThumbSize {
    Normal,
    Large,
    XLarge,
    XXLarge,
}

impl ThumbSize {
    pub const ALL: [Self; 4] = [Self::Normal, Self::Large, Self::XLarge, Self::XXLarge];

    pub fn pixels(self) -> i32 {
        match self {
            Self::Normal => 128,
            Self::Large => 256,
            Self::XLarge => 512,
            Self::XXLarge => 1024,
        }
    }

    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Large => "large",
            Self::XLarge => "x-large",
            Self::XXLarge => "xx-large",
        }
    }

    /// The smallest size covering `pixels` (device pixels).
    pub fn for_pixels(pixels: i32) -> Self {
        Self::ALL
            .into_iter()
            .find(|size| size.pixels() >= pixels)
            .unwrap_or(Self::XXLarge)
    }

    /// This size and the larger ones: any of them can be shown at this size.
    pub fn and_larger(self) -> impl Iterator<Item = Self> {
        Self::ALL.into_iter().filter(move |size| *size >= self)
    }
}

/// `$XDG_CACHE_HOME/thumbnails`.
pub fn cache_root() -> PathBuf {
    glib::user_cache_dir().join("thumbnails")
}

/// The file name of a thumbnail: the MD5 of the file's URI.
pub fn file_name(uri: &str) -> String {
    let hash = glib::compute_checksum_for_string(glib::ChecksumType::Md5, uri)
        .expect("MD5 is always available");
    format!("{hash}.png")
}

pub fn thumbnail_path(root: &Path, uri: &str, size: ThumbSize) -> PathBuf {
    root.join(size.dir_name()).join(file_name(uri))
}

pub fn fail_path(root: &Path, uri: &str) -> PathBuf {
    root.join("fail").join(FAIL_DIR).join(file_name(uri))
}

/// Loads the thumbnail at `path` if it was made for `uri` as last modified
/// at `mtime` (a UNIX timestamp).
pub fn load_valid(path: &Path, uri: &str, mtime: i64) -> Option<gdk_pixbuf::Pixbuf> {
    let pixbuf = gdk_pixbuf::Pixbuf::from_file(path).ok()?;
    is_valid(&pixbuf, uri, mtime).then_some(pixbuf)
}

/// A thumbnail is valid while the file's modification time matches the
/// one recorded in it.
pub fn is_valid(pixbuf: &gdk_pixbuf::Pixbuf, uri: &str, mtime: i64) -> bool {
    let recorded_mtime = pixbuf
        .option("tEXt::Thumb::MTime")
        .and_then(|value| value.trim().parse::<f64>().ok())
        .map(|value| value as i64);
    // An MD5 collision is unlikely, but the URI is there to check.
    let same_uri = pixbuf
        .option("tEXt::Thumb::URI")
        .is_none_or(|recorded| recorded == uri);
    recorded_mtime == Some(mtime) && same_uri
}

/// Writes `pixbuf` as the thumbnail of `uri`. The file is written next to
/// its destination and renamed into place, so readers never see half a
/// thumbnail, and only the user can read it (thumbnails reveal content).
pub fn save(
    pixbuf: &gdk_pixbuf::Pixbuf,
    path: &Path,
    uri: &str,
    mtime: i64,
    mime_type: Option<&str>,
) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let dir = path.parent().ok_or("no parent directory")?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|err| err.to_string())?;

    let mtime = mtime.to_string();
    let mut options = vec![
        ("tEXt::Thumb::URI", uri),
        ("tEXt::Thumb::MTime", mtime.as_str()),
        ("tEXt::Software", "Lia Files"),
    ];
    if let Some(mime_type) = mime_type {
        options.push(("tEXt::Thumb::Mimetype", mime_type));
    }

    let temp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    let result = pixbuf
        .savev(&temp, "png", &options)
        .map_err(|err| err.to_string())
        .and_then(|()| {
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))
                .and_then(|()| std::fs::rename(&temp, path))
                .map_err(|err| err.to_string())
        });
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Records that no thumbnail could be made for `uri` at `mtime`, so that
/// it isn't tried again until the file changes.
pub fn save_failure(root: &Path, uri: &str, mtime: i64) -> Result<(), String> {
    let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, 1, 1)
        .ok_or("cannot allocate an image")?;
    pixbuf.fill(0);
    save(&pixbuf, &fail_path(root, uri), uri, mtime, None)
}
