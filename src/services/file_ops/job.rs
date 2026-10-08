//! `FileOpJob`: one operation as seen by the UI. Its properties mirror the
//! worker's [`Control`] (polled by the queue while running), so the
//! operations popover can simply bind to them.

use std::cell::{Cell, OnceCell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use gettextrs::{gettext, ngettext};
use gtk::{gio, glib, prelude::*, subclass::prelude::*};

use super::operation::{Control, OpKind, Operation, Outcome, Status};
use crate::services::undo::UndoRecord;
use crate::util::format;
use crate::widgets::folder_pane::display_name;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "LiaJobState")]
pub enum JobState {
    #[default]
    Queued,
    Running,
    Paused,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn is_finished(self) -> bool {
        matches!(self, Self::Done | Self::Failed | Self::Cancelled)
    }
}

/// Why a job runs, which decides what happens to undo records.
#[derive(Debug)]
pub enum Purpose {
    /// A user action: its record goes on the undo stack.
    Action,
    /// Running `record.inverse`.
    Undo(UndoRecord),
    /// Running `record.forward` again.
    Redo(UndoRecord),
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::FileOpJob)]
    pub struct FileOpJob {
        #[property(get)]
        pub(super) id: Cell<u64>,
        /// "Copying 3 items to “Documents”".
        #[property(get)]
        pub(super) title: RefCell<String>,
        #[property(get, builder(JobState::default()))]
        pub(super) state: Cell<JobState>,
        /// Fraction done, or -1 while unknown.
        #[property(get)]
        pub(super) progress: Cell<f64>,
        #[property(get)]
        pub(super) bytes_total: Cell<u64>,
        #[property(get)]
        pub(super) bytes_done: Cell<u64>,
        #[property(get)]
        pub(super) current_file: RefCell<String>,
        /// "12 MB of 40 MB · 5.2 MB/s · 6 seconds left".
        #[property(get)]
        pub(super) status_text: RefCell<String>,
        /// Summary of what went wrong, once finished.
        #[property(get)]
        pub(super) error: RefCell<String>,

        pub(super) operation: OnceCell<Operation>,
        pub(super) control: Arc<Control>,
        pub(super) purpose: RefCell<Option<Purpose>>,
        /// The widget (window) the job was started from: dialogs and toasts
        /// go there.
        pub(super) origin: glib::WeakRef<gtk::Widget>,
        pub(super) outcome: RefCell<Option<Outcome>>,
        /// The job's result was recorded in the undo history.
        pub(super) undoable: Cell<bool>,
        pub(super) started: Cell<Option<Instant>>,
        pub(super) finished: Cell<Option<Instant>>,
        /// Smoothed transfer speed (bytes/s) and the last sample.
        pub(super) speed: Cell<f64>,
        pub(super) last_sample: Cell<Option<(Instant, u64)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FileOpJob {
        const NAME: &'static str = "LiaFileOpJob";
        type Type = super::FileOpJob;
    }

    #[glib::derived_properties]
    impl ObjectImpl for FileOpJob {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![glib::subclass::Signal::builder("finished").build()])
        }
    }
}

glib::wrapper! {
    pub struct FileOpJob(ObjectSubclass<imp::FileOpJob>);
}

