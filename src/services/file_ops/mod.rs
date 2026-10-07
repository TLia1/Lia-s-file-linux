//! The file operations engine: copy, move, trash, delete, restore, rename,
//! create, link — run as background jobs with progress, pause, cancel,
//! conflict & error questions and undo.
//!
//! - [`operation`]: what an operation is, its questions and its progress.
//! - [`engine`]: runs one operation (blocking, on a worker thread).
//! - [`job`] / [`queue`]: the GObject side — jobs the UI can bind to, and
//!   the queue that schedules them (one at a time per device).

pub mod engine;
pub mod job;
pub mod naming;
pub mod operation;
pub mod queue;
pub mod trash;

pub use job::{FileOpJob, JobState};
pub use operation::*;
pub use queue::{JobQueue, queue};
