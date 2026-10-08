//! Executes an [`Operation`].
//!
//! [`run`] is synchronous and meant for a worker thread, like Nautilus'
//! file operations: it uses GIO's blocking calls (which go through GVfs for
//! `smb://`, `mtp://`, `trash:///`…), so the code reads top to bottom and
//! recursion needs no async plumbing. The UI thread follows along through
//! the shared [`Control`]; questions go through [`Interaction`], whose
//! methods block until the user answers.
//!
//! Safety rules:
//! - Local copies are written to `.name.part` and renamed into place once
//!   complete, so an interrupted copy never leaves a truncated file under
//!   the real name. Partial files are removed on failure or cancellation.
//! - Moves try an instant rename first and fall back to copy + delete; a
//!   source is deleted only once everything in it was copied.
//! - Nothing is overwritten without the user choosing "Replace".
//! - Archives are extracted into a hidden folder first, and created under
//!   a `.part` name (see [`archive`]).

mod archive;
mod batch_rename;

use std::collections::HashMap;
use std::path::PathBuf;

use gettextrs::gettext;
use gtk::{gio, glib, prelude::*};

use super::naming::{self, UniqueStyle};
use super::operation::*;
use super::trash;
use crate::services::undo::UndoRecord;

const SCAN_ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,standard::size";
const SUMMARY_ATTRIBUTES: &str = "standard::display-name,standard::type,standard::size,\
                                  standard::content-type,time::modified,thumbnail::path";
const TRASH_ATTRIBUTES: &str = "standard::name,trash::orig-path,trash::deletion-date";
pub const TRASH_URI: &str = "trash:///";

const NOFOLLOW: gio::FileQueryInfoFlags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;

/// Runs `operation` to completion (or cancellation), blocking the calling
/// thread.
pub fn run(operation: &Operation, control: &Control, ui: &dyn Interaction) -> Outcome {
    let mut runner = Runner {
        control,
        ui,
        conflict_all: None,
        error_all: None,
        count_bytes: true,
        created: Vec::new(),
        moved: Vec::new(),
        trashed: Vec::new(),
        renamed: Vec::new(),
        errors: Vec::new(),
        skipped: 0,
        succeeded: 0,
    };
    let result = runner.dispatch(operation);
    let status = if result.is_err() || control.is_cancelled() {
        Status::Cancelled
    } else {
        Status::Done
    };
    let undo = runner.undo_record(operation);
    control.update(|p| {
        p.scanning = false;
        p.current.clear();
    });
    Outcome {
        status,
        undo,
        created: runner.created,
        errors: runner.errors,
        skipped: runner.skipped,
        succeeded: runner.succeeded,
    }
}

/// The user cancelled (or the job was cancelled): unwind.
struct Abort;

/// `Ok(Some(_))`: done · `Ok(None)`: skipped · `Err(Abort)`: stop.
type Step<T> = Result<Option<T>, Abort>;

/// A scanned file, with its children for folders.
struct Node {
    file: gio::File,
    name: PathBuf,
    display_name: String,
    file_type: gio::FileType,
    size: u64,
    children: Vec<Node>,
    /// Some children couldn't be listed (and were skipped).
    incomplete: bool,
}

impl Node {
    fn from_info(file: gio::File, info: &gio::FileInfo) -> Self {
        Self {
            file,
            name: info.name(),
            display_name: info.display_name().into(),
            file_type: info.file_type(),
            size: info.size().max(0) as u64,
            children: Vec::new(),
            incomplete: false,
        }
    }

    fn is_dir(&self) -> bool {
        self.file_type == gio::FileType::Directory
    }

    fn total_size(&self) -> u64 {
        if self.is_dir() {
            self.children.iter().map(Node::total_size).sum()
        } else {
            self.size
        }
    }

    fn count(&self) -> u64 {
        1 + self.children.iter().map(Node::count).sum::<u64>()
    }
}

/// Where an item goes once its conflict (if any) is resolved.
enum Target {
    /// Create the item here, overwriting an existing file if `overwrite`.
    Write {
        file: gio::File,
        overwrite: bool,
    },
    /// Merge into this existing folder.
    Merge(gio::File),
    Skip,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Copy,
    Move,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MergePolicy {
    /// Ask before merging two folders (top-level items).
    Ask,
    /// Merge without asking (inside a folder the user chose to merge).
    Silent,
    /// Folders can't be merged (restoring, linking).
    Never,
}

enum RenameResult {
    Renamed,
    /// A rename isn't possible (other file system…): copy + delete.
    Fallback,
    Skipped,
}

struct Runner<'a> {
    control: &'a Control,
    ui: &'a dyn Interaction,
    /// "Do this for all conflicts".
    conflict_all: Option<ConflictChoice>,
    /// "Skip all" / "Delete all permanently".
    error_all: Option<ErrorChoice>,
    /// Progress counts bytes (transfers) or only files (deletion).
    count_bytes: bool,
    /// Top-level items created (copies, new files, links, restored items).
    created: Vec<gio::File>,
    /// Items moved or renamed: (from, to).
    moved: Vec<(gio::File, gio::File)>,
    trashed: Vec<gio::File>,
    /// Batch renames done: (original file, original name, final file,
    /// final name).
    renamed: Vec<(gio::File, String, gio::File, String)>,
    errors: Vec<String>,
    skipped: u32,
    succeeded: u32,
}

