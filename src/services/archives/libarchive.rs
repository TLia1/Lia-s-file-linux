//! Safe wrappers around libarchive: [`Reader`] lists and extracts archives
//! (any format and filter libarchive knows), [`Writer`] creates them.
//!
//! Both are blocking and meant for the file-operation worker threads. Paths
//! are passed to libarchive as raw bytes (the locale is UTF-8, see
//! `init_i18n`).

use std::ffi::{CStr, CString, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use super::ArchiveFormat;
use super::ffi::*;

/// An error reported by libarchive.
#[derive(Debug, Clone)]
pub struct Error {
    pub message: String,
    /// Nothing more can be done with this archive.
    pub fatal: bool,
}

impl Error {
    fn new(message: impl Into<String>, fatal: bool) -> Self {
        Self {
            message: message.into(),
            fatal,
        }
    }

    /// The last error of `a`.
    fn last(a: *mut archive, fatal: bool) -> Self {
        // SAFETY: `a` is a live archive handle; the string is copied.
        let message = unsafe {
            let ptr = archive_error_string(a);
            if ptr.is_null() {
                "Unknown error".to_owned()
            } else {
                CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        };
        Self::new(message, fatal)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

fn c_path(path: &Path) -> Result<CString, Error> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| Error::new("Invalid file name", true))
}

/// Checks a libarchive status: warnings are fine, `FAILED` concerns the
/// current entry only, anything worse ends the archive.
fn check(a: *mut archive, status: c_int) -> Result<(), Error> {
    match status {
        ARCHIVE_OK | ARCHIVE_WARN => Ok(()),
        ARCHIVE_FAILED => Err(Error::last(a, false)),
        _ => Err(Error::last(a, true)),
    }
}

// ── Reading ────────────────────────────────────────────────────────────────

/// What an entry of an archive is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// An entry's header, as stored in the archive.
#[derive(Debug, Clone)]
pub struct EntryInfo {
    /// The path inside the archive, unchecked (see `sanitize_entry_path`).
    pub path: Vec<u8>,
    /// For hard links: the entry they link to.
    pub hardlink: Option<Vec<u8>>,
    pub kind: EntryKind,
    pub size: u64,
    pub encrypted: bool,
}

/// An archive opened for reading.
pub struct Reader {
    a: *mut archive,
    /// The header just read, valid until the next one.
    entry: *mut archive_entry,
}

impl Reader {
    /// Opens the archive at `path`. `raw` also accepts a plain compressed
    /// file (`.gz`, `.xz`…), seen as one entry.
    pub fn open(path: &Path, raw: bool) -> Result<Self, Error> {
        let path = c_path(path)?;
        // SAFETY: plain libarchive calls on a handle we own.
        unsafe {
            let a = archive_read_new();
            if a.is_null() {
                return Err(Error::new("Out of memory", true));
            }
            let reader = Self {
                a,
                entry: std::ptr::null_mut(),
            };
            archive_read_support_filter_all(a);
            archive_read_support_format_all(a);
            if raw {
                archive_read_support_format_raw(a);
            }
            check(a, archive_read_open_filename(a, path.as_ptr(), 64 * 1024))
                .map_err(|err| Error::new(err.message, true))?;
            Ok(reader)
        }
    }

    /// Reads the next header; `None` at the end of the archive.
    pub fn next_entry(&mut self) -> Result<Option<EntryInfo>, Error> {
        // SAFETY: the entry pointer is owned by the archive and only read
        // until the next call.
        unsafe {
            let mut entry = std::ptr::null_mut();
            let status = archive_read_next_header(self.a, &mut entry);
            if status == ARCHIVE_EOF {
                self.entry = std::ptr::null_mut();
                return Ok(None);
            }
            if !matches!(status, ARCHIVE_OK | ARCHIVE_WARN) {
                self.entry = std::ptr::null_mut();
                // A broken header can't be skipped reliably.
                return Err(Error::last(self.a, true));
            }
            self.entry = entry;
            let bytes = |ptr: *const std::ffi::c_char| {
                (!ptr.is_null()).then(|| CStr::from_ptr(ptr).to_bytes().to_vec())
            };
            let kind = match archive_entry_filetype(entry) & AE_IFMT {
                AE_IFREG => EntryKind::File,
                AE_IFDIR => EntryKind::Dir,
                AE_IFLNK => EntryKind::Symlink,
                _ => EntryKind::Other,
            };
            Ok(Some(EntryInfo {
                path: bytes(archive_entry_pathname(entry)).unwrap_or_default(),
                hardlink: bytes(archive_entry_hardlink(entry)),
                kind,
                size: archive_entry_size(entry).max(0) as u64,
                encrypted: archive_entry_is_encrypted(entry) != 0,
            }))
        }
    }

    /// Whether the archive is a plain compressed file (see [`Reader::open`]).
    /// Known once the first header was read.
    pub fn is_raw(&self) -> bool {
        // SAFETY: plain query on a live handle.
        unsafe { archive_format(self.a) & ARCHIVE_FORMAT_BASE_MASK == ARCHIVE_FORMAT_RAW }
    }

    /// Bytes read so far from the archive file (compressed).
    pub fn bytes_read(&self) -> u64 {
        // SAFETY: plain query on a live handle.
        unsafe { archive_filter_bytes(self.a, -1).max(0) as u64 }
    }

    /// Writes the current entry to `path` on disk (and its hard link
    /// target to `hardlink`). `between_blocks` is called after each block
    /// of data with [`Reader::bytes_read`]; returning false stops
    /// (`Ok(false)`).
    pub fn extract_entry(
        &mut self,
        disk: &mut DiskWriter,
        path: &Path,
        hardlink: Option<&Path>,
        mut between_blocks: impl FnMut(u64) -> bool,
    ) -> Result<bool, Error> {
        if self.entry.is_null() {
            return Err(Error::new("No entry to extract", true));
        }
        let path = c_path(path)?;
        let hardlink = hardlink.map(c_path).transpose()?;
        // SAFETY: the entry belongs to this reader and is current; the
        // block pointers are only used before the next read.
        unsafe {
            archive_entry_copy_pathname(self.entry, path.as_ptr());
            if let Some(hardlink) = &hardlink {
                archive_entry_copy_hardlink(self.entry, hardlink.as_ptr());
            }
            check(disk.a, archive_write_header(disk.a, self.entry))?;
            loop {
                let mut buffer: *const c_void = std::ptr::null();
                let mut size = 0usize;
                let mut offset: la_int64_t = 0;
                let status = archive_read_data_block(self.a, &mut buffer, &mut size, &mut offset);
                if status == ARCHIVE_EOF {
                    break;
                }
                check(self.a, status)?;
                if archive_write_data_block(disk.a, buffer, size, offset) < 0 {
                    return Err(Error::last(disk.a, false));
                }
                if !between_blocks(self.bytes_read()) {
                    return Ok(false);
                }
            }
            check(disk.a, archive_write_finish_entry(disk.a))?;
        }
        Ok(true)
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and freed once.
        unsafe {
            archive_read_free(self.a);
        }
    }
}

/// Writes extracted entries to disk.
pub struct DiskWriter {
    a: *mut archive,
}

impl DiskWriter {
    /// Restores permissions and times (not owners), never overwrites, and
    /// refuses paths with `..` or going through symbolic links.
    pub fn new() -> Result<Self, Error> {
        // SAFETY: plain libarchive calls on a handle we own.
        unsafe {
            let a = archive_write_disk_new();
            if a.is_null() {
                return Err(Error::new("Out of memory", true));
            }
            archive_write_disk_set_options(
                a,
                ARCHIVE_EXTRACT_PERM
                    | ARCHIVE_EXTRACT_TIME
                    | ARCHIVE_EXTRACT_NO_OVERWRITE
                    | ARCHIVE_EXTRACT_SECURE_SYMLINKS
                    | ARCHIVE_EXTRACT_SECURE_NODOTDOT,
            );
            Ok(Self { a })
        }
    }

    /// Applies what is left to do (e.g. folder times, set last), then
    /// closes.
    pub fn close(self) -> Result<(), Error> {
        // SAFETY: live handle; freed by `Drop`.
        unsafe { check(self.a, archive_write_close(self.a)) }
    }
}

impl Drop for DiskWriter {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and freed once.
        unsafe {
            archive_write_free(self.a);
        }
    }
}

// ── Writing ────────────────────────────────────────────────────────────────

/// An archive being created.
pub struct Writer {
    a: *mut archive,
}

impl Writer {
    pub fn create(path: &Path, format: ArchiveFormat) -> Result<Self, Error> {
        let path = c_path(path)?;
        // SAFETY: plain libarchive calls on a handle we own.
        unsafe {
            let a = archive_write_new();
            if a.is_null() {
                return Err(Error::new("Out of memory", true));
            }
            let writer = Self { a };
            let status = match format {
                ArchiveFormat::Zip => archive_write_set_format_zip(a),
                ArchiveFormat::SevenZip => archive_write_set_format_7zip(a),
                ArchiveFormat::TarGz => {
                    check(a, archive_write_set_format_pax_restricted(a))?;
                    archive_write_add_filter_gzip(a)
                }
                ArchiveFormat::TarXz => {
                    check(a, archive_write_set_format_pax_restricted(a))?;
                    archive_write_add_filter_xz(a)
                }
            };
            check(a, status).map_err(|err| Error::new(err.message, true))?;
            check(a, archive_write_open_filename(a, path.as_ptr()))
                .map_err(|err| Error::new(err.message, true))?;
            Ok(writer)
        }
    }

    fn write_header(
        &mut self,
        name: &Path,
        filetype: mode_t,
        perm: u32,
        size: u64,
        mtime: i64,
        symlink: Option<&Path>,
    ) -> Result<(), Error> {
        let name = c_path(name)?;
        let symlink = symlink.map(c_path).transpose()?;
        // SAFETY: the entry is created, used and freed here.
        unsafe {
            let entry = archive_entry_new();
            if entry.is_null() {
                return Err(Error::new("Out of memory", true));
            }
            archive_entry_copy_pathname(entry, name.as_ptr());
            archive_entry_set_filetype(entry, filetype);
            archive_entry_set_perm(entry, perm & 0o7777);
            archive_entry_set_size(entry, size as la_int64_t);
            archive_entry_set_mtime(entry, mtime, 0);
            if let Some(symlink) = &symlink {
                archive_entry_copy_symlink(entry, symlink.as_ptr());
            }
            let status = archive_write_header(self.a, entry);
            archive_entry_free(entry);
            check(self.a, status)
        }
    }

    pub fn add_dir(&mut self, name: &Path, perm: u32, mtime: i64) -> Result<(), Error> {
        self.write_header(name, AE_IFDIR, perm, 0, mtime, None)
    }

    pub fn add_symlink(&mut self, name: &Path, target: &Path, mtime: i64) -> Result<(), Error> {
        self.write_header(name, AE_IFLNK, 0o777, 0, mtime, Some(target))
    }

    /// Starts a file entry; write its contents with [`Writer::write_data`].
    pub fn begin_file(
        &mut self,
        name: &Path,
        perm: u32,
        size: u64,
        mtime: i64,
    ) -> Result<(), Error> {
        self.write_header(name, AE_IFREG, perm, size, mtime, None)
    }

    pub fn write_data(&mut self, data: &[u8]) -> Result<(), Error> {
        // SAFETY: `data` is valid for its length.
        let written = unsafe { archive_write_data(self.a, data.as_ptr().cast(), data.len()) };
        if written < 0 {
            return Err(Error::last(self.a, true));
        }
        Ok(())
    }

    /// Ends the entry started last.
    pub fn finish_entry(&mut self) -> Result<(), Error> {
        // SAFETY: live handle.
        unsafe { check(self.a, archive_write_finish_entry(self.a)) }
    }

    /// Writes what's buffered and closes the file. Dropping a writer
    /// without closing it leaves an incomplete archive.
    pub fn close(self) -> Result<(), Error> {
        // SAFETY: live handle; freed by `Drop`.
        unsafe {
            check(self.a, archive_write_close(self.a)).map_err(|err| Error::new(err.message, true))
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // SAFETY: the handle is ours and freed once.
        unsafe {
            archive_write_free(self.a);
        }
    }
}
