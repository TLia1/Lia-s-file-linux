//! Pure helpers for file names: extensions, "copy" and numbered names, and
//! validation of names typed by the user.

use gettextrs::gettext;

/// Multi-part extensions kept together ("backup.tar.gz" → "backup" + ".tar.gz").
const COMPOUND_EXTENSIONS: [&str; 7] = [
    ".tar.gz",
    ".tar.bz2",
    ".tar.xz",
    ".tar.zst",
    ".tar.lz",
    ".tar.lzma",
    ".tar.z",
];

/// Longest extension (without the dot) still treated as one; longer
/// suffixes, or suffixes with spaces ("Mr. Smith"), are part of the name.
const MAX_EXTENSION_LEN: usize = 8;

/// Most file systems limit a name to 255 bytes.
pub const MAX_NAME_BYTES: usize = 255;

/// Characters FAT, exFAT and NTFS can't store.
const PORTABLE_FORBIDDEN: [char; 9] = ['<', '>', ':', '"', '\\', '|', '?', '*', '/'];

/// Device names reserved on FAT/NTFS, with or without an extension.
const RESERVED_NAMES: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Splits a file name into stem and extension (including the dot).
/// Folders and dotfiles without another dot have no extension.
pub fn split_extension(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir {
        return (name, "");
    }
    let lower = name.to_lowercase();
    for compound in COMPOUND_EXTENSIONS {
        // `to_lowercase` keeps ASCII byte offsets, and the suffixes are ASCII.
        if lower.ends_with(compound) && lower.len() == name.len() && name.len() > compound.len() {
            return name.split_at(name.len() - compound.len());
        }
    }
    match name.rfind('.') {
        Some(dot) if dot > 0 => {
            let extension = &name[dot + 1..];
            if extension.is_empty()
                || extension.chars().count() > MAX_EXTENSION_LEN
                || extension.contains(char::is_whitespace)
            {
                (name, "")
            } else {
                name.split_at(dot)
            }
        }
        _ => (name, ""),
    }
}

/// Number of characters to pre-select when renaming: the name without its
/// extension.
pub fn stem_char_count(name: &str, is_dir: bool) -> usize {
    split_extension(name, is_dir).0.chars().count()
}

/// Strips a trailing " (n)" counter. Returns the base and the counter.
/// Large numbers are part of the name ("Trip (2024)"), not counters.
fn strip_counter(stem: &str) -> (&str, Option<u32>) {
    if let Some(inner) = stem.strip_suffix(')')
        && let Some(open) = inner.rfind(" (")
        && let Ok(n) = inner[open + 2..].parse::<u32>()
        && !inner[open + 2..].starts_with('+')
        && n < 1000
    {
        return (&stem[..open], Some(n));
    }
    (stem, None)
}

/// Strips a trailing " (copy)" or " (copy n)" marker.
fn strip_copy_marker(stem: &str) -> &str {
    let word = gettext("copy");
    let Some(inner) = stem.strip_suffix(')') else {
        return stem;
    };
    let marker = format!(" ({word}");
    if let Some(start) = inner.rfind(&marker) {
        let rest = &inner[start + marker.len()..];
        if rest.is_empty()
            || rest
                .strip_prefix(' ')
                .is_some_and(|n| n.parse::<u32>().is_ok())
        {
            return &stem[..start];
        }
    }
    stem
}

/// "name (2).ext", "name (3).ext"… for "Keep both". An existing counter
/// is replaced rather than appended to, so "name (2)" becomes "name (3)".
pub fn numbered_name(name: &str, is_dir: bool, n: u32) -> String {
    let (stem, extension) = split_extension(name, is_dir);
    let (base, _) = strip_counter(stem);
    format!("{base} ({n}){extension}")
}

