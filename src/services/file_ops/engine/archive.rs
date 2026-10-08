//! Extracting and creating archives, as steps of the [`Runner`].
//!
//! - Extraction writes into a hidden `.name.part` folder in the
//!   destination, then moves the result into place: the archive's single
//!   top-level item, or the folder itself (named after the archive) when
//!   there are several. Cancelling or failing removes the hidden folder, so
//!   no half-extracted tree is left under a real name.
//! - Entry paths are made relative and `..` is refused, and libarchive is
//!   told not to write through symbolic links: an archive can't write
//!   outside its folder.
//! - New archives are written to `.name.part`, then renamed.

use std::path::{Path, PathBuf};

use gettextrs::gettext;
use gtk::{gio, glib, prelude::*};

use super::{
    Abort, MergePolicy, NOFOLLOW, RenameResult, Runner, Target, display_name, unique_sibling,
};
use crate::services::archives::libarchive::{self, DiskWriter, EntryKind, Reader, Writer};
use crate::services::archives::{self, ArchiveFormat};
use crate::services::file_ops::naming::{self, UniqueStyle};
use crate::services::file_ops::operation::{ErrorChoice, ErrorKind};

/// Data is copied into archives in blocks of this size.
const BLOCK_SIZE: usize = 256 * 1024;

/// Removes a temporary file or folder when dropped (on success it's gone
/// or empty already).
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let result = match std::fs::symlink_metadata(&self.0) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&self.0),
            Ok(_) => std::fs::remove_file(&self.0),
            Err(_) => return,
        };
        if let Err(err) = result {
            tracing::warn!("cannot remove {}: {err}", self.0.display());
        }
    }
}

/// A file to put in a new archive.
struct Item {
    path: PathBuf,
    /// The path inside the archive.
    name: PathBuf,
    kind: EntryKind,
    size: u64,
    mode: u32,
    mtime: i64,
    link: Option<PathBuf>,
}

fn io_error(err: &std::io::Error) -> glib::Error {
    let code = match err.kind() {
        std::io::ErrorKind::PermissionDenied => gio::IOErrorEnum::PermissionDenied,
        std::io::ErrorKind::NotFound => gio::IOErrorEnum::NotFound,
        _ => gio::IOErrorEnum::Failed,
    };
    glib::Error::new(code, &err.to_string())
}

fn archive_error(err: &libarchive::Error) -> glib::Error {
    glib::Error::new(gio::IOErrorEnum::Failed, &err.message)
}

