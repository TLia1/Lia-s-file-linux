//! What a file operation does, how it asks the user for decisions, and how
//! its progress is shared with the UI.
//!
//! Everything here is `Send`: operations run on a worker thread (see
//! [`super::engine`]) while the UI thread watches the [`Control`].

use std::path::PathBuf;
use std::sync::{Condvar, Mutex, OnceLock};

use gtk::{gio, prelude::*};
use serde::{Deserialize, Serialize};

use crate::services::archives::ArchiveFormat;
use crate::services::recovery::RecoveryLog;
use crate::services::undo::UndoRecord;

/// A file operation. Transfers take `(source, target)` pairs where the
/// target is the exact path to create; conflicts may change it.
///
/// Serializable (files as URIs) so that the undo history survives a
/// restart.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Operation {
    Copy {
        #[serde(with = "uris::pairs")]
        pairs: Vec<(gio::File, gio::File)>,
    },
    Move {
        #[serde(with = "uris::pairs")]
        pairs: Vec<(gio::File, gio::File)>,
    },
    /// Creates symbolic links: each target points to its source.
    Link {
        #[serde(with = "uris::pairs")]
        pairs: Vec<(gio::File, gio::File)>,
    },
    /// Copies each file next to itself as "name (copy).ext".
    Duplicate {
        #[serde(with = "uris::files")]
        files: Vec<gio::File>,
    },
    Trash {
        #[serde(with = "uris::files")]
        files: Vec<gio::File>,
    },
    /// Permanent, recursive deletion.
    Delete {
        #[serde(with = "uris::files")]
        files: Vec<gio::File>,
    },
    /// Moves files out of the trash. Accepts `trash:///` items or the
    /// original locations of trashed files.
    Restore {
        #[serde(with = "uris::files")]
        files: Vec<gio::File>,
    },
    EmptyTrash,
    Rename {
        #[serde(with = "uris::file")]
        file: gio::File,
        name: String,
    },
    /// Creates `name` in `parent`, numbered if it exists.
    CreateFolder {
        #[serde(with = "uris::file")]
        parent: gio::File,
        name: String,
    },
    /// Creates an empty file, or a copy of `template`.
    CreateFile {
        #[serde(with = "uris::file")]
        parent: gio::File,
        name: String,
        #[serde(with = "uris::option")]
        template: Option<gio::File>,
    },
    /// Renames several files at once; names may be swapped between them.
    BatchRename {
        #[serde(with = "uris::renames")]
        renames: Vec<(gio::File, String)>,
    },
    /// Extracts each archive into `destination`: its single top-level item
    /// directly, or several items into a folder named after the archive.
    Extract {
        #[serde(with = "uris::files")]
        archives: Vec<gio::File>,
        #[serde(with = "uris::file")]
        destination: gio::File,
    },
    /// Creates the archive `target` (renamed if it exists) of `sources`.
    Compress {
        #[serde(with = "uris::files")]
        sources: Vec<gio::File>,
        #[serde(with = "uris::file")]
        target: gio::File,
        format: ArchiveFormat,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Copy,
    Move,
    Link,
    Duplicate,
    Trash,
    Delete,
    Restore,
    EmptyTrash,
    Rename,
    CreateFolder,
    CreateFile,
    BatchRename,
    Extract,
    Compress,
}

impl Operation {
    /// Copies `sources` into the folder `destination`.
    pub fn copy_into(sources: &[gio::File], destination: &gio::File) -> Self {
        Self::Copy {
            pairs: pairs_into(sources, destination),
        }
    }

    /// Moves `sources` into the folder `destination`.
    pub fn move_into(sources: &[gio::File], destination: &gio::File) -> Self {
        Self::Move {
            pairs: pairs_into(sources, destination),
        }
    }

    /// Links `sources` into the folder `destination`.
    pub fn link_into(sources: &[gio::File], destination: &gio::File) -> Self {
        Self::Link {
            pairs: pairs_into(sources, destination),
        }
    }

