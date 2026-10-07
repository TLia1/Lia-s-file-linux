//! `JobQueue`: runs file operations in the background.
//!
//! - Jobs writing to the same device run one after another (configurable
//!   with the `jobs-per-device` setting); jobs on different devices run in
//!   parallel. Quick jobs (rename, new folder…) start immediately.
//! - Each running job has a worker thread executing [`engine::run`]. The
//!   worker sends questions (conflicts, errors) and its outcome over a
//!   channel; the UI thread answers with dialogs and polls the progress.
//! - Finished jobs update the undo history and are announced with the
//!   `job-finished` signal (windows show a toast).

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use gtk::{gio, glib, glib::clone, prelude::*, subclass::prelude::*};

use super::engine;
use super::job::{FileOpJob, JobState, Purpose};
use super::operation::*;
use crate::dialogs;
use crate::services::undo::undo_manager;
use crate::util::settings;

/// How often running jobs' progress is copied to their properties.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// Finished jobs stay in the popover this long.
const KEEP_FINISHED: Duration = Duration::from_secs(4);
/// Jobs finishing faster than this are never shown in the popover.
const SHOW_AFTER: f64 = 0.5;

/// Worker → UI messages.
enum Message {
    Conflict(Conflict, async_channel::Sender<ConflictResponse>),
    Error(OpError, async_channel::Sender<ErrorResponse>),
    Finished(Outcome),
}

/// The worker's side of [`Interaction`]: forwards questions to the UI
/// thread and blocks until they're answered.
struct Bridge {
    sender: async_channel::Sender<Message>,
}

impl Interaction for Bridge {
    fn conflict(&self, conflict: &Conflict) -> ConflictResponse {
        let (reply, answer) = async_channel::bounded(1);
        let cancel = ConflictResponse {
            choice: ConflictChoice::Cancel,
            apply_to_all: false,
        };
        if self
            .sender
            .send_blocking(Message::Conflict(conflict.clone(), reply))
            .is_err()
        {
            return cancel;
        }
        answer.recv_blocking().unwrap_or(cancel)
    }

    fn error(&self, error: &OpError) -> ErrorResponse {
        let (reply, answer) = async_channel::bounded(1);
        let cancel = ErrorResponse {
            choice: ErrorChoice::Cancel,
            apply_to_all: false,
        };
        if self
            .sender
            .send_blocking(Message::Error(error.clone(), reply))
            .is_err()
        {
            return cancel;
        }
        answer.recv_blocking().unwrap_or(cancel)
    }
}

mod imp {
    use super::*;

    #[derive(glib::Properties)]
    #[properties(wrapper_type = super::JobQueue)]
    pub struct JobQueue {
        /// Jobs shown in the operations popover (not the quick ones).
        pub(super) jobs: gio::ListStore,
        /// Jobs waiting for their device to be free.
        pub(super) waiting: RefCell<Vec<FileOpJob>>,
        /// Device → number of running jobs.
        pub(super) running: RefCell<HashMap<String, usize>>,
        pub(super) devices: RefCell<HashMap<u64, String>>,
        /// Jobs not finished yet.
        #[property(get)]
        pub(super) n_active: Cell<u32>,
        /// Combined progress of the active jobs, or -1 while unknown.
        #[property(get)]
        pub(super) progress: Cell<f64>,
    }

    impl Default for JobQueue {
        fn default() -> Self {
            Self {
                jobs: gio::ListStore::new::<FileOpJob>(),
                waiting: RefCell::default(),
                running: RefCell::default(),
                devices: RefCell::default(),
                n_active: Cell::new(0),
                progress: Cell::new(-1.0),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for JobQueue {
        const NAME: &'static str = "LiaJobQueue";
        type Type = super::JobQueue;
    }

    #[glib::derived_properties]
    impl ObjectImpl for JobQueue {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    glib::subclass::Signal::builder("job-finished")
                        .param_types([FileOpJob::static_type()])
                        .build(),
                ]
            })
        }
    }
}

glib::wrapper! {
    pub struct JobQueue(ObjectSubclass<imp::JobQueue>);
}

thread_local! {
    static QUEUE: OnceCell<JobQueue> = const { OnceCell::new() };
}

/// The application's job queue.
pub fn queue() -> JobQueue {
    QUEUE.with(|cell| cell.get_or_init(glib::Object::new).clone())
}

impl JobQueue {
    /// The jobs shown in the operations popover.
    pub fn jobs(&self) -> gio::ListStore {
        self.imp().jobs.clone()
    }

    /// Starts a user action. `origin` is the widget it was started from;
    /// questions and the final toast appear in its window.
    pub fn submit(
        &self,
        operation: Operation,
        origin: Option<&impl IsA<gtk::Widget>>,
    ) -> FileOpJob {
        self.submit_with(operation, Purpose::Action, origin.map(|w| w.upcast_ref()))
    }