impl Runner<'_> {
    fn dispatch(&mut self, operation: &Operation) -> Result<(), Abort> {
        match operation {
            Operation::Copy { pairs } => self.run_transfer(pairs, Mode::Copy),
            Operation::Move { pairs } => self.run_transfer(pairs, Mode::Move),
            Operation::Duplicate { files } => {
                let pairs: Vec<_> = files
                    .iter()
                    .map(|file| {
                        let is_dir = self.file_type(file) == gio::FileType::Directory;
                        (
                            file.clone(),
                            unique_sibling(file, is_dir, UniqueStyle::Copy),
                        )
                    })
                    .collect();
                self.run_transfer(&pairs, Mode::Copy)
            }
            Operation::Link { pairs } => self.run_link(pairs),
            Operation::Trash { files } => self.run_trash(files),
            Operation::Delete { files } => self.run_delete(files),
            Operation::EmptyTrash => self.run_empty_trash(),
            Operation::Restore { files } => self.run_restore(files),
            Operation::Rename { file, name } => self.run_rename(file, name),
            Operation::CreateFolder { parent, name } => self.run_create(parent, name, None, true),
            Operation::CreateFile {
                parent,
                name,
                template,
            } => self.run_create(parent, name, template.as_ref(), false),
            Operation::BatchRename { renames } => self.run_batch_rename(renames),
            Operation::Extract {
                archives,
                destination,
            } => self.run_extract(archives, destination),
            Operation::Compress {
                sources,
                target,
                format,
            } => self.run_compress(sources, target, *format),
        }
    }

    /// The record that reverts what was actually done.
    fn undo_record(&self, operation: &Operation) -> Option<UndoRecord> {
        let trash_created = || Operation::Trash {
            files: self.created.clone(),
        };
        let reversed = || Operation::Move {
            pairs: self
                .moved
                .iter()
                .rev()
                .map(|(from, to)| (to.clone(), from.clone()))
                .collect(),
        };
        let record = match operation {
            Operation::Copy { .. }
            | Operation::Duplicate { .. }
            | Operation::Link { .. }
            | Operation::Extract { .. }
            | Operation::Compress { .. }
                if !self.created.is_empty() =>
            {
                UndoRecord::new(operation.clone(), trash_created())
            }
            Operation::Move { .. } if !self.moved.is_empty() => UndoRecord::new(
                Operation::Move {
                    pairs: self.moved.clone(),
                },
                reversed(),
            ),
            Operation::Rename { .. } if !self.moved.is_empty() => {
                UndoRecord::new(operation.clone(), reversed())
            }
            Operation::BatchRename { .. } if !self.renamed.is_empty() => UndoRecord::new(
                Operation::BatchRename {
                    renames: self
                        .renamed
                        .iter()
                        .map(|(from, _, _, name)| (from.clone(), name.clone()))
                        .collect(),
                },
                Operation::BatchRename {
                    renames: self
                        .renamed
                        .iter()
                        .map(|(_, name, to, _)| (to.clone(), name.clone()))
                        .collect(),
                },
            ),
            Operation::Trash { .. } if !self.trashed.is_empty() => UndoRecord::new(
                Operation::Trash {
                    files: self.trashed.clone(),
                },
                Operation::Restore {
                    files: self.trashed.clone(),
                },
            ),
            Operation::Restore { .. } if !self.created.is_empty() => UndoRecord::new(
                Operation::Restore {
                    files: self.created.clone(),
                },
                trash_created(),
            ),
            // Redo recreates the item under the name it actually got.
            Operation::CreateFolder { parent, .. } => {
                let name = self.created.first()?.basename()?;
                UndoRecord::new(
                    Operation::CreateFolder {
                        parent: parent.clone(),
                        name: name.to_string_lossy().into_owned(),
                    },
                    trash_created(),
                )
            }
            Operation::CreateFile {
                parent, template, ..
            } => {
                let name = self.created.first()?.basename()?;
                UndoRecord::new(
                    Operation::CreateFile {
                        parent: parent.clone(),
                        name: name.to_string_lossy().into_owned(),
                        template: template.clone(),
                    },
                    trash_created(),
                )
            }
            _ => return None,
        };
        Some(record)
    }

    // ── Plumbing ───────────────────────────────────────────────────────────

    fn cancellable(&self) -> Option<&gio::Cancellable> {
        Some(self.control.cancellable())
    }

    /// Waits while paused; stops if cancelled.
    fn check(&self) -> Result<(), Abort> {
        self.control.wait_while_paused();
        if self.control.is_cancelled() {
            Err(Abort)
        } else {
            Ok(())
        }
    }

    fn set_current(&self, name: &str) {
        self.control.update(|p| {
            if p.current != name {
                p.current = name.to_owned();
            }
        });
    }

    fn file_type(&self, file: &gio::File) -> gio::FileType {
        file.query_file_type(NOFOLLOW, self.cancellable())
    }

    fn exists(&self, file: &gio::File) -> bool {
        self.file_type(file) != gio::FileType::Unknown
    }

    /// Counts `node` as processed, e.g. when skipped.
    fn mark_done(&self, node: &Node) {
        let bytes = if self.count_bytes {
            node.total_size()
        } else {
            0
        };
        self.control.update(|p| {
            p.files_done += node.count();
            p.bytes_done += bytes;
        });
    }

    /// Asks what to do about an error. Records skipped items.
    fn ask_error(
        &mut self,
        file: &gio::File,
        err: &glib::Error,
        kind: ErrorKind,
        fatal: bool,
    ) -> Result<ErrorChoice, Abort> {
        let name = display_name(file);
        let remembered = self.error_all.filter(|choice| {
            *choice == ErrorChoice::Skip
                || (*choice == ErrorChoice::DeletePermanently
                    && kind == ErrorKind::TrashUnsupported)
        });
        let choice = match remembered {
            Some(choice) => choice,
            None => {
                let response = self.ui.error(&OpError {
                    kind,
                    file_name: name.clone(),
                    message: err.message().to_owned(),
                    fatal,
                });
                if response.apply_to_all
                    && matches!(
                        response.choice,
                        ErrorChoice::Skip | ErrorChoice::DeletePermanently
                    )
                {
                    self.error_all = Some(response.choice);
                }
                response.choice
            }
        };
        let choice = match choice {
            ErrorChoice::Retry if fatal => ErrorChoice::Skip,
            ErrorChoice::DeletePermanently if kind != ErrorKind::TrashUnsupported => {
                ErrorChoice::Skip
            }
            choice => choice,
        };
        match choice {
            ErrorChoice::Cancel => {
                self.control.cancel();
                return Err(Abort);
            }
            ErrorChoice::Skip => {
                self.errors.push(format!("{name}: {}", err.message()));
                self.skipped += 1;
            }
            ErrorChoice::Retry | ErrorChoice::DeletePermanently => {}
        }
        Ok(choice)
    }

    /// Reports a problem that retrying can't fix; the item is skipped.
    fn fail(&mut self, file: &gio::File, message: &str) -> Result<(), Abort> {
        let err = glib::Error::new(gio::IOErrorEnum::Failed, message);
        self.ask_error(file, &err, ErrorKind::Generic, true)
            .map(|_| ())
    }

    /// Runs `f`, asking the user on failure: Retry runs it again, Skip
    /// returns `Ok(None)`, Cancel aborts.
    fn attempt<T>(
        &mut self,
        file: &gio::File,
        mut f: impl FnMut() -> Result<T, glib::Error>,
    ) -> Step<T> {
        loop {
            self.check()?;
            match f() {
                Ok(value) => return Ok(Some(value)),
                Err(err) => {
                    if self.control.is_cancelled() || err.matches(gio::IOErrorEnum::Cancelled) {
                        return Err(Abort);
                    }
                    match self.ask_error(file, &err, ErrorKind::Generic, false)? {
                        ErrorChoice::Retry => continue,
                        _ => return Ok(None),
                    }
                }
            }
        }
    }

    /// Creates the parent folders of `target` if needed (undoing a move
    /// whose source folder has since been removed).
    fn ensure_parent(&self, target: &gio::File) {
        if let Some(parent) = target.parent()
            && !self.exists(&parent)
            && let Err(err) = parent.make_directory_with_parents(self.cancellable())
            && !err.matches(gio::IOErrorEnum::Exists)
        {
            tracing::debug!("cannot create {}: {err}", parent.uri());
        }
    }

    /// Decides where `source` goes when `target` may already exist.
    fn resolve(
        &mut self,
        source: &gio::File,
        source_is_dir: bool,
        target: &gio::File,
        merge: MergePolicy,
    ) -> Result<Target, Abort> {
        let existing = self.file_type(target);
        if existing == gio::FileType::Unknown {
            return Ok(Target::Write {
                file: target.clone(),
                overwrite: false,
            });
        }
        let target_is_dir = existing == gio::FileType::Directory;
        let is_merge = source_is_dir && target_is_dir && merge != MergePolicy::Never;
        if is_merge && merge == MergePolicy::Silent {
            return Ok(Target::Merge(target.clone()));
        }
        let can_replace = !source_is_dir && !target_is_dir || is_merge;

        let remembered = self
            .conflict_all
            .filter(|choice| *choice != ConflictChoice::Replace || can_replace);
        let choice = match remembered {
            Some(choice) => choice,
            None => {
                let conflict = Conflict {
                    source: summary(source),
                    target: summary(target),
                    folder_name: target
                        .parent()
                        .map(|p| display_name(&p))
                        .unwrap_or_default(),
                    is_merge,
                    can_replace,
                };
                let response = self.ui.conflict(&conflict);
                if response.apply_to_all && response.choice != ConflictChoice::Cancel {
                    self.conflict_all = Some(response.choice);
                }
                response.choice
            }
        };
        Ok(match choice {
            ConflictChoice::Replace if can_replace => {
                if is_merge {
                    Target::Merge(target.clone())
                } else {
                    Target::Write {
                        file: target.clone(),
                        overwrite: true,
                    }
                }
            }
            ConflictChoice::Replace | ConflictChoice::KeepBoth => Target::Write {
                file: unique_sibling(target, source_is_dir, UniqueStyle::Numbered),
                overwrite: false,
            },
            ConflictChoice::Skip => {
                self.skipped += 1;
                Target::Skip
            }
            ConflictChoice::Cancel => {
                self.control.cancel();
                return Err(Abort);
            }
        })
    }

    // ── Scanning ───────────────────────────────────────────────────────────

    /// Lists `file` recursively, counting totals for the progress.
    fn scan(&mut self, file: &gio::File) -> Step<Node> {
        let cancellable = self.control.cancellable().clone();
        let Some(info) = self.attempt(file, || {
            file.query_info(SCAN_ATTRIBUTES, NOFOLLOW, Some(&cancellable))
        })?
        else {
            return Ok(None);
        };
        let mut node = Node::from_info(file.clone(), &info);
        self.count_scanned(&node);
        if node.is_dir() {
            self.scan_children(&mut node)?;
        }
        Ok(Some(node))
    }

    fn scan_children(&mut self, node: &mut Node) -> Result<(), Abort> {
        let cancellable = self.control.cancellable().clone();
        let dir = node.file.clone();
        let Some(enumerator) = self.attempt(&dir, || {
            dir.enumerate_children(SCAN_ATTRIBUTES, NOFOLLOW, Some(&cancellable))
        })?
        else {
            node.incomplete = true;
            return Ok(());
        };
        loop {
            match self.attempt(&dir, || enumerator.next_file(Some(&cancellable)))? {
                Some(Some(info)) => {
                    let mut child = Node::from_info(enumerator.child(&info), &info);
                    self.count_scanned(&child);
                    if child.is_dir() {
                        self.scan_children(&mut child)?;
                    }
                    node.children.push(child);
                }
                Some(None) => break,
                None => {
                    node.incomplete = true;
                    break;
                }
            }
        }
        Ok(())
    }

    fn count_scanned(&self, node: &Node) {
        let bytes = if self.count_bytes && !node.is_dir() {
            node.size
        } else {
            0
        };
        self.control.update(|p| {
            p.files_total += 1;
            p.bytes_total += bytes;
        });
    }

    // ── Copy & move ────────────────────────────────────────────────────────

    fn run_transfer(&mut self, pairs: &[(gio::File, gio::File)], mode: Mode) -> Result<(), Abort> {
        // Resolve the top-level targets; same-device moves are done here,
        // without scanning.
        let mut pending = Vec::new();
        for (source, target) in pairs {
            self.check()?;
            self.set_current(&display_name(source));
            let source_is_dir = self.file_type(source) == gio::FileType::Directory;
            let mut target = target.clone();
            if source.equal(&target) {
                if mode == Mode::Move {
                    continue;
                }
                // Pasting into the same folder makes a copy next to it.
                target = unique_sibling(&target, source_is_dir, UniqueStyle::Copy);
            } else if target.has_prefix(source) {
                self.fail(
                    source,
                    &gettext("A folder can't be copied or moved into itself"),
                )?;
                continue;
            }
            self.ensure_parent(&target);
            let resolved = self.resolve(source, source_is_dir, &target, MergePolicy::Ask)?;
            if let (Mode::Move, Target::Write { file, overwrite }) = (mode, &resolved) {
                match self.try_rename(source, file, *overwrite)? {
                    RenameResult::Renamed => {
                        self.moved.push((source.clone(), file.clone()));
                        self.succeeded += 1;
                        self.control.update(|p| {
                            p.files_total += 1;
                            p.files_done += 1;
                        });
                        continue;
                    }
                    RenameResult::Skipped => continue,
                    RenameResult::Fallback => {}
                }
            }
            if !matches!(resolved, Target::Skip) {
                pending.push((source.clone(), resolved));
            }
        }
        if pending.is_empty() {
            return Ok(());
        }

        self.control.update(|p| p.scanning = true);
        let mut nodes = Vec::new();
        for (source, target) in pending {
            if let Some(node) = self.scan(&source)? {
                nodes.push((node, target));
            }
        }
        self.control.update(|p| p.scanning = false);

        for (node, target) in nodes {
            // A merge may be on the same device: try renaming its children.
            let try_rename = mode == Mode::Move && matches!(target, Target::Merge(_));
            if self.transfer(&node, target, mode, true, try_rename)? {
                self.succeeded += 1;
            }
        }
        Ok(())
    }

    /// Moves with a plain rename when the file system allows it.
    fn try_rename(
        &mut self,
        source: &gio::File,
        target: &gio::File,
        overwrite: bool,
    ) -> Result<RenameResult, Abort> {
        let mut flags = gio::FileCopyFlags::NOFOLLOW_SYMLINKS
            | gio::FileCopyFlags::ALL_METADATA
            | gio::FileCopyFlags::NO_FALLBACK_FOR_MOVE;
        if overwrite {
            flags |= gio::FileCopyFlags::OVERWRITE;
        }
        loop {
            self.check()?;
            match source.move_(target, flags, self.cancellable(), None) {
                Ok(()) => return Ok(RenameResult::Renamed),
                Err(err)
                    if err.matches(gio::IOErrorEnum::NotSupported)
                        || err.matches(gio::IOErrorEnum::WouldRecurse)
                        || err.matches(gio::IOErrorEnum::WouldMerge) =>
                {
                    return Ok(RenameResult::Fallback);
                }
                Err(err) => {
                    if self.control.is_cancelled() {
                        return Err(Abort);
                    }
                    match self.ask_error(source, &err, ErrorKind::Generic, false)? {
                        ErrorChoice::Retry => continue,
                        _ => return Ok(RenameResult::Skipped),
                    }
                }
            }
        }
    }

    /// Copies (or moves) `node` to `target`. `track`: record the result as
    /// a top-level item. Returns whether everything was transferred.
    fn transfer(
        &mut self,
        node: &Node,
        target: Target,
        mode: Mode,
        track: bool,
        try_rename: bool,
    ) -> Result<bool, Abort> {
        self.check()?;
        self.set_current(&node.display_name);
        let (file, overwrite, merging) = match target {
            Target::Skip => {
                self.mark_done(node);
                return Ok(false);
            }
            Target::Merge(file) => (file, false, true),
            Target::Write { file, overwrite } => (file, overwrite, false),
        };
        let cancellable = self.control.cancellable().clone();

        if mode == Mode::Move && !merging && try_rename {
            match self.try_rename(&node.file, &file, overwrite)? {
                RenameResult::Renamed => {
                    self.mark_done(node);
                    if track {
                        self.moved.push((node.file.clone(), file));
                    }
                    return Ok(true);
                }
                RenameResult::Skipped => {
                    self.mark_done(node);
                    return Ok(false);
                }
                RenameResult::Fallback => {}
            }
        }

        if node.is_dir() {
            if !merging {
                if self
                    .attempt(&file, || file.make_directory(Some(&cancellable)))?
                    .is_none()
                {
                    self.mark_done(node);
                    return Ok(false);
                }
                if track {
                    self.record_transfer(mode, node, &file);
                }
            }
            let mut complete = !node.incomplete;
            for child in &node.children {
                let child_target = self.resolve(
                    &child.file,
                    child.is_dir(),
                    &file.child(&child.name),
                    MergePolicy::Silent,
                )?;
                complete &=
                    self.transfer(child, child_target, mode, merging && track, try_rename)?;
            }
            if !merging {
                // After the children, so a read-only folder can be filled.
                let flags =
                    gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA;
                if let Err(err) = node.file.copy_attributes(&file, flags, Some(&cancellable)) {
                    tracing::debug!("cannot copy attributes to {}: {err}", file.uri());
                }
            }
            self.control.update(|p| p.files_done += 1);
            if mode == Mode::Move && complete {
                complete = self
                    .attempt(&node.file, || node.file.delete(Some(&cancellable)))?
                    .is_some();
            }
            return Ok(complete);
        }

        if !self.copy_file(node, &file, overwrite)? {
            return Ok(false);
        }
        if track {
            self.record_transfer(mode, node, &file);
        }
        if mode == Mode::Move {
            return Ok(self
                .attempt(&node.file, || node.file.delete(Some(&cancellable)))?
                .is_some());
        }
        Ok(true)
    }

    fn record_transfer(&mut self, mode: Mode, node: &Node, target: &gio::File) {
        match mode {
            Mode::Copy => self.created.push(target.clone()),
            Mode::Move => self.moved.push((node.file.clone(), target.clone())),
        }
    }

    /// Copies one file (or symlink). Returns false if it was skipped.
    fn copy_file(
        &mut self,
        node: &Node,
        target: &gio::File,
        overwrite: bool,
    ) -> Result<bool, Abort> {
        let control = self.control;
        // Local files are written under a temporary name first.
        let part = (target.is_native() && node.file_type != gio::FileType::SymbolicLink)
            .then(|| {
                let name = naming::part_name(&target.basename()?.to_string_lossy())?;
                Some(target.parent()?.child(name))
            })
            .flatten();
        let base = control.progress().bytes_done;

        let copied = self.attempt(&node.file, || {
            control.update(|p| p.bytes_done = base);
            let destination = part.as_ref().unwrap_or(target);
            let mut flags =
                gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA;
            if part.is_some() || overwrite {
                flags |= gio::FileCopyFlags::OVERWRITE;
            }
            let mut on_progress = |current: i64, _total: i64| {
                control.update(|p| p.bytes_done = base + current.max(0) as u64);
                // Pausing blocks right here, in the middle of the file.
                control.wait_while_paused();
            };
            let mut result = node.file.copy(
                destination,
                flags,
                Some(control.cancellable()),
                Some(&mut on_progress),
            );
            if result.is_ok()
                && let Some(part) = &part
            {
                let mut flags = gio::FileCopyFlags::NOFOLLOW_SYMLINKS;
                if overwrite {
                    flags |= gio::FileCopyFlags::OVERWRITE;
                }
                result = part.move_(target, flags, gio::Cancellable::NONE, None);
            }
            if let Err(err) = &result {
                // Never leave a half-written file behind.
                if let Some(part) = &part {
                    let _ = part.delete(gio::Cancellable::NONE);
                } else if !overwrite && !err.matches(gio::IOErrorEnum::Exists) {
                    let _ = target.delete(gio::Cancellable::NONE);
                }
            }
            result
        })?;

        control.update(|p| {
            p.bytes_done = base + node.size;
            p.files_done += 1;
        });
        Ok(copied.is_some())
    }

    // ── Links ──────────────────────────────────────────────────────────────

    fn run_link(&mut self, pairs: &[(gio::File, gio::File)]) -> Result<(), Abort> {
        self.control.update(|p| p.files_total = pairs.len() as u64);
        let cancellable = self.control.cancellable().clone();
        for (source, target) in pairs {
            self.check()?;
            let Some(path) = source.path() else {
                self.fail(source, &gettext("Links can only point to local files"))?;
                continue;
            };
            let resolved = self.resolve(source, false, target, MergePolicy::Never)?;
            if let Target::Write { file, overwrite } = resolved {
                let created = self.attempt(&file, || {
                    if overwrite {
                        file.delete(Some(&cancellable))?;
                    }
                    file.make_symbolic_link(&path, Some(&cancellable))
                })?;
                if created.is_some() {
                    self.created.push(file);
                    self.succeeded += 1;
                }
            }
            self.control.update(|p| p.files_done += 1);
        }
        Ok(())
    }

    // ── Trash & delete ─────────────────────────────────────────────────────

    fn run_trash(&mut self, files: &[gio::File]) -> Result<(), Abort> {
        self.control.update(|p| p.files_total = files.len() as u64);
        for file in files {
            self.check()?;
            self.set_current(&display_name(file));
            loop {
                let err = match file.trash(self.cancellable()) {
                    Ok(()) => {
                        self.trashed.push(file.clone());
                        self.succeeded += 1;
                        break;
                    }
                    Err(err) => err,
                };
                if self.control.is_cancelled() {
                    return Err(Abort);
                }
                let kind = if err.matches(gio::IOErrorEnum::NotSupported) {
                    ErrorKind::TrashUnsupported
                } else {
                    ErrorKind::Generic
                };
                match self.ask_error(file, &err, kind, false)? {
                    ErrorChoice::Retry => continue,
                    ErrorChoice::DeletePermanently => {
                        self.count_bytes = false;
                        if self.delete_tree(file)? {
                            self.succeeded += 1;
                        }
                        break;
                    }
                    _ => break,
                }
            }
            self.control.update(|p| p.files_done += 1);
        }
        Ok(())
    }

    fn run_delete(&mut self, files: &[gio::File]) -> Result<(), Abort> {
        self.count_bytes = false;
        self.control.update(|p| p.scanning = true);
        let mut nodes = Vec::new();
        for file in files {
            self.check()?;
            if let Some(node) = self.scan(file)? {
                nodes.push(node);
            }
        }
        self.control.update(|p| p.scanning = false);
        for node in &nodes {
            if self.delete_node(node)? {
                self.succeeded += 1;
            }
        }
        Ok(())
    }

    fn run_empty_trash(&mut self) -> Result<(), Abort> {
        let trash = gio::File::for_uri(TRASH_URI);
        let cancellable = self.control.cancellable().clone();
        let list = || trash.enumerate_children("standard::name", NOFOLLOW, Some(&cancellable));
        let enumerator = match list() {
            Ok(enumerator) => enumerator,
            // Without GVfs, empty the home trash directly.
            Err(err) if err.matches(gio::IOErrorEnum::NotSupported) => {
                return self.run_delete(&trash::home_trash_contents());
            }
            Err(_) => match self.attempt(&trash, list)? {
                Some(enumerator) => enumerator,
                None => return Ok(()),
            },
        };
        let mut items = Vec::new();
        while let Some(Some(info)) =
            self.attempt(&trash, || enumerator.next_file(Some(&cancellable)))?
        {
            items.push(enumerator.child(&info));
        }
        self.run_delete(&items)
    }

    /// Deletes `file` and everything in it, after counting.
    fn delete_tree(&mut self, file: &gio::File) -> Result<bool, Abort> {
        match self.scan(file)? {
            Some(node) => self.delete_node(&node),
            None => Ok(false),
        }
    }

    fn delete_node(&mut self, node: &Node) -> Result<bool, Abort> {
        self.check()?;
        self.set_current(&node.display_name);
        let mut complete = !node.incomplete;
        for child in &node.children {
            complete &= self.delete_node(child)?;
        }
        let deleted = complete && {
            let cancellable = self.control.cancellable().clone();
            self.attempt(&node.file, || node.file.delete(Some(&cancellable)))?
                .is_some()
        };
        self.control.update(|p| p.files_done += 1);
        Ok(deleted)
    }

    // ── Restore ────────────────────────────────────────────────────────────

    fn run_restore(&mut self, files: &[gio::File]) -> Result<(), Abort> {
        self.control.update(|p| p.files_total = files.len() as u64);
        let cancellable = self.control.cancellable().clone();
        // Trash items by original path (through GVfs), built on first use.
        let mut index: Option<HashMap<PathBuf, gio::File>> = None;

        for file in files {
            self.check()?;
            // The `.trashinfo` file to remove once a local item is restored.
            let mut info_file = None;
            let (item, original) = if file.uri_scheme().as_deref() == Some("trash") {
                let Some(info) = self.attempt(file, || {
                    file.query_info(TRASH_ATTRIBUTES, NOFOLLOW, Some(&cancellable))
                })?
                else {
                    continue;
                };
                match info.attribute_byte_string(gio::FILE_ATTRIBUTE_TRASH_ORIG_PATH) {
                    Some(path) => (file.clone(), gio::File::for_path(path.as_str())),
                    None => {
                        self.fail(
                            file,
                            &gettext("Only items at the top of the Trash can be restored"),
                        )?;
                        continue;
                    }
                }
            } else if let Some(found) = file.path().and_then(|path| trash::find_trashed(&path)) {
                // Local trash: found directly, no daemon needed.
                info_file = Some(found.info);
                (gio::File::for_path(found.file), file.clone())
            } else {
                if index.is_none() {
                    index = Some(self.trash_index());
                }
                let item = file
                    .path()
                    .and_then(|path| index.as_mut().unwrap().remove(&path));
                match item {
                    Some(item) => (item, file.clone()),
                    None => {
                        self.fail(file, &gettext("The item is no longer in the Trash"))?;
                        continue;
                    }
                }
            };

            self.set_current(&display_name(&original));
            self.ensure_parent(&original);
            let is_dir = self.file_type(&item) == gio::FileType::Directory;
            if let Target::Write {
                file: target,
                overwrite,
            } = self.resolve(&item, is_dir, &original, MergePolicy::Never)?
            {
                let mut flags =
                    gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA;
                if overwrite {
                    flags |= gio::FileCopyFlags::OVERWRITE;
                }
                let restored = self.attempt(&item, || {
                    item.move_(&target, flags, Some(&cancellable), None)
                })?;
                if restored.is_some() {
                    if let Some(info) = info_file
                        && let Err(err) = std::fs::remove_file(&info)
                    {
                        tracing::warn!("cannot remove {}: {err}", info.display());
                    }
                    self.created.push(target);
                    self.succeeded += 1;
                }
            }
            self.control.update(|p| p.files_done += 1);
        }
        Ok(())
    }

    /// Maps original paths to items of `trash:///`, keeping the most
    /// recently deleted item for each path. Empty without GVfs: items not
    /// found are then reported one by one.
    fn trash_index(&mut self) -> HashMap<PathBuf, gio::File> {
        let trash = gio::File::for_uri(TRASH_URI);
        let cancellable = self.control.cancellable();
        let mut latest: HashMap<PathBuf, (String, gio::File)> = HashMap::new();
        let enumerator =
            match trash.enumerate_children(TRASH_ATTRIBUTES, NOFOLLOW, Some(cancellable)) {
                Ok(enumerator) => enumerator,
                Err(err) => {
                    tracing::debug!("cannot list the trash: {err}");
                    return HashMap::new();
                }
            };
        while let Ok(Some(info)) = enumerator.next_file(Some(cancellable)) {
            let Some(path) = info.attribute_byte_string(gio::FILE_ATTRIBUTE_TRASH_ORIG_PATH) else {
                continue;
            };
            // ISO 8601 dates compare correctly as strings.
            let date = info
                .attribute_string(gio::FILE_ATTRIBUTE_TRASH_DELETION_DATE)
                .map(String::from)
                .unwrap_or_default();
            let item = enumerator.child(&info);
            let path = PathBuf::from(path.as_str());
            if latest.get(&path).is_none_or(|(d, _)| *d <= date) {
                latest.insert(path, (date, item));
            }
        }
        latest
            .into_iter()
            .map(|(path, (_, item))| (path, item))
            .collect()
    }

    // ── Single items ───────────────────────────────────────────────────────

    fn run_rename(&mut self, file: &gio::File, name: &str) -> Result<(), Abort> {
        self.control.update(|p| p.files_total = 1);
        self.set_current(&display_name(file));
        let cancellable = self.control.cancellable().clone();
        if let Some(renamed) =
            self.attempt(file, || file.set_display_name(name, Some(&cancellable)))?
        {
            if !renamed.equal(file) {
                self.moved.push((file.clone(), renamed.clone()));
            }
            self.created.push(renamed);
            self.succeeded += 1;
        }
        self.control.update(|p| p.files_done = 1);
        Ok(())
    }

    fn run_create(
        &mut self,
        parent: &gio::File,
        name: &str,
        template: Option<&gio::File>,
        folder: bool,
    ) -> Result<(), Abort> {
        self.control.update(|p| p.files_total = 1);
        let target = unique_sibling(&parent.child(name), folder, UniqueStyle::Numbered);
        let cancellable = self.control.cancellable().clone();
        let created = self.attempt(&target, || {
            if folder {
                target.make_directory(Some(&cancellable))
            } else if let Some(template) = template {
                // Without ALL_METADATA: the new file gets the current time.
                template.copy(&target, gio::FileCopyFlags::NONE, Some(&cancellable), None)
            } else {
                target
                    .create(gio::FileCreateFlags::NONE, Some(&cancellable))?
                    .close(Some(&cancellable))
            }
        })?;
        if created.is_some() {
            self.created.push(target);
            self.succeeded += 1;
        }
        self.control.update(|p| p.files_done = 1);
        Ok(())
    }
}

