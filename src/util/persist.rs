//! Small state files (view settings, undo history) saved from the UI thread
//! without blocking it.
//!
//! Writes are atomic — the new contents go to a temporary file which is
//! synced, then renamed over the old one — so a crash or power loss leaves
//! either the old or the new version, never a truncated file. They are
//! also ordered: a write of an older version that finishes late never
//! replaces a newer one.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub struct StateFile {
    path: PathBuf,
    /// Incremented on each save.
    generation: AtomicU64,
    /// The newest version written to disk.
    written: Mutex<u64>,
}

impl StateFile {
    pub fn new(path: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            path,
            generation: AtomicU64::new(0),
            written: Mutex::new(0),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The saved contents; `None` if there are none or they can't be read.
    pub fn read(&self) -> Option<String> {
        match std::fs::read_to_string(&self.path) {
            Ok(contents) => Some(contents),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
            Err(err) => {
                tracing::warn!("cannot read {}: {err}", self.path.display());
                None
            }
        }
    }

    /// Writes `contents` on a worker thread.
    pub fn save(self: &Arc<Self>, contents: String) {
        let generation = self.next_generation();
        let file = self.clone();
        std::thread::spawn(move || file.write(&contents, generation));
    }

    /// Writes `contents` now (when the app quits).
    pub fn save_now(&self, contents: &str) {
        self.write(contents, self.next_generation());
    }

    fn next_generation(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn write(&self, contents: &str, generation: u64) {
        let mut written = self.written.lock().unwrap_or_else(|e| e.into_inner());
        if *written >= generation {
            return;
        }
        match write_atomic(&self.path, contents.as_bytes()) {
            Ok(()) => *written = generation,
            Err(err) => tracing::warn!("cannot save {}: {err}", self.path.display()),
        }
    }
}

/// Replaces `path` with `contents` atomically.
pub fn write_atomic(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut temp = path.as_os_str().to_owned();
    temp.push(".new");
    let temp = PathBuf::from(temp);
    let result = (|| {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(contents)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}