    /// Undoes the last action. Returns false if there is nothing to undo.
    pub fn undo(&self, origin: Option<&impl IsA<gtk::Widget>>) -> bool {
        let Some(record) = undo_manager().with(|stack| stack.pop_undo()) else {
            return false;
        };
        let operation = record.inverse.clone();
        self.submit_with(
            operation,
            Purpose::Undo(record),
            origin.map(|w| w.upcast_ref()),
        );
        true
    }

    /// Redoes the last undone action. Returns false if there is none.
    pub fn redo(&self, origin: Option<&impl IsA<gtk::Widget>>) -> bool {
        let Some(record) = undo_manager().with(|stack| stack.pop_redo()) else {
            return false;
        };
        let operation = record.forward.clone();
        self.submit_with(
            operation,
            Purpose::Redo(record),
            origin.map(|w| w.upcast_ref()),
        );
        true
    }

    /// Cancels a job, waiting or running.
    pub fn cancel(&self, job: &FileOpJob) {
        if job.is_finished() {
            return;
        }
        let waiting = {
            let mut waiting = self.imp().waiting.borrow_mut();
            let position = waiting.iter().position(|j| j == job);
            position.map(|p| waiting.remove(p))
        };
        if waiting.is_some() {
            let outcome = Outcome {
                status: Status::Cancelled,
                undo: None,
                created: Vec::new(),
                errors: Vec::new(),
                skipped: 0,
                succeeded: 0,
            };
            self.on_finished(job, None, outcome);
        } else {
            job.control().cancel();
            // Shown right away; this also closes a question it was asking.
            job.set_state(JobState::Cancelled);
        }
    }