/// "name (copy).ext", then "name (copy 2).ext"… for duplicates. Copying a
/// copy doesn't stack markers.
pub fn copy_name(name: &str, is_dir: bool, n: u32) -> String {
    let (stem, extension) = split_extension(name, is_dir);
    let base = strip_copy_marker(stem);
    let word = gettext("copy");
    if n <= 1 {
        format!("{base} ({word}){extension}")
    } else {
        format!("{base} ({word} {n}){extension}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UniqueStyle {
    /// "name (2).ext"
    Numbered,
    /// "name (copy).ext"
    Copy,
}

/// The first variant of `name` for which `exists` returns false. With
/// `Numbered`, `name` itself is tried first.
pub fn unique_name(
    name: &str,
    is_dir: bool,
    style: UniqueStyle,
    exists: impl Fn(&str) -> bool,
) -> String {
    if style == UniqueStyle::Numbered && !exists(name) {
        return name.to_owned();
    }
    let first = match style {
        UniqueStyle::Numbered => strip_counter(split_extension(name, is_dir).0)
            .1
            .map_or(2, |n| n + 1),
        UniqueStyle::Copy => 1,
    };
    (first..)
        .map(|n| match style {
            UniqueStyle::Numbered => numbered_name(name, is_dir, n),
            UniqueStyle::Copy => copy_name(name, is_dir, n),
        })
        .find(|candidate| !exists(candidate))
        .unwrap()
}

/// Name of the temporary file a copy is written to before being renamed
/// into place, or `None` if it would be too long.
pub fn part_name(name: &str) -> Option<String> {
    let part = format!(".{name}.part");
    (part.len() <= MAX_NAME_BYTES).then_some(part)
}

/// Whether a file system type (`filesystem::type`) needs Windows-portable
/// names.
pub fn needs_portable_names(fs_type: &str) -> bool {
    matches!(
        fs_type,
        "vfat" | "msdos" | "fat" | "exfat" | "ntfs" | "ntfs3" | "fuseblk"
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NameError {
    Empty,
    Dot,
    Slash,
    TooLong,
    /// A character the target file system can't store.
    Forbidden(char),
    Reserved,
    TrailingDotOrSpace,
}

impl NameError {
    pub fn message(&self) -> String {
        match self {
            Self::Empty => gettext("The name can't be empty"),
            Self::Dot => gettext("“.” and “..” are reserved names"),
            Self::Slash => gettext("The name can't contain “/”"),
            Self::TooLong => gettext("The name is too long"),
            Self::Forbidden(c) => {
                gettext("This drive doesn't allow “{}” in names").replace("{}", &c.to_string())
            }
            Self::Reserved => gettext("This name is reserved on this drive"),
            Self::TrailingDotOrSpace => {
                gettext("On this drive, names can't end with a dot or a space")
            }
        }
    }
}

/// Checks a name typed by the user. `portable` applies the extra FAT/NTFS
/// rules.
pub fn validate_name(name: &str, portable: bool) -> Result<(), NameError> {
    if name.is_empty() {
        return Err(NameError::Empty);
    }
    if name == "." || name == ".." {
        return Err(NameError::Dot);
    }
    if name.contains('/') {
        return Err(NameError::Slash);
    }
    if name.contains('\0') {
        return Err(NameError::Forbidden('\0'));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(NameError::TooLong);
    }
    if portable {
        if let Some(c) = name
            .chars()
            .find(|c| PORTABLE_FORBIDDEN.contains(c) || c.is_control())
        {
            return Err(NameError::Forbidden(c));
        }
        if name.ends_with('.') || name.ends_with(' ') {
            return Err(NameError::TrailingDotOrSpace);
        }
        let stem = name.split('.').next().unwrap_or(name).trim_end();
        if RESERVED_NAMES.iter().any(|r| r.eq_ignore_ascii_case(stem)) {
            return Err(NameError::Reserved);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_extensions() {
        assert_eq!(split_extension("report.pdf", false), ("report", ".pdf"));
        assert_eq!(split_extension("a.b.c.txt", false), ("a.b.c", ".txt"));
        assert_eq!(
            split_extension("backup.tar.gz", false),
            ("backup", ".tar.gz")
        );
        assert_eq!(
            split_extension("Backup.TAR.GZ", false),
            ("Backup", ".TAR.GZ")
        );
        assert_eq!(split_extension(".bashrc", false), (".bashrc", ""));
        assert_eq!(split_extension(".config.json", false), (".config", ".json"));
        assert_eq!(split_extension("Makefile", false), ("Makefile", ""));
        assert_eq!(split_extension("trailing.", false), ("trailing.", ""));
        assert_eq!(split_extension("Mr. Smith", false), ("Mr. Smith", ""));
        assert_eq!(split_extension("photos.2024", true), ("photos.2024", ""));
        assert_eq!(split_extension(".tar.gz", false), (".tar", ".gz"));
        assert_eq!(stem_char_count("été.txt", false), 3);
    }

    #[test]
    fn numbers_names() {
        assert_eq!(numbered_name("a.txt", false, 2), "a (2).txt");
        assert_eq!(numbered_name("a (2).txt", false, 3), "a (3).txt");
        assert_eq!(numbered_name("folder", true, 2), "folder (2)");
        assert_eq!(numbered_name("x.tar.gz", false, 2), "x (2).tar.gz");
        assert_eq!(
            numbered_name("Trip (2024).jpg", false, 2),
            "Trip (2024) (2).jpg"
        );
        assert_eq!(copy_name("a.txt", false, 1), "a (copy).txt");
        assert_eq!(copy_name("a.txt", false, 2), "a (copy 2).txt");
        assert_eq!(copy_name("a (copy).txt", false, 2), "a (copy 2).txt");
        assert_eq!(copy_name("a (copy 3).txt", false, 4), "a (copy 4).txt");
        assert_eq!(copy_name("Notes", true, 1), "Notes (copy)");
    }

    #[test]
    fn finds_unique_names() {
        let taken = ["a.txt", "a (2).txt", "a (copy).txt"];
        let exists = |n: &str| taken.contains(&n);
        assert_eq!(
            unique_name("b.txt", false, UniqueStyle::Numbered, exists),
            "b.txt"
        );
        assert_eq!(
            unique_name("a.txt", false, UniqueStyle::Numbered, exists),
            "a (3).txt"
        );
        assert_eq!(
            unique_name("a (2).txt", false, UniqueStyle::Numbered, exists),
            "a (3).txt"
        );
        assert_eq!(
            unique_name("a.txt", false, UniqueStyle::Copy, exists),
            "a (copy 2).txt"
        );
    }

    #[test]
    fn part_names() {
        assert_eq!(part_name("a.txt").as_deref(), Some(".a.txt.part"));
        assert_eq!(part_name(&"x".repeat(250)), None);
    }

    #[test]
    fn validates_names() {
        assert_eq!(validate_name("ok.txt", false), Ok(()));
        assert_eq!(validate_name("", false), Err(NameError::Empty));
        assert_eq!(validate_name("..", false), Err(NameError::Dot));
        assert_eq!(validate_name("a/b", false), Err(NameError::Slash));
        assert_eq!(
            validate_name(&"x".repeat(256), false),
            Err(NameError::TooLong)
        );
        assert_eq!(validate_name("what?", false), Ok(()));
        assert_eq!(validate_name("what?", true), Err(NameError::Forbidden('?')));
        assert_eq!(validate_name("a:b", true), Err(NameError::Forbidden(':')));
        assert_eq!(
            validate_name("name.", true),
            Err(NameError::TrailingDotOrSpace)
        );
        assert_eq!(validate_name("con.txt", true), Err(NameError::Reserved));
        assert_eq!(validate_name("console.txt", true), Ok(()));
        assert!(needs_portable_names("vfat"));
        assert!(!needs_portable_names("ext4"));
    }
}
