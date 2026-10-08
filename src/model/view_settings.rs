//! Per-folder view memory: the view mode, sort and grouping chosen for a
//! folder are remembered and restored when it's opened again. Folders
//! without settings use the defaults (Preferences).
//!
//! Stored as JSON in `~/.local/share/lia-files/views.json` rather than as
//! GIO metadata, which needs the GVfs metadata daemon and only works on
//! some file systems. [`ViewStore`] is the plain data; the functions at the
//! bottom are the app-wide store, saved in the background shortly after
//! each change.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::time::Duration;

use gtk::{gio, glib, prelude::*};
use serde::{Deserialize, Serialize};

/// Folders remembered at most; the least recently changed are forgotten.
pub const MAX_FOLDERS: usize = 2000;
/// Changes are written this long after the last one.
const SAVE_DELAY: Duration = Duration::from_secs(1);

/// How one folder is shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderView {
    /// "details" or "grid".
    pub view: String,
    /// Column id: "name", "modified", "type", "size"…
    pub sort: String,
    pub descending: bool,
    /// `GroupBy` nick.
    #[serde(default = "no_group")]
    pub group: String,
    /// When last changed (UNIX time).
    #[serde(default)]
    pub used: i64,
}

fn no_group() -> String {
    "none".into()
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ViewStore {
    #[serde(default)]
    folders: HashMap<String, FolderView>,
}

impl ViewStore {
    /// Reads a saved store; a damaged file gives an empty store.
    pub fn parse(json: &str) -> Self {
        serde_json::from_str(json).unwrap_or_else(|err| {
            tracing::warn!("ignoring unreadable view settings: {err}");
            Self::default()
        })
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn get(&self, uri: &str) -> Option<&FolderView> {
        self.folders.get(uri)
    }

    /// Remembers `view` for the folder `uri`, at time `now`.
    pub fn set(&mut self, uri: &str, mut view: FolderView, now: i64) {
        view.used = now;
        self.folders.insert(uri.to_owned(), view);
        if self.folders.len() > MAX_FOLDERS {
            let mut by_age: Vec<(i64, String)> = self
                .folders
                .iter()
                .map(|(uri, view)| (view.used, uri.clone()))
                .collect();
            by_age.sort();
            for (_, uri) in by_age.into_iter().take(self.folders.len() - MAX_FOLDERS) {
                self.folders.remove(&uri);
            }
        }
    }

    /// Forgets a folder's settings (it uses the defaults again).
    pub fn remove(&mut self, uri: &str) -> bool {
        self.folders.remove(uri).is_some()
    }

    pub fn len(&self) -> usize {
        self.folders.len()
    }

    pub fn is_empty(&self) -> bool {
        self.folders.is_empty()
    }
}

// ── The app's store ────────────────────────────────────────────────────────

struct Persisted {
    store: RefCell<ViewStore>,
    path: PathBuf,
    save_timeout: RefCell<Option<glib::SourceId>>,
    /// Incremented on each change; writes of older versions are dropped.
    generation: Cell<u64>,
}

thread_local! {
    static STORE: OnceCell<Rc<Persisted>> = const { OnceCell::new() };
}

/// The newest version written to disk (writes happen on worker threads).
static WRITTEN: Mutex<u64> = Mutex::new(0);

fn persisted() -> Rc<Persisted> {
    STORE.with(|cell| {
        cell.get_or_init(|| {
            let path = glib::user_data_dir().join("lia-files").join("views.json");
            // Read once, at the first folder opened: a small local file.
            let store = match std::fs::read_to_string(&path) {
                Ok(json) => ViewStore::parse(&json),
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => ViewStore::default(),
                Err(err) => {
                    tracing::warn!("cannot read {}: {err}", path.display());
                    ViewStore::default()
                }
            };
            Rc::new(Persisted {
                store: RefCell::new(store),
                path,
                save_timeout: RefCell::default(),
                generation: Cell::new(0),
            })
        })
        .clone()
    })
}

/// The settings remembered for `folder`.
pub fn lookup(folder: &gio::File) -> Option<FolderView> {
    persisted().store.borrow().get(&folder.uri()).cloned()
}

/// Remembers `view` for `folder`; `None` forgets it.
pub fn remember(folder: &gio::File, view: Option<FolderView>) {
    let persisted = persisted();
    let uri = folder.uri();
    {
        let mut store = persisted.store.borrow_mut();
        // Unchanged (apart from the time): nothing to save.
        let unchanged = match (&view, store.get(&uri)) {
            (Some(new), Some(old)) => {
                *new == FolderView {
                    used: new.used,
                    ..old.clone()
                }
            }
            (None, None) => true,
            _ => false,
        };
        if unchanged {
            return;
        }
        match view {
            Some(view) => store.set(&uri, view, glib::real_time() / 1_000_000),
            None => {
                store.remove(&uri);
            }
        }
    }
    persisted.generation.set(persisted.generation.get() + 1);
    schedule_save(&persisted);
}

fn schedule_save(persisted: &Rc<Persisted>) {
    if let Some(id) = persisted.save_timeout.take() {
        id.remove();
    }
    let weak = Rc::downgrade(persisted);
    let id = glib::timeout_add_local_once(SAVE_DELAY, move || {
        let Some(persisted) = weak.upgrade() else {
            return;
        };
        persisted.save_timeout.take();
        let json = persisted.store.borrow().to_json();
        let path = persisted.path.clone();
        let generation = persisted.generation.get();
        std::thread::spawn(move || write(&path, &json, generation));
    });
    persisted.save_timeout.replace(Some(id));
}

/// Writes pending changes now (when the app quits).
pub fn flush() {
    let Some(persisted) = STORE.with(|cell| cell.get().cloned()) else {
        return;
    };
    if let Some(id) = persisted.save_timeout.take() {
        id.remove();
        let json = persisted.store.borrow().to_json();
        write(&persisted.path, &json, persisted.generation.get());
    }
}

/// Replaces the file atomically, unless a newer version was written.
fn write(path: &Path, json: &str, generation: u64) {
    let mut written = WRITTEN.lock().unwrap_or_else(|e| e.into_inner());
    if *written >= generation {
        return;
    }
    let result = (|| {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temp = path.with_extension("json.part");
        std::fs::write(&temp, json)?;
        std::fs::rename(&temp, path)
    })();
    match result {
        Ok(()) => *written = generation,
        Err(err) => tracing::warn!("cannot save {}: {err}", path.display()),
    }
}
