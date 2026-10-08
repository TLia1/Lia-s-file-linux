//! Recursive search: walks a folder tree on a worker thread and streams
//! the files whose name contains the query back to the main thread.
//!
//! - The walk uses GIO's synchronous API, so network and other GVfs
//!   locations are searched the same way as local folders.
//! - Folders are walked breadth-first: matches close to the starting folder
//!   come first.
//! - Only names are read while walking; the full `FileInfo` (icon, type,
//!   size…) is queried for matches only.
//! - Symbolic links to folders aren't followed (no loops), and kernel
//!   interfaces (`/proc`, `/sys`…) are skipped.
//! - Dropping the [`Search`] cancels it.

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

use gtk::{gio, prelude::*};

use crate::model::FileItem;
use crate::model::directory_model::ATTRIBUTES;

/// The search stops after this many results.
pub const MAX_RESULTS: usize = 50_000;
/// Matches are sent at most this often (the first one right away), so the
/// view isn't re-sorted for every file.
const SEND_INTERVAL: Duration = Duration::from_millis(100);
/// What the walk needs to know about every file.
const WALK_ATTRIBUTES: &str = "standard::name,standard::display-name,standard::type,\
                               standard::is-hidden,standard::is-backup";
/// Folders not searched when reached from a parent.
const SKIPPED_PATHS: [&str; 4] = ["/proc", "/sys", "/dev", "/run"];

/// What to look for.
#[derive(Debug, Clone)]
pub struct Query {
    /// Lower-cased text the name must contain.
    pub text: String,
    /// Also search hidden files and folders.
    pub show_hidden: bool,
}

/// Whether a file named `display_name` matches the lower-cased `query`.
pub fn name_matches(display_name: &str, query: &str) -> bool {
    display_name.to_lowercase().contains(query)
}

/// A file found by the search.
pub struct Match {
    file: gio::File,
    info: SendInfo,
}

impl Match {
    pub fn file(&self) -> &gio::File {
        &self.file
    }

    pub fn into_item(self) -> FileItem {
        FileItem::new(self.file, self.info.0)
    }
}

/// A `FileInfo` queried on the worker thread for the main thread.
struct SendInfo(gio::FileInfo);

// SAFETY: a `FileInfo` must not be used from two threads at once. Each one
// is created by the worker and then moved, with its only reference, to the
// receiving thread; the worker never touches it again. (GIO's own async
// queries hand their results over the same way.)
unsafe impl Send for SendInfo {}

pub enum Event {
    Found(Vec<Match>),
    /// The search stopped at [`MAX_RESULTS`].
    LimitReached,
}

/// A running search. Read its results with [`Search::next`]; the channel
/// closes when the walk is done. Dropping it cancels the walk.
pub struct Search {
    receiver: async_channel::Receiver<Event>,
    cancellable: gio::Cancellable,
}

impl Search {
    pub fn start(root: &gio::File, query: Query) -> Self {
        let (sender, receiver) = async_channel::unbounded();
        let cancellable = gio::Cancellable::new();
        let root = root.clone();
        let worker_cancellable = cancellable.clone();
        let spawned = std::thread::Builder::new()
            .name("lia-search".into())
            .spawn(move || walk(root, &query, &sender, &worker_cancellable));
        if let Err(err) = spawned {
            tracing::warn!("cannot start the search thread: {err}");
        }
        Self {
            receiver,
            cancellable,
        }
    }

    /// The next batch of results, or `None` once the search is done.
    pub async fn next(&self) -> Option<Event> {
        self.receiver.recv().await.ok()
    }
}

impl Drop for Search {
    fn drop(&mut self) {
        self.cancellable.cancel();
    }
}

fn walk(
    root: gio::File,
    query: &Query,
    sender: &async_channel::Sender<Event>,
    cancellable: &gio::Cancellable,
) {
    let started = Instant::now();
    let mut folders = VecDeque::from([root.clone()]);
    let mut batch: Vec<Match> = Vec::new();
    let mut last_send: Option<Instant> = None;
    let mut found = 0;
    let mut scanned = 0usize;

    // Sends the batch; false once nobody is listening anymore.
    let send = |batch: &mut Vec<Match>, last_send: &mut Option<Instant>| {
        *last_send = Some(Instant::now());
        batch.is_empty()
            || sender
                .send_blocking(Event::Found(std::mem::take(batch)))
                .is_ok()
    };

    while let Some(folder) = folders.pop_front() {
        if cancellable.is_cancelled() {
            return;
        }
        let enumerator = match folder.enumerate_children(
            WALK_ATTRIBUTES,
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            Some(cancellable),
        ) {
            Ok(enumerator) => enumerator,
            Err(err) => {
                tracing::debug!("search: cannot list {}: {err}", folder.uri());
                continue;
            }
        };
        loop {
            let info = match enumerator.next_file(Some(cancellable)) {
                Ok(Some(info)) => info,
                Ok(None) => break,
                Err(err) => {
                    tracing::debug!("search: error listing {}: {err}", folder.uri());
                    break;
                }
            };
            scanned += 1;
            if !query.show_hidden && (info.is_hidden() || info.is_backup()) {
                continue;
            }
            let child = enumerator.child(&info);
            if info.file_type() == gio::FileType::Directory && !is_skipped(&child) {
                folders.push_back(child.clone());
            }
            if !name_matches(&info.display_name(), &query.text) {
                continue;
            }
            // Same attributes and flags as `DirectoryModel`, so results
            // look and sort like folder items.
            match child.query_info(ATTRIBUTES, gio::FileQueryInfoFlags::NONE, Some(cancellable)) {
                Ok(info) => {
                    batch.push(Match {
                        file: child,
                        info: SendInfo(info),
                    });
                    found += 1;
                }
                Err(err) => tracing::debug!("search: cannot query {}: {err}", child.uri()),
            }
            if found >= MAX_RESULTS {
                if send(&mut batch, &mut last_send) {
                    let _ = sender.send_blocking(Event::LimitReached);
                }
                return;
            }
            if !batch.is_empty()
                && last_send.is_none_or(|t| t.elapsed() >= SEND_INTERVAL)
                && !send(&mut batch, &mut last_send)
            {
                return;
            }
        }
        let _ = enumerator.close(Some(cancellable));
    }
    send(&mut batch, &mut last_send);
    tracing::debug!(
        "search for {:?} in {}: {found} results, {scanned} files scanned in {:?}",
        query.text,
        root.uri(),
        started.elapsed()
    );
}

/// Kernel interfaces that walks don't enter when reached from a parent.
pub(crate) fn is_skipped(folder: &gio::File) -> bool {
    folder
        .path()
        .is_some_and(|path| SKIPPED_PATHS.iter().any(|p| path == Path::new(p)))
}
