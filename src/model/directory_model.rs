//! `DirectoryModel`: a `GListModel` of `FileItem`s for one folder.
//!
//! - Loading is streamed: children are enumerated asynchronously in batches
//!   and appended as they arrive, so the first files show up immediately.
//! - A `gio::FileMonitor` keeps the model live. Events are coalesced: every
//!   changed name is re-queried after a short delay and the result is
//!   upserted or removed, which handles create/delete/change/rename/move
//!   uniformly.
//! - Everything goes through `gio::File`, so `smb://`, `trash:///`,
//!   `recent:///` etc. work exactly like local folders.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use gtk::{gio, glib, glib::clone, prelude::*, subclass::prelude::*};

use super::FileItem;

/// Attributes queried for every file.
pub const ATTRIBUTES: &str = "standard::*,time::modified,time::created,unix::mode,owner::user,\
                              thumbnail::*,trash::orig-path,trash::deletion-date";

const BATCH_SIZE: i32 = 500;
/// While loading, enumerated items are published to the model (and thus the
/// sort model and the view) at most this often. Every publish makes the
/// sorted view rebuild much of its state, so publishing each I/O batch
/// doubled the load time of a 100k-file folder. The first batch is
/// published immediately.
const PUBLISH_INTERVAL: Duration = Duration::from_millis(150);
const COALESCE_DELAY: Duration = Duration::from_millis(100);

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::DirectoryModel)]
    pub struct DirectoryModel {
        pub(super) items: RefCell<Vec<FileItem>>,
        pub(super) index: RefCell<HashMap<PathBuf, FileItem>>,
        #[property(get, nullable)]
        pub(super) location: RefCell<Option<gio::File>>,
        /// True while the initial enumeration is running.
        #[property(get)]
        pub(super) loading: Cell<bool>,
        pub(super) error: RefCell<Option<glib::Error>>,
        /// Incremented on every `load()`; async work from an older
        /// generation discards its results.
        pub(super) generation: Cell<u64>,
        pub(super) load_task: RefCell<Option<glib::JoinHandle<()>>>,
        pub(super) monitor: RefCell<Option<gio::FileMonitor>>,
        pub(super) pending: RefCell<HashSet<PathBuf>>,
        pub(super) flush_timer: RefCell<Option<glib::JoinHandle<()>>>,
        pub(super) flushing: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DirectoryModel {
        const NAME: &'static str = "LiaDirectoryModel";
        type Type = super::DirectoryModel;
        type Interfaces = (gio::ListModel,);
    }

    #[glib::derived_properties]
    impl ObjectImpl for DirectoryModel {
        fn dispose(&self) {
            self.obj().stop();
        }
    }

    impl ListModelImpl for DirectoryModel {
        fn item_type(&self) -> glib::Type {
            FileItem::static_type()
        }

        fn n_items(&self) -> u32 {
            self.items.borrow().len() as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            self.items
                .borrow()
                .get(position as usize)
                .map(|item| item.clone().upcast())
        }
    }
}

glib::wrapper! {
    pub struct DirectoryModel(ObjectSubclass<imp::DirectoryModel>)
        @implements gio::ListModel;
}

impl Default for DirectoryModel {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl DirectoryModel {
    pub fn new() -> Self {
        Self::default()
    }

    /// The error that stopped the last load, if any. Valid once `loading`
    /// turns false.
    pub fn error(&self) -> Option<glib::Error> {
        self.imp().error.borrow().clone()
    }

