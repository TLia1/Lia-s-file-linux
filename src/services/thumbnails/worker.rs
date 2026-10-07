//! The worker threads: look a thumbnail up in the cache, or make one with
//! the right thumbnailer, off the UI thread.
//!
//! Jobs are taken newest first: the most recently requested thumbnails are
//! the ones on screen after a scroll. Jobs whose item scrolled out of view
//! before a worker got to them are cancelled and skipped.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use gtk::{gdk_pixbuf, glib};

use super::spec::{self, ThumbSize};
use super::thumbnailers::{Registry, Thumbnailer};

/// A thumbnailer taking longer than this is killed.
const THUMBNAILER_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Job {
    pub uri: String,
    /// The local path, needed to make a thumbnail.
    pub path: Option<PathBuf>,
    pub mime_type: String,
    pub mtime: i64,
    pub size: ThumbSize,
    /// Whether a missing thumbnail may be made (policy, file size…).
    pub generate: bool,
    pub cancelled: Arc<AtomicBool>,
    pub reply: async_channel::Sender<Outcome>,
}

#[derive(Debug)]
pub enum Outcome {
    Ready(Image),
    /// No thumbnail (and none will be made until the file changes).
    Failed,
    /// No thumbnailer handles this MIME type.
    Unsupported,
}

/// Decoded pixels, sent to the UI thread to become a texture.
#[derive(Debug)]
pub struct Image {
    pub bytes: glib::Bytes,
    pub width: i32,
    pub height: i32,
    pub stride: usize,
    pub has_alpha: bool,
}

impl Image {
    fn from_pixbuf(pixbuf: &gdk_pixbuf::Pixbuf) -> Self {
        Self {
            bytes: pixbuf.read_pixel_bytes(),
            width: pixbuf.width(),
            height: pixbuf.height(),
            stride: pixbuf.rowstride() as usize,
            has_alpha: pixbuf.has_alpha(),
        }
    }
}

#[derive(Default)]
pub struct Queue {
    jobs: Mutex<Vec<Job>>,
    available: Condvar,
}

impl Queue {
    pub fn push(&self, job: Job) {
        let mut jobs = self.jobs.lock().unwrap();
        // Drop jobs nobody waits for anymore, so the queue stays short
        // while scrolling through a huge folder.
        jobs.retain(|job| !job.cancelled.load(Ordering::Relaxed));
        jobs.push(job);
        drop(jobs);
        self.available.notify_one();
    }

    /// The newest job that's still wanted; blocks until there is one.
    fn pop(&self) -> Job {
        let mut jobs = self.jobs.lock().unwrap();
        loop {
            while let Some(job) = jobs.pop() {
                if !job.cancelled.load(Ordering::Relaxed) {
                    return job;
                }
            }
            jobs = self.available.wait(jobs).unwrap();
        }
    }
}

pub fn spawn_workers(queue: &Arc<Queue>, count: usize) {
    for i in 0..count {
        let queue = queue.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("thumbnailer-{i}"))
            .spawn(move || {
                let root = spec::cache_root();
                loop {
                    let job = queue.pop();
                    let outcome = process(&job, &root, registry());
                    // The receiver is gone if the request was dropped.
                    let _ = job.reply.send_blocking(outcome);
                }
            });
        if let Err(err) = spawned {
            tracing::warn!("cannot start a thumbnail worker: {err}");
        }
    }
}

fn registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let registry = Registry::system();
        if registry.is_empty() {
            tracing::info!("no thumbnailers installed: only cached thumbnails are shown");
        }
        registry
    })
}

/// Finds or makes the thumbnail for `job`, using the cache under `root`.
pub fn process(job: &Job, root: &Path, registry: &Registry) -> Outcome {
    // A larger thumbnail is fine too: it's scaled down when drawn.
    for size in job.size.and_larger() {
        let path = spec::thumbnail_path(root, &job.uri, size);
        if let Some(pixbuf) = spec::load_valid(&path, &job.uri, job.mtime) {
            return Outcome::Ready(Image::from_pixbuf(&pixbuf));
        }
    }

    let Some(input) = job.path.as_deref().filter(|_| job.generate) else {
        return Outcome::Failed;
    };
    if spec::load_valid(&spec::fail_path(root, &job.uri), &job.uri, job.mtime).is_some() {
        return Outcome::Failed;
    }

    let pixels = job.size.pixels();
    let pixbuf = match registry.find(&job.mime_type) {
        Some(thumbnailer) => run_thumbnailer(thumbnailer, input, &job.uri, pixels),
        // Without a thumbnailer for images (glycin), load them here.
        None if job.mime_type.starts_with("image/") => load_image(input, pixels),
        None => return Outcome::Unsupported,
    };
    let Some(pixbuf) = pixbuf.map(|p| fit(p, pixels)) else {
        tracing::debug!("no thumbnail for {}", job.uri);
        if let Err(err) = spec::save_failure(root, &job.uri, job.mtime) {
            tracing::debug!("cannot record failed thumbnail: {err}");
        }
        return Outcome::Failed;
    };

    let path = spec::thumbnail_path(root, &job.uri, job.size);
    if let Err(err) = spec::save(&pixbuf, &path, &job.uri, job.mtime, Some(&job.mime_type)) {
        tracing::debug!("cannot save thumbnail {}: {err}", path.display());
    }
    Outcome::Ready(Image::from_pixbuf(&pixbuf))
}

/// Runs a thumbnailer and loads what it wrote.
fn run_thumbnailer(
    thumbnailer: &Thumbnailer,
    input: &Path,
    uri: &str,
    size: i32,
) -> Option<gdk_pixbuf::Pixbuf> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let output = glib::tmp_dir().join(format!(
        "lia-files-thumbnail-{}-{}.png",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let argv = thumbnailer.command(input, uri, &output, size)?;
    let result = run_with_timeout(&argv).then(|| gdk_pixbuf::Pixbuf::from_file(&output).ok());
    let _ = std::fs::remove_file(&output);
    result.flatten()
}

fn run_with_timeout(argv: &[std::ffi::OsString]) -> bool {
    let (program, args) = argv.split_first().expect("non-empty command");
    let mut child = match Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            tracing::debug!("cannot run {}: {err}", program.to_string_lossy());
            return false;
        }
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if started.elapsed() < THUMBNAILER_TIMEOUT => {
                std::thread::sleep(Duration::from_millis(15));
            }
            _ => {
                tracing::debug!("{} timed out", program.to_string_lossy());
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn load_image(path: &Path, size: i32) -> Option<gdk_pixbuf::Pixbuf> {
    let pixbuf = gdk_pixbuf::Pixbuf::from_file_at_scale(path, size, size, true).ok()?;
    Some(pixbuf.apply_embedded_orientation().unwrap_or(pixbuf))
}

/// Scales `pixbuf` down so its longest side is at most `size` (never up).
fn fit(pixbuf: gdk_pixbuf::Pixbuf, size: i32) -> gdk_pixbuf::Pixbuf {
    let (width, height) = (pixbuf.width(), pixbuf.height());
    let longest = width.max(height);
    if longest <= size {
        return pixbuf;
    }
    let scale = |side: i32| ((side as f64 * size as f64 / longest as f64).round() as i32).max(1);
    pixbuf
        .scale_simple(
            scale(width),
            scale(height),
            gdk_pixbuf::InterpType::Bilinear,
        )
        .unwrap_or(pixbuf)
}