impl Runner<'_> {
    // ── Extract ────────────────────────────────────────────────────────────

    pub(super) fn run_extract(
        &mut self,
        archives: &[gio::File],
        destination: &gio::File,
    ) -> Result<(), Abort> {
        // Progress follows the compressed bytes read.
        let sizes: Vec<u64> = archives
            .iter()
            .map(|archive| {
                archive
                    .query_info(
                        gio::FILE_ATTRIBUTE_STANDARD_SIZE,
                        NOFOLLOW,
                        self.cancellable(),
                    )
                    .map_or(0, |info| info.size().max(0) as u64)
            })
            .collect();
        self.control.update(|p| p.bytes_total = sizes.iter().sum());
        for (archive, size) in archives.iter().zip(sizes) {
            self.check()?;
            let base = self.control.progress().bytes_done;
            if self.extract_one(archive, destination, base)? {
                self.succeeded += 1;
            }
            self.control.update(|p| p.bytes_done = base + size);
        }
        Ok(())
    }

    /// Extracts one archive. Returns whether its contents were placed.
    fn extract_one(
        &mut self,
        archive: &gio::File,
        destination: &gio::File,
        base: u64,
    ) -> Result<bool, Abort> {
        let name = display_name(archive);
        self.set_current(&name);
        let (Some(path), Some(_)) = (archive.path(), destination.path()) else {
            self.fail(
                archive,
                &gettext("Only archives on this computer or on mounted drives can be extracted"),
            )?;
            return Ok(false);
        };
        let content_type = archive
            .query_info(
                gio::FILE_ATTRIBUTE_STANDARD_CONTENT_TYPE,
                gio::FileQueryInfoFlags::NONE,
                self.cancellable(),
            )
            .ok()
            .and_then(|info| info.content_type())
            .map(String::from)
            .unwrap_or_default();
        let raw = archives::is_compressed_file(&content_type);
        let stem = archives::archive_stem(&name);

        // Hidden until complete.
        let temp = unique_sibling(
            &destination.child(naming::part_name(&stem).unwrap_or_else(|| ".archive.part".into())),
            true,
            UniqueStyle::Numbered,
        );
        let Some(temp_path) = temp.path() else {
            return Ok(false);
        };
        let cancellable = self.control.cancellable().clone();
        self.control.note_temporary(&temp);
        loop {
            if self
                .attempt(&temp, || temp.make_directory(Some(&cancellable)))?
                .is_none()
            {
                return Ok(false);
            }
            let scratch = Scratch(temp_path.clone());
            match self.extract_to(archive, &path, &temp_path, raw, &stem, base)? {
                Ok(()) => {
                    let placed = self.place_extracted(archive, &temp, destination, &stem)?;
                    drop(scratch);
                    return Ok(placed);
                }
                Err(err) => {
                    drop(scratch);
                    self.control.update(|p| p.bytes_done = base);
                    match self.ask_error(
                        archive,
                        &archive_error(&err),
                        ErrorKind::Generic,
                        err.fatal,
                    )? {
                        ErrorChoice::Retry => continue,
                        _ => return Ok(false),
                    }
                }
            }
        }
    }

    /// Extracts every entry of the archive at `path` into `temp`. The inner
    /// error ends this archive (the user is asked what to do).
    fn extract_to(
        &mut self,
        archive: &gio::File,
        path: &Path,
        temp: &Path,
        raw: bool,
        stem: &str,
        base: u64,
    ) -> Result<Result<(), libarchive::Error>, Abort> {
        let mut reader = match Reader::open(path, raw) {
            Ok(reader) => reader,
            Err(err) => return Ok(Err(err)),
        };
        let mut disk = match DiskWriter::new() {
            Ok(disk) => disk,
            Err(err) => return Ok(Err(err)),
        };
        let control = self.control;
        loop {
            self.check()?;
            let entry = match reader.next_entry() {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(err) => return Ok(Err(err)),
            };
            if entry.encrypted {
                return Ok(Err(libarchive::Error {
                    message: gettext(
                        "The archive is protected with a password. Password-protected \
                         archives can't be extracted yet.",
                    ),
                    fatal: true,
                }));
            }
            // A plain compressed file is one entry, named after the archive.
            let relative = if reader.is_raw() {
                Some(PathBuf::from(stem))
            } else {
                archives::sanitize_entry_path(&entry.path)
            };
            let Some(relative) = relative else {
                tracing::warn!(
                    "skipping unsafe path in {}: {}",
                    path.display(),
                    String::from_utf8_lossy(&entry.path)
                );
                continue;
            };
            let hardlink = entry
                .hardlink
                .as_deref()
                .and_then(archives::sanitize_entry_path)
                .map(|link| temp.join(link));
            if let Some(name) = relative.file_name() {
                self.set_current(&name.to_string_lossy());
            }
            let result = reader.extract_entry(
                &mut disk,
                &temp.join(&relative),
                hardlink.as_deref(),
                |read| {
                    control.update(|p| p.bytes_done = base + read);
                    control.wait_while_paused();
                    !control.is_cancelled()
                },
            );
            match result {
                Ok(true) => {}
                Ok(false) => return Err(Abort),
                Err(err) if err.fatal => return Ok(Err(err)),
                Err(err) => {
                    // One entry couldn't be written: skip it or stop.
                    let message = format!("{}: {}", relative.display(), err.message);
                    let err = glib::Error::new(gio::IOErrorEnum::Failed, &message);
                    self.ask_error(archive, &err, ErrorKind::Generic, true)?;
                }
            }
            self.control.update(|p| p.files_done += 1);
        }
        // Applies folder times and permissions, set last.
        if let Err(err) = disk.close() {
            tracing::debug!("finishing extraction of {}: {err}", path.display());
        }
        Ok(Ok(()))
    }

    /// Moves what was extracted into `temp` to the destination. Returns
    /// whether it was placed (not skipped).
    fn place_extracted(
        &mut self,
        archive: &gio::File,
        temp: &gio::File,
        destination: &gio::File,
        stem: &str,
    ) -> Result<bool, Abort> {
        let temp_path = temp.path().unwrap_or_default();
        let mut names: Vec<std::ffi::OsString> = std::fs::read_dir(&temp_path)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok().map(|e| e.file_name()))
                    .collect()
            })
            .unwrap_or_default();
        let (item, target) = match names.len() {
            0 => {
                self.fail(archive, &gettext("The archive is empty"))?;
                return Ok(false);
            }
            1 => {
                let name = names.pop().unwrap();
                (temp.child(&name), destination.child(&name))
            }
            _ => (temp.clone(), destination.child(stem)),
        };
        let is_dir = self.file_type(&item) == gio::FileType::Directory;
        // Folders aren't merged: "Keep both" gives "name (2)".
        let Target::Write { file, overwrite } =
            self.resolve(&item, is_dir, &target, MergePolicy::Never)?
        else {
            return Ok(false);
        };
        match self.try_rename(&item, &file, overwrite)? {
            RenameResult::Renamed => {
                self.created.push(file);
                Ok(true)
            }
            RenameResult::Skipped => Ok(false),
            RenameResult::Fallback => {
                self.fail(
                    archive,
                    &gettext("The extracted files couldn't be moved into place"),
                )?;
                Ok(false)
            }
        }
    }

    // ── Compress ───────────────────────────────────────────────────────────

    pub(super) fn run_compress(
        &mut self,
        sources: &[gio::File],
        target: &gio::File,
        format: ArchiveFormat,
    ) -> Result<(), Abort> {
        self.control.update(|p| p.scanning = true);
        let mut items = Vec::new();
        for source in sources {
            self.check()?;
            let Some(path) = source.path() else {
                self.fail(
                    source,
                    &gettext("Only files on this computer or on mounted drives can be compressed"),
                )?;
                continue;
            };
            let Some(name) = path.file_name().map(PathBuf::from) else {
                continue;
            };
            self.scan_item(&path, name, &mut items)?;
        }
        self.control.update(|p| p.scanning = false);
        if items.is_empty() {
            return Ok(());
        }

        let target = unique_sibling(target, false, UniqueStyle::Numbered);
        let part = target
            .path()
            .zip(target.basename())
            .and_then(|(path, name)| {
                let part = naming::part_name(&name.to_string_lossy())?;
                Some(path.with_file_name(part))
            });
        let Some(part) = part else {
            self.fail(
                &target,
                &gettext("Archives can only be created on this computer or on mounted drives"),
            )?;
            return Ok(());
        };
        self.control.note_temporary(&gio::File::for_path(&part));
        loop {
            let scratch = Scratch(part.clone());
            match self.write_archive(&part, format, &items)? {
                Ok(()) => {
                    let part_file = gio::File::for_path(&part);
                    let cancellable = self.control.cancellable().clone();
                    let moved = self.attempt(&target, || {
                        part_file.move_(
                            &target,
                            gio::FileCopyFlags::NOFOLLOW_SYMLINKS,
                            Some(&cancellable),
                            None,
                        )
                    })?;
                    drop(scratch);
                    if moved.is_some() {
                        self.created.push(target);
                        self.succeeded += 1;
                    }
                    return Ok(());
                }
                Err(err) => {
                    drop(scratch);
                    self.control.update(|p| {
                        p.bytes_done = 0;
                        p.files_done = 0;
                    });
                    match self.ask_error(&target, &err, ErrorKind::Generic, false)? {
                        ErrorChoice::Retry => continue,
                        _ => return Ok(()),
                    }
                }
            }
        }
    }

    /// Lists `path` (recursively for folders) as archive items named
    /// `name`…, counting totals for the progress.
    fn scan_item(
        &mut self,
        path: &Path,
        name: PathBuf,
        items: &mut Vec<Item>,
    ) -> Result<(), Abort> {
        use std::os::unix::fs::MetadataExt;
        let file = gio::File::for_path(path);
        let Some(meta) = self.attempt(&file, || {
            std::fs::symlink_metadata(path).map_err(|e| io_error(&e))
        })?
        else {
            return Ok(());
        };
        let file_type = meta.file_type();
        let kind = if file_type.is_dir() {
            EntryKind::Dir
        } else if file_type.is_file() {
            EntryKind::File
        } else if file_type.is_symlink() {
            EntryKind::Symlink
        } else {
            // Devices, sockets, pipes: not archived.
            return Ok(());
        };
        let link = if kind == EntryKind::Symlink {
            match self.attempt(&file, || std::fs::read_link(path).map_err(|e| io_error(&e)))? {
                Some(link) => Some(link),
                None => return Ok(()),
            }
        } else {
            None
        };
        let size = if kind == EntryKind::File {
            meta.len()
        } else {
            0
        };
        self.control.update(|p| {
            p.files_total += 1;
            p.bytes_total += size;
        });
        items.push(Item {
            path: path.to_owned(),
            name: name.clone(),
            kind,
            size,
            mode: meta.mode(),
            mtime: meta.mtime(),
            link,
        });
        if kind != EntryKind::Dir {
            return Ok(());
        }
        let Some(entries) =
            self.attempt(&file, || std::fs::read_dir(path).map_err(|e| io_error(&e)))?
        else {
            return Ok(());
        };
        // Sorted, so that archives of the same files are the same.
        let mut children: Vec<std::ffi::OsString> = entries
            .filter_map(|e| e.ok().map(|e| e.file_name()))
            .collect();
        children.sort();
        for child in children {
            self.check()?;
            self.scan_item(&path.join(&child), name.join(&child), items)?;
        }
        Ok(())
    }

    /// Writes `items` to a new archive at `part`. The inner error is about
    /// the whole archive (it may be retried).
    fn write_archive(
        &mut self,
        part: &Path,
        format: ArchiveFormat,
        items: &[Item],
    ) -> Result<Result<(), glib::Error>, Abort> {
        let mut writer = match Writer::create(part, format) {
            Ok(writer) => writer,
            Err(err) => return Ok(Err(archive_error(&err))),
        };
        let mut buffer = vec![0u8; BLOCK_SIZE];
        for item in items {
            self.check()?;
            if let Some(name) = item.name.file_name() {
                self.set_current(&name.to_string_lossy());
            }
            let written = match item.kind {
                EntryKind::Dir => writer.add_dir(&item.name, item.mode, item.mtime),
                EntryKind::Symlink => writer.add_symlink(
                    &item.name,
                    item.link.as_deref().unwrap_or(Path::new("")),
                    item.mtime,
                ),
                _ => {
                    let file = gio::File::for_path(&item.path);
                    let Some(mut input) = self.attempt(&file, || {
                        std::fs::File::open(&item.path).map_err(|e| io_error(&e))
                    })?
                    else {
                        // Skipped: counted as done.
                        self.control.update(|p| {
                            p.files_done += 1;
                            p.bytes_done += item.size;
                        });
                        continue;
                    };
                    match self.copy_into(&mut writer, &mut input, item, &mut buffer)? {
                        Ok(()) => Ok(()),
                        Err(err) => return Ok(Err(err)),
                    }
                }
            };
            if let Err(err) = written {
                return Ok(Err(archive_error(&err)));
            }
            self.control.update(|p| p.files_done += 1);
        }
        Ok(writer.close().map_err(|err| archive_error(&err)))
    }

    /// Writes one file's entry and contents.
    fn copy_into(
        &mut self,
        writer: &mut Writer,
        input: &mut std::fs::File,
        item: &Item,
        buffer: &mut [u8],
    ) -> Result<Result<(), glib::Error>, Abort> {
        use std::io::Read;
        if let Err(err) = writer.begin_file(&item.name, item.mode, item.size, item.mtime) {
            return Ok(Err(archive_error(&err)));
        }
        let mut done = 0u64;
        loop {
            self.check()?;
            let read = match input.read(buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(err) => {
                    let message = format!("{}: {err}", item.path.display());
                    return Ok(Err(glib::Error::new(gio::IOErrorEnum::Failed, &message)));
                }
            };
            if let Err(err) = writer.write_data(&buffer[..read]) {
                return Ok(Err(archive_error(&err)));
            }
            // A file that grew since the scan is cut at its scanned size.
            let counted = (read as u64).min(item.size.saturating_sub(done));
            done += read as u64;
            self.control.update(|p| p.bytes_done += counted);
        }
        if done < item.size {
            self.control.update(|p| p.bytes_done += item.size - done);
        }
        Ok(writer.finish_entry().map_err(|err| archive_error(&err)))
    }
}