    /// Loads `location`, replacing the current contents.
    pub fn load(&self, location: &gio::File) {
        let imp = self.imp();
        self.stop();
        let generation = imp.generation.get() + 1;
        imp.generation.set(generation);

        imp.location.replace(Some(location.clone()));
        imp.error.replace(None);
        imp.index.borrow_mut().clear();
        let removed = imp.items.take().len() as u32;
        if removed > 0 {
            self.items_changed(0, removed, 0);
        }
        self.set_loading(true);
        self.notify_location();

        // Start monitoring before enumerating so no change is missed; events
        // arriving during the load are applied once it finishes.
        match location.monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
        {
            Ok(monitor) => {
                monitor.connect_changed(clone!(
                    #[weak(rename_to = model)]
                    self,
                    move |_, file, other, event| model.on_monitor_event(file, other, event)
                ));
                imp.monitor.replace(Some(monitor));
            }
            Err(err) => tracing::debug!("cannot monitor {}: {err}", location.uri()),
        }

        let task = glib::spawn_future_local(clone!(
            #[weak(rename_to = model)]
            self,
            #[strong]
            location,
            async move {
                let started = std::time::Instant::now();
                let result = model.enumerate(&location, generation).await;
                if model.imp().generation.get() != generation {
                    return;
                }
                if let Err(err) = result {
                    tracing::debug!("failed to list {}: {err}", location.uri());
                    model.imp().error.replace(Some(err));
                }
                tracing::debug!(
                    "listed {} items in {:?}: {}",
                    model.n_items(),
                    started.elapsed(),
                    location.uri()
                );
                model.imp().load_task.take();
                model.set_loading(false);
                model.schedule_flush();
            }
        ));
        imp.load_task.replace(Some(task));
    }

    /// Reloads the current location from scratch.
    pub fn reload(&self) {
        if let Some(location) = self.location() {
            self.load(&location);
        }
    }

    /// Cancels loading and monitoring.
    pub fn stop(&self) {
        let imp = self.imp();
        imp.generation.set(imp.generation.get() + 1);
        if let Some(task) = imp.load_task.take() {
            task.abort();
        }
        if let Some(monitor) = imp.monitor.take() {
            monitor.cancel();
        }
        if let Some(timer) = imp.flush_timer.take() {
            timer.abort();
        }
        imp.pending.borrow_mut().clear();
        imp.flushing.set(false);
        if imp.loading.get() {
            self.set_loading(false);
        }
    }

    /// Stops loading and empties the model (for locations that aren't
    /// folders, such as the "This PC" page).
    pub fn clear(&self) {
        let imp = self.imp();
        self.stop();
        imp.location.replace(None);
        imp.error.replace(None);
        imp.index.borrow_mut().clear();
        let removed = imp.items.take().len() as u32;
        if removed > 0 {
            self.items_changed(0, removed, 0);
        }
        self.notify_location();
    }

    /// Whether changes are followed live. Unmonitored folders must be
    /// reloaded after an operation changes them.
    pub fn is_monitored(&self) -> bool {
        self.imp().monitor.borrow().is_some()
    }

    /// Finds an item by its `gio::File`.
    pub fn find(&self, file: &gio::File) -> Option<FileItem> {
        let name = file.basename()?;
        self.imp().index.borrow().get(&name).cloned()
    }

    fn set_loading(&self, loading: bool) {
        self.imp().loading.set(loading);
        self.notify_loading();
    }

    async fn enumerate(&self, location: &gio::File, generation: u64) -> Result<(), glib::Error> {
        let started = std::time::Instant::now();
        let enumerator = location
            .enumerate_children_future(
                ATTRIBUTES,
                gio::FileQueryInfoFlags::NONE,
                glib::Priority::DEFAULT,
            )
            .await?;
        let mut buffer: Vec<FileItem> = Vec::new();
        let mut last_publish: Option<std::time::Instant> = None;
        loop {
            let infos = enumerator
                .next_files_future(BATCH_SIZE, glib::Priority::DEFAULT)
                .await?;
            if self.imp().generation.get() != generation {
                return Ok(());
            }
            if infos.is_empty() {
                break;
            }
            buffer.extend(
                infos
                    .into_iter()
                    .map(|info| FileItem::new(enumerator.child(&info), info)),
            );
            if last_publish.is_none_or(|t| t.elapsed() >= PUBLISH_INTERVAL) {
                if last_publish.is_none() {
                    tracing::debug!("first {} items in {:?}", buffer.len(), started.elapsed());
                }
                self.append(std::mem::take(&mut buffer));
                last_publish = Some(std::time::Instant::now());
            }
        }
        self.append(buffer);
        Ok(())
    }

    fn append(&self, items: Vec<FileItem>) {
        let imp = self.imp();
        let position = imp.items.borrow().len() as u32;
        let mut added = 0;
        {
            let mut index = imp.index.borrow_mut();
            let mut list = imp.items.borrow_mut();
            for item in items {
                if index.insert(item.name(), item.clone()).is_none() {
                    list.push(item);
                    added += 1;
                }
            }
        }
        if added > 0 {
            self.items_changed(position, 0, added);
        }
    }