    pub fn connect_job_finished<F: Fn(&Self, &FileOpJob) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "job-finished",
            false,
            glib::closure_local!(move |queue: &Self, job: &FileOpJob| f(queue, job)),
        )
    }

    fn submit_with(
        &self,
        operation: Operation,
        purpose: Purpose,
        origin: Option<&gtk::Widget>,
    ) -> FileOpJob {
        let quick = operation.is_quick();
        let job = FileOpJob::new(operation, purpose, origin);
        self.set_n_active(self.n_active() + 1);
        if quick {
            self.start(&job, None);
            return job;
        }
        self.imp().jobs.append(&job);
        self.imp().waiting.borrow_mut().push(job.clone());
        job.connect_progress_notify(clone!(
            #[weak(rename_to = queue)]
            self,
            move |_| queue.update_progress()
        ));
        glib::spawn_future_local(clone!(
            #[weak(rename_to = queue)]
            self,
            #[strong]
            job,
            async move {
                let device = device_of(job.operation()).await;
                queue.imp().devices.borrow_mut().insert(job.id(), device);
                queue.schedule();
            }
        ));
        job
    }

    /// Starts waiting jobs whose device has a free slot.
    fn schedule(&self) {
        let imp = self.imp();
        let limit = settings().int("jobs-per-device").max(1) as usize;
        let ready: Vec<(FileOpJob, String)> = {
            let mut waiting = imp.waiting.borrow_mut();
            let mut running = imp.running.borrow_mut();
            let devices = imp.devices.borrow();
            let mut ready = Vec::new();
            waiting.retain(|job| {
                let Some(device) = devices.get(&job.id()) else {
                    return true;
                };
                let count = running.entry(device.clone()).or_default();
                if *count >= limit {
                    return true;
                }
                *count += 1;
                ready.push((job.clone(), device.clone()));
                false
            });
            ready
        };
        for (job, device) in ready {
            self.start(&job, Some(device));
        }
    }

    fn start(&self, job: &FileOpJob, device: Option<String>) {
        job.set_state(JobState::Running);
        let (sender, receiver) = async_channel::unbounded();
        let control = job.control();
        let operation = job.operation().clone();
        let spawned = std::thread::Builder::new()
            .name("lia-file-op".into())
            .spawn(move || {
                let bridge = Bridge {
                    sender: sender.clone(),
                };
                let outcome = engine::run(&operation, &control, &bridge);
                let _ = sender.send_blocking(Message::Finished(outcome));
            });
        if let Err(err) = spawned {
            tracing::error!("cannot start a worker thread: {err}");
            let outcome = Outcome {
                status: Status::Cancelled,
                undo: None,
                created: Vec::new(),
                errors: vec![err.to_string()],
                skipped: 0,
                succeeded: 0,
            };
            self.on_finished(job, device, outcome);
            return;
        }

        let poll = glib::timeout_add_local(
            POLL_INTERVAL,
            clone!(
                #[weak]
                job,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    job.sync_progress();
                    glib::ControlFlow::Continue
                }
            ),
        );
        glib::spawn_future_local(clone!(
            #[weak(rename_to = queue)]
            self,
            #[strong]
            job,
            async move {
                let mut poll = Some(poll);
                while let Ok(message) = receiver.recv().await {
                    match message {
                        Message::Conflict(conflict, reply) => {
                            let response = dialogs::file_ops::ask_conflict(&job, &conflict).await;
                            let _ = reply.send(response).await;
                        }
                        Message::Error(error, reply) => {
                            let response = dialogs::file_ops::ask_error(&job, &error).await;
                            let _ = reply.send(response).await;
                        }
                        Message::Finished(outcome) => {
                            if let Some(poll) = poll.take() {
                                poll.remove();
                            }
                            queue.on_finished(&job, device, outcome);
                            break;
                        }
                    }
                }
            }
        ));
    }

    fn on_finished(&self, job: &FileOpJob, device: Option<String>, mut outcome: Outcome) {
        let imp = self.imp();
        if let Some(device) = &device
            && let Some(count) = imp.running.borrow_mut().get_mut(device)
        {
            *count = count.saturating_sub(1);
        }
        imp.devices.borrow_mut().remove(&job.id());

        // Undo bookkeeping.
        let undo = undo_manager();
        let nothing_done = outcome.succeeded == 0;
        match job.take_purpose() {
            Purpose::Action => {
                if let Some(record) = outcome.undo.take() {
                    undo.with(|stack| stack.record(record));
                    job.set_undoable();
                }
            }
            Purpose::Undo(record) => undo.with(|stack| {
                if nothing_done {
                    stack.restore_undo(record);
                } else {
                    stack.push_undone(record);
                }
            }),
            Purpose::Redo(record) => undo.with(|stack| match outcome.undo.take() {
                Some(new_record) => stack.push_redone(new_record),
                None if nothing_done => stack.restore_redo(record),
                None => {}
            }),
        }

        job.finish(outcome);
        self.set_n_active(self.n_active().saturating_sub(1));
        self.update_progress();
        self.emit_by_name::<()>("job-finished", &[job]);

        // Leave the popover: right away if it was too quick to matter.
        let delay = if job.elapsed() < SHOW_AFTER {
            Duration::ZERO
        } else {
            KEEP_FINISHED
        };
        glib::timeout_add_local_once(
            delay,
            clone!(
                #[weak(rename_to = queue)]
                self,
                #[weak]
                job,
                move || {
                    let jobs = &queue.imp().jobs;
                    if let Some(position) = jobs.find(&job) {
                        jobs.remove(position);
                    }
                }
            ),
        );
        self.schedule();
    }

    fn set_n_active(&self, n: u32) {
        if self.imp().n_active.replace(n) != n {
            self.notify_n_active();
        }
    }

    /// Combined progress: bytes when known, otherwise the average fraction.
    fn update_progress(&self) {
        let jobs = &self.imp().jobs;
        let active: Vec<FileOpJob> = (0..jobs.n_items())
            .filter_map(|i| jobs.item(i).and_downcast::<FileOpJob>())
            .filter(|job| !job.is_finished())
            .collect();
        let total: u64 = active.iter().map(|j| j.bytes_total()).sum();
        let progress = if active.is_empty() {
            1.0
        } else if total > 0 && active.iter().all(|j| j.bytes_total() > 0) {
            active.iter().map(|j| j.bytes_done()).sum::<u64>() as f64 / total as f64
        } else {
            let known: Vec<f64> = active
                .iter()
                .map(|j| j.progress())
                .filter(|p| *p >= 0.0)
                .collect();
            if known.is_empty() {
                -1.0
            } else {
                known.iter().sum::<f64>() / active.len() as f64
            }
        };
        if self.imp().progress.replace(progress) != progress {
            self.notify_progress();
        }
    }
}

/// Identifies the device an operation writes to (or deletes from).
async fn device_of(operation: &Operation) -> String {
    let probe = match operation {
        Operation::EmptyTrash | Operation::Restore { .. } => return "trash".into(),
        _ => operation
            .destination()
            .or_else(|| operation.sources().first().and_then(|f| f.parent())),
    };
    let Some(probe) = probe else {
        return String::new();
    };
    let fallback = || probe.uri_scheme().map(String::from).unwrap_or_default();
    match probe
        .query_info_future(
            gio::FILE_ATTRIBUTE_ID_FILESYSTEM,
            gio::FileQueryInfoFlags::NONE,
            glib::Priority::DEFAULT,
        )
        .await
    {
        Ok(info) => info
            .attribute_string(gio::FILE_ATTRIBUTE_ID_FILESYSTEM)
            .map(String::from)
            .unwrap_or_else(fallback),
        Err(_) => fallback(),
    }
}
