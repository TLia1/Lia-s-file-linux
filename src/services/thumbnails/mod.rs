//! `ThumbnailService`: thumbnails for images, videos, documents… following
//! the freedesktop thumbnail spec (shared cache in `~/.cache/thumbnails`).
//!
//! - Lookups and generation run on a small pool of worker threads; the
//!   newest requests go first, and requests for items scrolled out of view
//!   are dropped (see [`worker`]).
//! - Missing thumbnails are made by the system thumbnailers (glycin for
//!   images, ffmpegthumbnailer for videos…), only for local files under a
//!   size limit by default (`thumbnail-policy`, `thumbnail-max-size`).
//! - Textures are kept in an in-memory LRU. Thumbnails are set on
//!   `FileItem::thumbnail`, so every view of an item shows them; an item
//!   loses its texture when it's evicted, which bounds memory in huge
//!   folders.

pub mod spec;
pub mod thumbnailers;
pub mod worker;

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gtk::{gdk, glib, prelude::*};

pub use spec::ThumbSize;

use crate::model::FileItem;
use crate::util::settings;
use worker::{Job, Outcome, Queue};

const WORKERS: usize = 3;
/// Memory for decoded thumbnails (4 bytes per pixel).
const CACHE_BYTES: usize = 64 * 1024 * 1024;

/// A thumbnail in the cache: the file's URI and modification time, and
/// the size asked for.
type Key = (String, i64, ThumbSize);

struct CacheEntry {
    texture: gdk::Texture,
    /// The items showing this texture, cleared on eviction.
    items: Vec<glib::WeakRef<FileItem>>,
    last_used: u64,
}

#[derive(Default)]
struct MemoryCache {
    entries: HashMap<Key, CacheEntry>,
    bytes: usize,
    clock: u64,
}

impl MemoryCache {
    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn get(&mut self, key: &Key) -> Option<gdk::Texture> {
        let now = self.tick();
        let entry = self.entries.get_mut(key)?;
        entry.last_used = now;
        Some(entry.texture.clone())
    }

    fn insert(&mut self, key: Key, texture: gdk::Texture) {
        let now = self.tick();
        self.bytes += texture_bytes(&texture);
        let entry = CacheEntry {
            texture,
            items: Vec::new(),
            last_used: now,
        };
        if let Some(old) = self.entries.insert(key, entry) {
            self.bytes -= texture_bytes(&old.texture);
        }
        while self.bytes > CACHE_BYTES && self.entries.len() > 1 {
            let oldest = self
                .entries
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| key.clone())
                .unwrap();
            let entry = self.entries.remove(&oldest).unwrap();
            self.bytes -= texture_bytes(&entry.texture);
            // Deferred: clearing a thumbnail notifies, and handlers may
            // call back into the service.
            glib::idle_add_local_once(move || {
                for item in entry.items.iter().filter_map(|i| i.upgrade()) {
                    if item.thumbnail().as_ref() == Some(&entry.texture) {
                        item.set_thumbnail(None);
                    }
                }
            });
        }
    }

    fn attach(&mut self, key: &Key, item: &FileItem) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.items.retain(|i| i.upgrade().is_some());
            if !entry
                .items
                .iter()
                .any(|i| i.upgrade().as_ref() == Some(item))
            {
                entry.items.push(item.downgrade());
            }
        }
    }
}

fn texture_bytes(texture: &gdk::Texture) -> usize {
    texture.width() as usize * texture.height() as usize * 4
}

/// What a thumbnail is looked up / made for.
#[derive(Clone)]
struct Source {
    uri: String,
    path: Option<PathBuf>,
    mime_type: String,
    mtime: i64,
    generate: bool,
}

/// Cancels a thumbnail request when dropped (unless a worker already
/// started on it).
pub struct ThumbnailRequest(Option<glib::JoinHandle<()>>);

impl Drop for ThumbnailRequest {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// Marks a queued job as cancelled when the waiting future is dropped.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

struct Inner {
    queue: Arc<Queue>,
    workers_started: Cell<bool>,
    root: PathBuf,
    cache: RefCell<MemoryCache>,
    /// MIME types no thumbnailer handles.
    unsupported: RefCell<HashSet<String>>,
    /// Files without a thumbnail, by URI and modification time.
    failed: RefCell<HashSet<(String, i64)>>,
}

#[derive(Clone)]
pub struct ThumbnailService(Rc<Inner>);

thread_local! {
    static SERVICE: OnceCell<ThumbnailService> = const { OnceCell::new() };
}

pub fn thumbnail_service() -> ThumbnailService {
    SERVICE.with(|cell| cell.get_or_init(ThumbnailService::new).clone())
}

impl ThumbnailService {
    fn new() -> Self {
        let service = Self(Rc::new(Inner {
            queue: Arc::default(),
            workers_started: Cell::new(false),
            root: spec::cache_root(),
            cache: RefCell::default(),
            unsupported: RefCell::default(),
            failed: RefCell::default(),
        }));
        // A more generous policy may give thumbnails to files that had none.
        let weak = Rc::downgrade(&service.0);
        settings().connect_changed(None, move |_, key| {
            if key.starts_with("thumbnail-")
                && let Some(inner) = weak.upgrade()
            {
                inner.failed.borrow_mut().clear();
            }
        });
        service
    }