    pub fn kind(&self) -> OpKind {
        match self {
            Self::Copy { .. } => OpKind::Copy,
            Self::Move { .. } => OpKind::Move,
            Self::Link { .. } => OpKind::Link,
            Self::Duplicate { .. } => OpKind::Duplicate,
            Self::Trash { .. } => OpKind::Trash,
            Self::Delete { .. } => OpKind::Delete,
            Self::Restore { .. } => OpKind::Restore,
            Self::EmptyTrash => OpKind::EmptyTrash,
            Self::Rename { .. } => OpKind::Rename,
            Self::CreateFolder { .. } => OpKind::CreateFolder,
            Self::CreateFile { .. } => OpKind::CreateFile,
            Self::BatchRename { .. } => OpKind::BatchRename,
            Self::Extract { .. } => OpKind::Extract,
            Self::Compress { .. } => OpKind::Compress,
        }
    }

    /// The files the operation reads from (or acts on).
    pub fn sources(&self) -> Vec<gio::File> {
        match self {
            Self::Copy { pairs } | Self::Move { pairs } | Self::Link { pairs } => {
                pairs.iter().map(|(s, _)| s.clone()).collect()
            }
            Self::Duplicate { files }
            | Self::Trash { files }
            | Self::Delete { files }
            | Self::Restore { files } => files.clone(),
            Self::Rename { file, .. } => vec![file.clone()],
            Self::BatchRename { renames } => renames.iter().map(|(f, _)| f.clone()).collect(),
            Self::Extract { archives, .. } => archives.clone(),
            Self::Compress { sources, .. } => sources.clone(),
            Self::EmptyTrash | Self::CreateFolder { .. } | Self::CreateFile { .. } => Vec::new(),
        }
    }

    /// The folder written to, when there is a single one.
    pub fn destination(&self) -> Option<gio::File> {
        match self {
            Self::Copy { pairs } | Self::Move { pairs } | Self::Link { pairs } => {
                let parent = pairs.first()?.1.parent()?;
                pairs
                    .iter()
                    .all(|(_, t)| t.has_parent(Some(&parent)))
                    .then_some(parent)
            }
            Self::CreateFolder { parent, .. } | Self::CreateFile { parent, .. } => {
                Some(parent.clone())
            }
            Self::Duplicate { files } => {
                let parent = files.first()?.parent()?;
                files
                    .iter()
                    .all(|f| f.has_parent(Some(&parent)))
                    .then_some(parent)
            }
            Self::Rename { file, .. } => file.parent(),
            Self::BatchRename { renames } => {
                let parent = renames.first()?.0.parent()?;
                renames
                    .iter()
                    .all(|(f, _)| f.has_parent(Some(&parent)))
                    .then_some(parent)
            }
            Self::Extract { destination, .. } => Some(destination.clone()),
            Self::Compress { target, .. } => target.parent(),
            _ => None,
        }
    }

    /// Number of top-level items, for messages.
    pub fn item_count(&self) -> usize {
        match self {
            Self::Copy { pairs } | Self::Move { pairs } | Self::Link { pairs } => pairs.len(),
            Self::Duplicate { files }
            | Self::Trash { files }
            | Self::Delete { files }
            | Self::Restore { files } => files.len(),
            Self::EmptyTrash => 0,
            Self::BatchRename { renames } => renames.len(),
            Self::Extract { archives, .. } => archives.len(),
            Self::Compress { sources, .. } => sources.len(),
            Self::Rename { .. } | Self::CreateFolder { .. } | Self::CreateFile { .. } => 1,
        }
    }

    /// Small, metadata-only operations that don't wait in the queue or
    /// show up in the operations popover.
    pub fn is_quick(&self) -> bool {
        matches!(
            self.kind(),
            OpKind::Rename
                | OpKind::BatchRename
                | OpKind::CreateFolder
                | OpKind::CreateFile
                | OpKind::Link
        )
    }
}

fn pairs_into(sources: &[gio::File], destination: &gio::File) -> Vec<(gio::File, gio::File)> {
    sources
        .iter()
        .filter_map(|source| {
            let name = source.basename()?;
            Some((source.clone(), destination.child(name)))
        })
        .collect()
}