impl FileOpJob {
    pub(super) fn new(
        operation: Operation,
        purpose: Purpose,
        origin: Option<&gtk::Widget>,
    ) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let job: Self = glib::Object::new();
        let imp = job.imp();
        imp.id.set(NEXT_ID.fetch_add(1, Ordering::Relaxed));
        imp.title.replace(title(&operation));
        imp.progress.set(-1.0);
        imp.status_text.replace(gettext("Waiting…"));
        imp.operation.set(operation).unwrap();
        imp.purpose.replace(Some(purpose));
        imp.origin.set(origin);
        job
    }

    pub fn operation(&self) -> &Operation {
        self.imp().operation.get().unwrap()
    }

    pub(super) fn control(&self) -> Arc<Control> {
        self.imp().control.clone()
    }

    pub fn origin(&self) -> Option<gtk::Widget> {
        self.imp().origin.upgrade()
    }

    pub fn is_finished(&self) -> bool {
        self.state().is_finished()
    }

    /// Whether the job's result was recorded in the undo history.
    pub fn is_undoable(&self) -> bool {
        self.imp().undoable.get()
    }

    /// Seconds since the job started (0 if it hasn't).
    pub fn elapsed(&self) -> f64 {
        let imp = self.imp();
        match (imp.started.get(), imp.finished.get()) {
            (Some(start), Some(end)) => (end - start).as_secs_f64(),
            (Some(start), None) => start.elapsed().as_secs_f64(),
            _ => 0.0,
        }
    }

    pub fn set_paused(&self, paused: bool) {
        let state = self.state();
        if !matches!(state, JobState::Running | JobState::Paused) {
            return;
        }
        self.imp().control.set_paused(paused);
        self.imp().last_sample.set(None);
        self.set_state(if paused {
            JobState::Paused
        } else {
            JobState::Running
        });
        self.sync_progress();
    }

    pub fn connect_finished<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "finished",
            false,
            glib::closure_local!(move |job: &Self| f(job)),
        )
    }

    /// The outcome, once finished.
    pub fn with_outcome<R>(&self, f: impl FnOnce(&Outcome) -> R) -> Option<R> {
        self.imp().outcome.borrow().as_ref().map(f)
    }

    // ── Driven by the queue ────────────────────────────────────────────────

    pub(super) fn set_state(&self, state: JobState) {
        let imp = self.imp();
        if imp.state.replace(state) == state {
            return;
        }
        if state == JobState::Running && imp.started.get().is_none() {
            imp.started.set(Some(Instant::now()));
        }
        self.notify_state();
    }

    pub(super) fn set_undoable(&self) {
        self.imp().undoable.set(true);
    }

    pub(super) fn take_purpose(&self) -> Purpose {
        self.imp().purpose.take().unwrap_or(Purpose::Action)
    }

    /// Copies the worker's progress into the properties.
    pub(super) fn sync_progress(&self) {
        let imp = self.imp();
        let progress = imp.control.progress();
        let now = Instant::now();

        if self.state() == JobState::Running && !progress.scanning {
            if let Some((time, bytes)) = imp.last_sample.get() {
                let dt = (now - time).as_secs_f64();
                if dt > 0.05 {
                    let instant = progress.bytes_done.saturating_sub(bytes) as f64 / dt;
                    let speed = imp.speed.get();
                    imp.speed.set(if speed == 0.0 {
                        instant
                    } else {
                        0.8 * speed + 0.2 * instant
                    });
                    imp.last_sample.set(Some((now, progress.bytes_done)));
                }
            } else {
                imp.last_sample.set(Some((now, progress.bytes_done)));
            }
        }

        let fraction = progress.fraction().unwrap_or(-1.0);
        if imp.progress.get() != fraction {
            imp.progress.set(fraction);
            self.notify_progress();
        }
        if imp.bytes_total.replace(progress.bytes_total) != progress.bytes_total {
            self.notify_bytes_total();
        }
        if imp.bytes_done.replace(progress.bytes_done) != progress.bytes_done {
            self.notify_bytes_done();
        }
        if *imp.current_file.borrow() != progress.current {
            imp.current_file.replace(progress.current.clone());
            self.notify_current_file();
        }

        let status = self.status_for(&progress);
        if *imp.status_text.borrow() != status {
            imp.status_text.replace(status);
            self.notify_status_text();
        }
    }

    fn status_for(&self, progress: &super::Progress) -> String {
        let state = self.state();
        if state == JobState::Queued {
            return gettext("Waiting…");
        }
        if progress.scanning {
            return ngettext(
                "Preparing… {} item found",
                "Preparing… {} items found",
                progress.files_total as u32,
            )
            .replace("{}", &progress.files_total.to_string());
        }
        let amount = if progress.bytes_total > 0 {
            gettext("{done} of {total}")
                .replace("{done}", &format::size(progress.bytes_done))
                .replace("{total}", &format::size(progress.bytes_total))
        } else {
            gettext("{done} of {total} items")
                .replace("{done}", &progress.files_done.to_string())
                .replace("{total}", &progress.files_total.to_string())
        };
        if state == JobState::Paused {
            // Translators: "Paused · 12 MB of 40 MB"
            return gettext("Paused · {}").replace("{}", &amount);
        }
        let speed = self.imp().speed.get();
        if progress.bytes_total == 0 || speed < 1.0 || self.elapsed() < 1.0 {
            return amount;
        }
        let left = progress.bytes_total.saturating_sub(progress.bytes_done) as f64 / speed;
        format!(
            "{amount} · {} · {}",
            format::speed(speed),
            format::time_left(left.ceil() as u64)
        )
    }

    pub(super) fn finish(&self, outcome: Outcome) {
        let imp = self.imp();
        imp.finished.set(Some(Instant::now()));
        self.sync_progress();
        let state = match outcome.status {
            Status::Cancelled => JobState::Cancelled,
            Status::Done if outcome.succeeded == 0 && !outcome.errors.is_empty() => {
                JobState::Failed
            }
            Status::Done => JobState::Done,
        };
        if let Some(first) = outcome.errors.first() {
            imp.error.replace(first.clone());
            self.notify_error();
        }
        imp.outcome.replace(Some(outcome));
        let status = match state {
            JobState::Cancelled => gettext("Cancelled"),
            JobState::Failed => gettext("Failed"),
            _ => self.done_message().unwrap_or_else(|| gettext("Done")),
        };
        imp.status_text.replace(status);
        self.notify_status_text();
        if state == JobState::Done {
            imp.progress.set(1.0);
            self.notify_progress();
        }
        self.set_state(state);
        self.emit_by_name::<()>("finished", &[]);
    }

    /// What to tell the user once done ("Moved 3 items to the Trash"), or
    /// `None` for operations that need no message.
    pub fn done_message(&self) -> Option<String> {
        let (succeeded, skipped) = self.with_outcome(|o| (o.succeeded, o.skipped))?;
        let operation = self.operation();
        let n = succeeded;
        let one = || first_name(operation);
        let to = || destination_name(operation);
        let message = match operation.kind() {
            OpKind::Rename | OpKind::CreateFolder | OpKind::CreateFile => return None,
            _ if n == 0 => return None,
            OpKind::BatchRename => {
                ngettext("Renamed {n} item", "Renamed {n} items", n).replace("{n}", &n.to_string())
            }
            OpKind::Extract => counted(
                n,
                || gettext("Extracted “{name}”").replace("{name}", &one()),
                ngettext("Extracted {n} archive", "Extracted {n} archives", n),
            ),
            OpKind::Compress => gettext("Created “{name}”").replace("{name}", &created_name(self)),
            OpKind::Copy => counted(
                n,
                || gettext("Copied “{name}” to “{to}”").replace("{name}", &one()),
                ngettext("Copied {n} item to “{to}”", "Copied {n} items to “{to}”", n),
            )
            .replace("{to}", &to()),
            OpKind::Move => counted(
                n,
                || gettext("Moved “{name}” to “{to}”").replace("{name}", &one()),
                ngettext("Moved {n} item to “{to}”", "Moved {n} items to “{to}”", n),
            )
            .replace("{to}", &to()),
            OpKind::Link => counted(
                n,
                || gettext("Created a link to “{name}”").replace("{name}", &one()),
                ngettext("Created {n} link", "Created {n} links", n),
            ),
            OpKind::Duplicate => counted(
                n,
                || gettext("Duplicated “{name}”").replace("{name}", &one()),
                ngettext("Duplicated {n} item", "Duplicated {n} items", n),
            ),
            OpKind::Trash => counted(
                n,
                || gettext("Moved “{name}” to the Trash").replace("{name}", &one()),
                ngettext(
                    "Moved {n} item to the Trash",
                    "Moved {n} items to the Trash",
                    n,
                ),
            ),
            OpKind::Delete => counted(
                n,
                || gettext("Deleted “{name}”").replace("{name}", &one()),
                ngettext("Deleted {n} item", "Deleted {n} items", n),
            ),
            OpKind::Restore => counted(
                n,
                || gettext("Restored “{name}”").replace("{name}", &one()),
                ngettext("Restored {n} item", "Restored {n} items", n),
            ),
            OpKind::EmptyTrash => gettext("Trash emptied"),
        };
        Some(if skipped > 0 {
            format!(
                "{message} — {}",
                ngettext("{} skipped", "{} skipped", skipped).replace("{}", &skipped.to_string())
            )
        } else {
            message
        })
    }
}