    /// Sets `item`'s `thumbnail` to one at least `size` big, now if it's in
    /// memory, otherwise once it's loaded or made. Dropping the returned
    /// request cancels it. Returns `None` when there's nothing to wait for.
    pub fn request(&self, item: &FileItem, size: ThumbSize) -> Option<ThumbnailRequest> {
        let source = self.source(item)?;
        if let Some((key, texture)) = self.cached(&source, size) {
            self.0.cache.borrow_mut().attach(&key, item);
            set_if_larger(item, &texture);
            return None;
        }
        let load = self.fetch(source, size);
        let item = item.downgrade();
        let service = self.clone();
        let handle = glib::spawn_future_local(async move {
            let Some((key, texture)) = load.await else {
                return;
            };
            if let Some(item) = item.upgrade() {
                service.0.cache.borrow_mut().attach(&key, &item);
                set_if_larger(&item, &texture);
            }
        });
        Some(ThumbnailRequest(Some(handle)))
    }

    /// Loads `item`'s thumbnail without setting it on the item.
    pub fn load(
        &self,
        item: &FileItem,
        size: ThumbSize,
    ) -> impl Future<Output = Option<gdk::Texture>> + 'static {
        let source = self.source(item);
        let service = self.clone();
        async move {
            let source = source?;
            if let Some((_, texture)) = service.cached(&source, size) {
                return Some(texture);
            }
            service.fetch(source, size).await.map(|(_, t)| t)
        }
    }

    /// Whether `item` may have a thumbnail (so a view can wait for it).
    pub fn may_have_thumbnail(&self, item: &FileItem) -> bool {
        self.source(item).is_some()
    }

    fn source(&self, item: &FileItem) -> Option<Source> {
        if item.is_dir() {
            return None;
        }
        let settings = settings();
        let policy = settings.string("thumbnail-policy");
        if policy == "never" {
            return None;
        }
        let mime_type = item.content_type()?.to_string();
        if self.0.unsupported.borrow().contains(&mime_type) {
            return None;
        }
        let file = item.target_file();
        let uri = file.uri().to_string();
        let mtime = item.modified();
        if self.0.failed.borrow().contains(&(uri.clone(), mtime)) {
            return None;
        }
        let path = file.path();
        // Never make thumbnails of thumbnails.
        if path.as_ref().is_some_and(|p| p.starts_with(&self.0.root)) {
            return None;
        }
        let max_size = u64::from(settings.uint("thumbnail-max-size")) * 1_000_000;
        let generate =
            path.is_some() && (policy == "always" || file.is_native()) && item.size() <= max_size;
        Some(Source {
            uri,
            path,
            mime_type,
            mtime,
            generate,
        })
    }

    /// A thumbnail in memory at least `size` big.
    fn cached(&self, source: &Source, size: ThumbSize) -> Option<(Key, gdk::Texture)> {
        let mut cache = self.0.cache.borrow_mut();
        size.and_larger().find_map(|size| {
            let key = (source.uri.clone(), source.mtime, size);
            cache.get(&key).map(|texture| (key, texture))
        })
    }

    /// Queues a job for `source` and waits for its outcome. Dropping the
    /// future cancels the job if no worker took it yet.
    fn fetch(
        &self,
        source: Source,
        size: ThumbSize,
    ) -> impl Future<Output = Option<(Key, gdk::Texture)>> + 'static {
        let (reply, receiver) = async_channel::bounded(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        self.ensure_workers();
        self.0.queue.push(Job {
            uri: source.uri.clone(),
            path: source.path.clone(),
            mime_type: source.mime_type.clone(),
            mtime: source.mtime,
            size,
            generate: source.generate,
            cancelled: cancelled.clone(),
            reply,
        });
        let service = self.clone();
        async move {
            let _cancel = CancelOnDrop(cancelled);
            let outcome = receiver.recv().await.ok()?;
            service.apply(source, size, outcome)
        }
    }

    fn apply(
        &self,
        source: Source,
        size: ThumbSize,
        outcome: Outcome,
    ) -> Option<(Key, gdk::Texture)> {
        match outcome {
            Outcome::Ready(image) => {
                let format = if image.has_alpha {
                    gdk::MemoryFormat::R8g8b8a8
                } else {
                    gdk::MemoryFormat::R8g8b8
                };
                let texture: gdk::Texture = gdk::MemoryTexture::new(
                    image.width,
                    image.height,
                    format,
                    &image.bytes,
                    image.stride,
                )
                .upcast();
                let key = (source.uri, source.mtime, size);
                self.0
                    .cache
                    .borrow_mut()
                    .insert(key.clone(), texture.clone());
                Some((key, texture))
            }
            Outcome::Failed => {
                self.0
                    .failed
                    .borrow_mut()
                    .insert((source.uri, source.mtime));
                None
            }
            Outcome::Unsupported => {
                self.0.unsupported.borrow_mut().insert(source.mime_type);
                None
            }
        }
    }

    fn ensure_workers(&self) {
        if !self.0.workers_started.replace(true) {
            worker::spawn_workers(&self.0.queue, WORKERS);
        }
    }
}

/// Sets `texture` as `item`'s thumbnail unless it already has a larger one.
fn set_if_larger(item: &FileItem, texture: &gdk::Texture) {
    let longest = |t: &gdk::Texture| t.width().max(t.height());
    if item
        .thumbnail()
        .is_none_or(|current| longest(&current) < longest(texture))
    {
        item.set_thumbnail(Some(texture));
    }
}