    /// Inserts `item`, or replaces the item with the same name.
    fn upsert(&self, item: FileItem) {
        let imp = self.imp();
        let name = item.name();
        let old = imp.index.borrow_mut().insert(name, item.clone());
        match old {
            Some(old) => {
                let position = imp.items.borrow().iter().position(|i| *i == old);
                if let Some(position) = position {
                    imp.items.borrow_mut()[position] = item;
                    self.items_changed(position as u32, 1, 1);
                }
            }
            None => {
                let position = imp.items.borrow().len() as u32;
                imp.items.borrow_mut().push(item);
                self.items_changed(position, 0, 1);
            }
        }
    }

    fn remove(&self, name: &PathBuf) {
        let imp = self.imp();
        let Some(old) = imp.index.borrow_mut().remove(name) else {
            return;
        };
        let position = imp.items.borrow().iter().position(|i| *i == old);
        if let Some(position) = position {
            imp.items.borrow_mut().remove(position);
            self.items_changed(position as u32, 1, 0);
        }
    }

    fn on_monitor_event(
        &self,
        file: &gio::File,
        other: Option<&gio::File>,
        event: gio::FileMonitorEvent,
    ) {
        use gio::FileMonitorEvent as E;
        let Some(location) = self.location() else {
            return;
        };
        let relevant = matches!(
            event,
            E::Created
                | E::Deleted
                | E::Changed
                | E::ChangesDoneHint
                | E::AttributeChanged
                | E::Renamed
                | E::MovedIn
                | E::MovedOut
                | E::Moved
        );
        if !relevant {
            return;
        }
        let mut pending = self.imp().pending.borrow_mut();
        for f in std::iter::once(file).chain(other) {
            if f.has_parent(Some(&location))
                && let Some(name) = f.basename()
            {
                pending.insert(name);
            }
        }
        drop(pending);
        self.schedule_flush();
    }

    fn schedule_flush(&self) {
        let imp = self.imp();
        if imp.flush_timer.borrow().is_some() || imp.pending.borrow().is_empty() {
            return;
        }
        // A future (rather than `timeout_add_local`) so the timer runs on the
        // thread-default main context, like the rest of the model's work.
        let timer = glib::spawn_future_local(clone!(
            #[weak(rename_to = model)]
            self,
            async move {
                glib::timeout_future(COALESCE_DELAY).await;
                model.imp().flush_timer.take();
                model.flush();
            }
        ));
        imp.flush_timer.replace(Some(timer));
    }

    /// Re-queries every pending name and applies the results.
    fn flush(&self) {
        let imp = self.imp();
        if imp.loading.get() || imp.flushing.get() {
            // Rescheduled when the load / current flush finishes.
            return;
        }
        let Some(location) = self.location() else {
            return;
        };
        let names: Vec<PathBuf> = imp.pending.take().into_iter().collect();
        if names.is_empty() {
            return;
        }
        imp.flushing.set(true);
        let generation = imp.generation.get();

        glib::spawn_future_local(clone!(
            #[weak(rename_to = model)]
            self,
            async move {
                // Start all queries at once; they run concurrently.
                let queries: Vec<_> = names
                    .into_iter()
                    .map(|name| {
                        let child = location.child(&name);
                        let handle = glib::spawn_future_local(child.query_info_future(
                            ATTRIBUTES,
                            gio::FileQueryInfoFlags::NONE,
                            glib::Priority::DEFAULT,
                        ));
                        (name, child, handle)
                    })
                    .collect();

                for (name, child, handle) in queries {
                    let Ok(result) = handle.await else { continue };
                    if model.imp().generation.get() != generation {
                        return;
                    }
                    match result {
                        Ok(info) => model.upsert(FileItem::new(child, info)),
                        Err(err) if err.matches(gio::IOErrorEnum::NotFound) => model.remove(&name),
                        Err(err) => tracing::debug!("cannot query {}: {err}", child.uri()),
                    }
                }
                model.imp().flushing.set(false);
                model.schedule_flush();
            }
        ));
    }
}