/// The name of the first item the job created.
fn created_name(job: &FileOpJob) -> String {
    job.with_outcome(|o| o.created.first().map(file_name))
        .flatten()
        .unwrap_or_default()
}

/// Names the item when there is one ("Moved “a.txt”…"), otherwise uses the
/// translated count (`many`, with a `{n}` placeholder).
fn counted(n: u32, one: impl FnOnce() -> String, many: String) -> String {
    if n == 1 {
        one()
    } else {
        many.replace("{n}", &n.to_string())
    }
}

fn first_name(operation: &Operation) -> String {
    operation
        .sources()
        .first()
        .map(file_name)
        .or_else(|| match operation {
            Operation::CreateFolder { name, .. } | Operation::CreateFile { name, .. } => {
                Some(name.clone())
            }
            Operation::Compress { target, .. } => Some(file_name(target)),
            _ => None,
        })
        .unwrap_or_default()
}

fn destination_name(operation: &Operation) -> String {
    operation
        .destination()
        .map(|d| display_name(&d))
        .unwrap_or_default()
}

/// A name for messages, without I/O. Trash items use their display name.
fn file_name(file: &gio::File) -> String {
    file.basename()
        .map(|b| b.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.parse_name().into())
}

/// The progress title of an operation.
pub fn title(operation: &Operation) -> String {
    let n = operation.item_count() as u32;
    let one = || first_name(operation);
    let to = || destination_name(operation);
    let pick = |single: String, many: String| {
        if n == 1 {
            single
        } else {
            many.replace("{n}", &n.to_string())
        }
    };
    match operation.kind() {
        OpKind::Copy => pick(
            gettext("Copying “{name}” to “{to}”").replace("{name}", &one()),
            ngettext(
                "Copying {n} item to “{to}”",
                "Copying {n} items to “{to}”",
                n,
            ),
        )
        .replace("{to}", &to()),
        OpKind::Move => pick(
            gettext("Moving “{name}” to “{to}”").replace("{name}", &one()),
            ngettext("Moving {n} item to “{to}”", "Moving {n} items to “{to}”", n),
        )
        .replace("{to}", &to()),
        OpKind::Link => pick(
            gettext("Creating a link to “{name}”").replace("{name}", &one()),
            ngettext("Creating {n} link", "Creating {n} links", n),
        ),
        OpKind::Duplicate => pick(
            gettext("Duplicating “{name}”").replace("{name}", &one()),
            ngettext("Duplicating {n} item", "Duplicating {n} items", n),
        ),
        OpKind::Trash => pick(
            gettext("Moving “{name}” to the Trash").replace("{name}", &one()),
            ngettext(
                "Moving {n} item to the Trash",
                "Moving {n} items to the Trash",
                n,
            ),
        ),
        OpKind::Delete => pick(
            gettext("Deleting “{name}”").replace("{name}", &one()),
            ngettext("Deleting {n} item", "Deleting {n} items", n),
        ),
        OpKind::Restore => pick(
            gettext("Restoring “{name}”").replace("{name}", &one()),
            ngettext("Restoring {n} item", "Restoring {n} items", n),
        ),
        OpKind::EmptyTrash => gettext("Emptying the Trash"),
        OpKind::Rename => gettext("Renaming “{name}”").replace("{name}", &one()),
        OpKind::CreateFolder | OpKind::CreateFile => {
            gettext("Creating “{name}”").replace("{name}", &one())
        }
        OpKind::BatchRename => {
            ngettext("Renaming {n} item", "Renaming {n} items", n).replace("{n}", &n.to_string())
        }
        OpKind::Extract => pick(
            gettext("Extracting “{name}”").replace("{name}", &one()),
            ngettext("Extracting {n} archive", "Extracting {n} archives", n),
        ),
        OpKind::Compress => {
            let Operation::Compress { target, .. } = operation else {
                unreachable!()
            };
            pick(
                gettext("Compressing “{name}” into “{archive}”").replace("{name}", &one()),
                ngettext(
                    "Compressing {n} item into “{archive}”",
                    "Compressing {n} items into “{archive}”",
                    n,
                ),
            )
            .replace("{archive}", &file_name(target))
        }
    }
}