/// Serde helpers storing `gio::File`s as URIs.
mod uris {
    use gtk::{gio, prelude::*};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    /// A file, serialized.
    struct Out<'a>(&'a gio::File);

    impl Serialize for Out<'_> {
        fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
            s.serialize_str(&self.0.uri())
        }
    }

    /// A file, deserialized.
    struct In(gio::File);

    impl<'de> Deserialize<'de> for In {
        fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
            String::deserialize(d).map(|uri| Self(gio::File::for_uri(&uri)))
        }
    }

    pub mod file {
        use super::*;

        pub fn serialize<S: Serializer>(file: &gio::File, s: S) -> Result<S::Ok, S::Error> {
            Out(file).serialize(s)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<gio::File, D::Error> {
            In::deserialize(d).map(|f| f.0)
        }
    }

    pub mod option {
        use super::*;

        pub fn serialize<S: Serializer>(file: &Option<gio::File>, s: S) -> Result<S::Ok, S::Error> {
            file.as_ref().map(Out).serialize(s)
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<gio::File>, D::Error> {
            Option::<In>::deserialize(d).map(|f| f.map(|f| f.0))
        }
    }

    pub mod files {
        use super::*;

        pub fn serialize<S: Serializer>(files: &[gio::File], s: S) -> Result<S::Ok, S::Error> {
            s.collect_seq(files.iter().map(Out))
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<gio::File>, D::Error> {
            Vec::<In>::deserialize(d).map(|v| v.into_iter().map(|f| f.0).collect())
        }
    }

    pub mod pairs {
        use super::*;

        pub fn serialize<S: Serializer>(
            pairs: &[(gio::File, gio::File)],
            s: S,
        ) -> Result<S::Ok, S::Error> {
            s.collect_seq(pairs.iter().map(|(a, b)| (Out(a), Out(b))))
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Vec<(gio::File, gio::File)>, D::Error> {
            Vec::<(In, In)>::deserialize(d)
                .map(|v| v.into_iter().map(|(a, b)| (a.0, b.0)).collect())
        }
    }

    pub mod renames {
        use super::*;

        pub fn serialize<S: Serializer>(
            renames: &[(gio::File, String)],
            s: S,
        ) -> Result<S::Ok, S::Error> {
            s.collect_seq(renames.iter().map(|(f, name)| (Out(f), name)))
        }

        pub fn deserialize<'de, D: Deserializer<'de>>(
            d: D,
        ) -> Result<Vec<(gio::File, String)>, D::Error> {
            Vec::<(In, String)>::deserialize(d)
                .map(|v| v.into_iter().map(|(f, name)| (f.0, name)).collect())
        }
    }
}

// ── Questions asked to the user ────────────────────────────────────────────

/// What the conflict dialog shows about one side of a conflict.
#[derive(Clone, Debug, Default)]
pub struct FileSummary {
    pub uri: String,
    pub name: String,
    pub size: u64,
    /// UNIX timestamp, 0 if unknown.
    pub modified: i64,
    pub is_dir: bool,
    pub content_type: Option<String>,
    /// A cached freedesktop thumbnail, if one exists.
    pub thumbnail: Option<PathBuf>,
}

/// A target that already exists.
#[derive(Clone, Debug)]
pub struct Conflict {
    pub source: FileSummary,
    pub target: FileSummary,
    /// Display name of the folder containing the target.
    pub folder_name: String,
    /// Both are folders: "Replace" merges them.
    pub is_merge: bool,
    /// False when a file and a folder clash: only Skip / Keep both apply.
    pub can_replace: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConflictChoice {
    /// Overwrite the target (or merge, for two folders).
    Replace,
    Skip,
    /// Give the new item a numbered name.
    KeepBoth,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConflictResponse {
    pub choice: ConflictChoice,
    /// Use the same answer for the remaining conflicts of this job.
    pub apply_to_all: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    Generic,
    /// The file system has no trash; offer to delete permanently.
    TrashUnsupported,
}

/// A per-file failure.
#[derive(Clone, Debug)]
pub struct OpError {
    pub kind: ErrorKind,
    pub file_name: String,
    pub message: String,
    /// Retrying can't help (e.g. moving a folder into itself).
    pub fatal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorChoice {
    Retry,
    Skip,
    Cancel,
    /// Only for [`ErrorKind::TrashUnsupported`].
    DeletePermanently,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErrorResponse {
    pub choice: ErrorChoice,
    /// For Skip and DeletePermanently: don't ask again in this job.
    pub apply_to_all: bool,
}

/// Answers the questions a running operation asks. Called from the worker
/// thread, which blocks until the answer comes back.
pub trait Interaction: Send + Sync {
    fn conflict(&self, conflict: &Conflict) -> ConflictResponse;
    fn error(&self, error: &OpError) -> ErrorResponse;
}

/// Gives the same answer to every question (tests, scripted use).
#[derive(Debug, Clone, Copy)]
pub struct FixedAnswers {
    pub conflict: ConflictChoice,
    pub error: ErrorChoice,
}

impl Interaction for FixedAnswers {
    fn conflict(&self, _: &Conflict) -> ConflictResponse {
        ConflictResponse {
            choice: self.conflict,
            apply_to_all: true,
        }
    }

    fn error(&self, _: &OpError) -> ErrorResponse {
        ErrorResponse {
            choice: self.error,
            apply_to_all: true,
        }
    }
}

// ── Progress & control ─────────────────────────────────────────────────────

/// A snapshot of a job's progress.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    /// Still counting the files to process.
    pub scanning: bool,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub files_total: u64,
    pub files_done: u64,
    /// Display name of the file being processed.
    pub current: String,
}

impl Progress {
    /// Fraction done, or `None` if unknown.
    pub fn fraction(&self) -> Option<f64> {
        if self.scanning {
            None
        } else if self.bytes_total > 0 {
            Some((self.bytes_done as f64 / self.bytes_total as f64).min(1.0))
        } else if self.files_total > 0 {
            Some((self.files_done as f64 / self.files_total as f64).min(1.0))
        } else {
            None
        }
    }
}

/// Shared between the worker running an operation and the UI: progress,
/// pause and cancellation.
#[derive(Default)]
pub struct Control {
    cancellable: gio::Cancellable,
    paused: Mutex<bool>,
    resumed: Condvar,
    progress: Mutex<Progress>,
    /// Temporary items to clean up after a crash (none in tests).
    recovery: OnceLock<RecoveryLog>,
}

impl Control {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancellable(&self) -> &gio::Cancellable {
        &self.cancellable
    }

    pub fn cancel(&self) {
        self.cancellable.cancel();
        // Wake a paused worker so that it notices.
        let _guard = self.paused.lock().unwrap();
        self.resumed.notify_all();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellable.is_cancelled()
    }

    pub fn set_paused(&self, paused: bool) {
        *self.paused.lock().unwrap() = paused;
        self.resumed.notify_all();
    }

    pub fn is_paused(&self) -> bool {
        *self.paused.lock().unwrap()
    }

    /// Blocks the calling (worker) thread while paused.
    pub fn wait_while_paused(&self) {
        let mut paused = self.paused.lock().unwrap();
        while *paused && !self.cancellable.is_cancelled() {
            paused = self.resumed.wait(paused).unwrap();
        }
    }

    pub fn progress(&self) -> Progress {
        self.progress.lock().unwrap().clone()
    }

    pub fn update(&self, f: impl FnOnce(&mut Progress)) {
        f(&mut self.progress.lock().unwrap());
    }

    /// Records the temporary items of this job in `log`.
    pub fn set_recovery_log(&self, log: RecoveryLog) {
        let _ = self.recovery.set(log);
    }

    /// `file` is about to be created under a temporary name.
    pub fn note_temporary(&self, file: &gio::File) {
        if let (Some(log), Some(path)) = (self.recovery.get(), file.path()) {
            log.temporary(&path);
        }
    }

    /// `file` is about to be renamed to `temp` for a while.
    pub fn note_moved_aside(&self, temp: &gio::File, file: &gio::File) {
        if let (Some(log), Some(temp), Some(path)) = (self.recovery.get(), temp.path(), file.path())
        {
            log.moved_aside(&temp, &path);
        }
    }

    /// The job ended: its temporary items are gone.
    pub fn close_recovery_log(&self) {
        if let Some(log) = self.recovery.get() {
            log.close();
        }
    }
}

// ── Result ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Finished, possibly with skipped items.
    Done,
    Cancelled,
}

#[derive(Debug)]
pub struct Outcome {
    pub status: Status,
    /// How to revert what was actually done, if anything was.
    pub undo: Option<UndoRecord>,
    /// The items created or moved into place (top level), e.g. to select
    /// them afterwards.
    pub created: Vec<gio::File>,
    /// Messages of the errors that led to skipped items.
    pub errors: Vec<String>,
    /// Number of items skipped (by conflicts or errors).
    pub skipped: u32,
    /// Number of top-level items processed successfully.
    pub succeeded: u32,
}
