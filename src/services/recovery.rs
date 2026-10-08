//! Cleaning up after a crash.
//!
//! Operations put things under temporary names while they work: copies are
//! written to `.name.part`, archives are extracted into a hidden folder,
//! batch renames move files aside as `.name.renaming`. They tidy up when
//! they finish or are cancelled, but not if the app is killed or crashes.
//!
//! So each job keeps a [`RecoveryLog`]: before creating a temporary item,
//! the engine appends its path to a log file in
//! `$XDG_STATE_HOME/lia-files/recovery/`, which is removed when the job
//! ends. A log still there at the next start belongs to a job that never
//! finished, and [`recover`] removes the temporary items it lists and
//! gives moved-aside files their names back. Only paths the log names are
//! touched, so nothing is guessed from file names alone.

use std::fs::File;
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use gtk::glib;

use crate::services::file_ops::naming::{self, UniqueStyle};

/// Record types. Each record is `kind path \0` or `kind path \0 path \0`
/// (paths can't contain NUL bytes, but may contain anything else).
const TEMPORARY: u8 = b'T';
const MOVED_ASIDE: u8 = b'A';

/// Where the logs of running jobs are kept.
pub fn recovery_dir() -> PathBuf {
    glib::user_state_dir().join("lia-files").join("recovery")
}

/// The temporary items of one job.
pub struct RecoveryLog {
    path: PathBuf,
    /// Opened on the first record: most jobs never create any.
    file: Mutex<Option<File>>,
}

impl RecoveryLog {
    /// A log in `dir`, named after this process and a per-process counter.
    pub fn new(dir: &Path) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        Self {
            path: dir.join(format!("{}-{n}.log", std::process::id())),
            file: Mutex::new(None),
        }
    }

    /// `path` is about to be created and will be gone when the job ends.
    pub fn temporary(&self, path: &Path) {
        self.append(TEMPORARY, &[path]);
    }

    /// `original` is about to be renamed to `temp`, and back when the job
    /// ends.
    pub fn moved_aside(&self, temp: &Path, original: &Path) {
        self.append(MOVED_ASIDE, &[temp, original]);
    }

    /// The job ended and cleaned up: the log is no longer needed.
    pub fn close(&self) {
        if self
            .file
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .is_some()
            && let Err(err) = std::fs::remove_file(&self.path)
        {
            tracing::warn!("cannot remove {}: {err}", self.path.display());
        }
    }

    fn append(&self, kind: u8, paths: &[&Path]) {
        let mut record = vec![kind];
        for path in paths {
            record.extend_from_slice(path.as_os_str().as_bytes());
            record.push(0);
        }
        let mut file = self.file.lock().unwrap_or_else(|e| e.into_inner());
        let result = (|| {
            if file.is_none() {
                if let Some(dir) = self.path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                *file = Some(File::create(&self.path)?);
            }
            // Not synced: the log has to survive the app, not the system.
            file.as_mut().unwrap().write_all(&record)
        })();
        if let Err(err) = result {
            // The operation goes on; only the cleanup after a crash is lost.
            tracing::warn!("cannot write {}: {err}", self.path.display());
        }
    }
}

impl Drop for RecoveryLog {
    fn drop(&mut self) {
        self.close();
    }
}

/// What [`recover`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Recovered {
    /// Temporary items removed.
    pub removed: usize,
    /// Moved-aside files given their names back.
    pub restored: usize,
}

/// Cleans up after the jobs whose log is in `dir` and whose process is
/// gone. Blocking: meant for a worker thread at startup.
pub fn recover(dir: &Path) -> Recovered {
    let mut recovered = Recovered::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return recovered;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "log") || is_running(&path) {
            continue;
        }
        match std::fs::read(&path) {
            Ok(log) => replay(&log, &mut recovered),
            Err(err) => {
                tracing::warn!("cannot read {}: {err}", path.display());
                continue;
            }
        }
        if let Err(err) = std::fs::remove_file(&path) {
            tracing::warn!("cannot remove {}: {err}", path.display());
        }
    }
    if recovered != Recovered::default() {
        tracing::info!(
            "cleaned up after interrupted operations: {} temporary items removed, {} files renamed back",
            recovered.removed,
            recovered.restored,
        );
    }
    recovered
}

/// Whether the log belongs to a running instance of the app (another
/// session's, or this one's).
fn is_running(log: &Path) -> bool {
    let Some(pid) = log
        .file_stem()
        .and_then(|stem| stem.to_str()?.split('-').next()?.parse::<u32>().ok())
    else {
        return false;
    };
    if pid == std::process::id() {
        return true;
    }
    // A pid may have been reused by another program since the crash.
    let comm = |pid: &str| std::fs::read(format!("/proc/{pid}/comm")).ok();
    comm(&pid.to_string()).is_some_and(|other| Some(other) == comm("self"))
}

/// Undoes what the records of one log describe.
fn replay(log: &[u8], recovered: &mut Recovered) {
    let mut fields = log.split(|b| *b == 0);
    while let Some(field) = fields.next() {
        let Some((&kind, first)) = field.split_first() else {
            continue;
        };
        let first = path_from(first);
        match kind {
            TEMPORARY => {
                if remove_temporary(&first) {
                    recovered.removed += 1;
                }
            }
            MOVED_ASIDE => {
                // A record cut short by the crash has no second path.
                let Some(original) = fields.next().filter(|f| !f.is_empty()) else {
                    break;
                };
                if restore_aside(&first, &path_from(original)) {
                    recovered.restored += 1;
                }
            }
            _ => break,
        }
    }
}

fn path_from(bytes: &[u8]) -> PathBuf {
    PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()))
}

/// Temporary items are hidden; anything else in a log is not ours to
/// delete.
fn is_hidden(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.as_bytes().first() == Some(&b'.'))
}

fn remove_temporary(path: &Path) -> bool {
    if !is_hidden(path) {
        return false;
    }
    let result = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        // Renamed into place or removed before the crash.
        Err(_) => return false,
    };
    match result {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("cannot remove {}: {err}", path.display());
            false
        }
    }
}

/// Renames `temp` back to `original`, or to a numbered variant if another
/// file took that name meanwhile. Never overwrites anything.
fn restore_aside(temp: &Path, original: &Path) -> bool {
    let (Some(parent), Some(name)) = (original.parent(), original.file_name()) else {
        return false;
    };
    let Ok(meta) = std::fs::symlink_metadata(temp) else {
        return false;
    };
    let target = match name.to_str() {
        Some(name) => {
            let free = naming::unique_name(name, meta.is_dir(), UniqueStyle::Numbered, |n| {
                std::fs::symlink_metadata(parent.join(n)).is_ok()
            });
            parent.join(free)
        }
        // Not UTF-8: numbered names can't be built, only the original fits.
        None if std::fs::symlink_metadata(original).is_err() => original.to_owned(),
        None => return false,
    };
    match std::fs::rename(temp, &target) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("cannot rename {} back: {err}", temp.display());
            false
        }
    }
}
