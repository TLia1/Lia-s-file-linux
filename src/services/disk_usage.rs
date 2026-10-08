//! Folder sizes for the Properties dialog: walks the selected items on a
//! worker thread and streams running totals back, so the dialog counts up
//! live instead of waiting for a large folder.
//!
//! - The walk uses GIO's synchronous API, so network and other GVfs
//!   locations are measured the same way as local folders.
//! - Sizes are the files' apparent sizes (what Copy would transfer), not
//!   the space they take on disk. Folders themselves count as 0 bytes.
//! - Symbolic links are counted as files and never followed; kernel
//!   interfaces (`/proc`, `/sys`…) are skipped.
//! - Dropping the [`Measurement`] cancels it.

use std::time::{Duration, Instant};

use gtk::{gio, prelude::*};

use crate::services::search::is_skipped;

/// Running totals are sent at most this often.
const SEND_INTERVAL: Duration = Duration::from_millis(100);
const WALK_ATTRIBUTES: &str = "standard::name,standard::type,standard::size";

/// What's been counted so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Total size of the files, in bytes.
    pub size: u64,
    /// Files (and links, devices…) inside the measured folders, plus the
    /// measured items that aren't folders.
    pub files: u64,
    /// Folders inside the measured folders (not counting those).
    pub folders: u64,
    /// Folders that couldn't be read: the totals are a lower bound.
    pub unreadable: u64,
}

/// A running measurement. [`Measurement::next`] returns updated totals
/// until it's done; the last value is final.
pub struct Measurement {
    receiver: async_channel::Receiver<Totals>,
    cancellable: gio::Cancellable,
}

impl Measurement {
    pub fn start(files: Vec<gio::File>) -> Self {
        let (sender, receiver) = async_channel::unbounded();
        let cancellable = gio::Cancellable::new();
        let worker_cancellable = cancellable.clone();
        let spawned = std::thread::Builder::new()
            .name("lia-disk-usage".into())
            .spawn(move || {
                let mut last_send = Instant::now();
                let totals = measure(&files, &worker_cancellable, |totals| {
                    if last_send.elapsed() >= SEND_INTERVAL {
                        last_send = Instant::now();
                        let _ = sender.send_blocking(*totals);
                    }
                });
                if !worker_cancellable.is_cancelled() {
                    let _ = sender.send_blocking(totals);
                }
            });
        if let Err(err) = spawned {
            tracing::warn!("cannot start the disk usage thread: {err}");
        }
        Self {
            receiver,
            cancellable,
        }
    }

    /// The next running totals, or `None` once the measurement is done.
    pub async fn next(&self) -> Option<Totals> {
        self.receiver.recv().await.ok()
    }
}

impl Drop for Measurement {
    fn drop(&mut self) {
        self.cancellable.cancel();
    }
}

/// Measures `files` (and everything inside the folders among them),
/// calling `progress` after each folder. Blocking.
pub fn measure(
    files: &[gio::File],
    cancellable: &gio::Cancellable,
    mut progress: impl FnMut(&Totals),
) -> Totals {
    let mut totals = Totals::default();
    let mut folders = Vec::new();
    for file in files {
        match file.query_info(
            WALK_ATTRIBUTES,
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            Some(cancellable),
        ) {
            Ok(info) if info.file_type() == gio::FileType::Directory => folders.push(file.clone()),
            Ok(info) => {
                totals.files += 1;
                totals.size += info.size().max(0) as u64;
            }
            Err(_) => totals.unreadable += 1,
        }
    }

    // Depth-first, so the pending list stays short in wide trees.
    while let Some(folder) = folders.pop() {
        if cancellable.is_cancelled() {
            break;
        }
        let enumerator = match folder.enumerate_children(
            WALK_ATTRIBUTES,
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            Some(cancellable),
        ) {
            Ok(enumerator) => enumerator,
            Err(_) => {
                totals.unreadable += 1;
                continue;
            }
        };
        loop {
            match enumerator.next_file(Some(cancellable)) {
                Ok(Some(info)) => {
                    if info.file_type() == gio::FileType::Directory {
                        totals.folders += 1;
                        let child = enumerator.child(&info);
                        if !is_skipped(&child) {
                            folders.push(child);
                        }
                    } else {
                        totals.files += 1;
                        totals.size += info.size().max(0) as u64;
                    }
                }
                Ok(None) => break,
                Err(_) => {
                    totals.unreadable += 1;
                    break;
                }
            }
        }
        progress(&totals);
    }
    totals
}