/// `target`, or a numbered / "copy" variant of it that doesn't exist.
fn unique_sibling(target: &gio::File, is_dir: bool, style: UniqueStyle) -> gio::File {
    let (Some(parent), Some(name)) = (target.parent(), target.basename()) else {
        return target.clone();
    };
    let name = name.to_string_lossy();
    let unique = naming::unique_name(&name, is_dir, style, |candidate| {
        parent
            .child(candidate)
            .query_file_type(NOFOLLOW, gio::Cancellable::NONE)
            != gio::FileType::Unknown
    });
    parent.child(unique)
}

/// The name shown for `file` in questions and progress.
pub fn display_name(file: &gio::File) -> String {
    file.query_info(
        gio::FILE_ATTRIBUTE_STANDARD_DISPLAY_NAME,
        gio::FileQueryInfoFlags::NONE,
        gio::Cancellable::NONE,
    )
    .map(|info| info.display_name().into())
    .unwrap_or_else(|_| {
        file.basename()
            .map(|b| b.to_string_lossy().into_owned())
            .unwrap_or_else(|| file.parse_name().into())
    })
}

fn summary(file: &gio::File) -> FileSummary {
    let mut summary = FileSummary {
        uri: file.uri().into(),
        name: display_name(file),
        ..Default::default()
    };
    if let Ok(info) = file.query_info(SUMMARY_ATTRIBUTES, NOFOLLOW, gio::Cancellable::NONE) {
        summary.name = info.display_name().into();
        summary.size = info.size().max(0) as u64;
        summary.modified = info
            .attribute_uint64(gio::FILE_ATTRIBUTE_TIME_MODIFIED)
            .try_into()
            .unwrap_or(0);
        summary.is_dir = info.file_type() == gio::FileType::Directory;
        summary.content_type = info.content_type().map(Into::into);
        summary.thumbnail = info
            .attribute_byte_string(gio::FILE_ATTRIBUTE_THUMBNAIL_PATH)
            .map(|p| PathBuf::from(p.as_str()));
    }
    summary
}
