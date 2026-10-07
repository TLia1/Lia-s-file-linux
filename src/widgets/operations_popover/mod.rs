//! `OperationsButton`: a header bar button that appears while file
//! operations run. It shows their combined progress as a ring; its popover
//! lists each job with a progress bar, speed, time left, pause and cancel.
//! Finished jobs fade out.

use std::cell::{Cell, OnceCell, RefCell};

use gettextrs::gettext;
use gtk::{glib, glib::clone, prelude::*, subclass::prelude::*};

use crate::services::file_ops::{FileOpJob, JobState, queue};

/// Jobs must run this long before the button appears (no flicker for
/// quick ones).
const REVEAL_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

mod ring {
    use super::*;
    use std::f64::consts::PI;

    mod imp {
        use super::*;

        #[derive(Default, glib::Properties)]
        #[properties(wrapper_type = super::ProgressRing)]
        pub struct ProgressRing {
            /// 0..1, or negative while unknown (drawn as a spinning arc).
            #[property(get, set = Self::set_fraction)]
            pub(super) fraction: Cell<f64>,
            pub(super) angle: Cell<f64>,
            pub(super) tick: RefCell<Option<gtk::TickCallbackId>>,
        }

        #[glib::object_subclass]
        impl ObjectSubclass for ProgressRing {
            const NAME: &'static str = "LiaProgressRing";
            type Type = super::ProgressRing;
            type ParentType = gtk::Widget;

            fn class_init(klass: &mut Self::Class) {
                klass.set_css_name("progress-ring");
                klass.set_accessible_role(gtk::AccessibleRole::ProgressBar);
            }
        }

        #[glib::derived_properties]
        impl ObjectImpl for ProgressRing {}

        impl WidgetImpl for ProgressRing {
            fn measure(&self, _: gtk::Orientation, _: i32) -> (i32, i32, i32, i32) {
                (16, 16, -1, -1)
            }

            fn unmap(&self) {
                if let Some(tick) = self.tick.take() {
                    tick.remove();
                }
                self.parent_unmap();
            }

            fn map(&self) {
                self.parent_map();
                self.update_animation();
            }

            fn snapshot(&self, snapshot: &gtk::Snapshot) {
                let widget = self.obj();
                let (w, h) = (widget.width() as f64, widget.height() as f64);
                let size = w.min(h);
                let line = 2.5;
                let radius = (size - line) / 2.0;
                let bounds = gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
                let cr = snapshot.append_cairo(&bounds);
                let color = widget.color();
                cr.set_line_width(line);
                cr.set_line_cap(gtk::cairo::LineCap::Round);

                cr.set_source_rgba(
                    color.red() as f64,
                    color.green() as f64,
                    color.blue() as f64,
                    0.2,
                );
                cr.arc(w / 2.0, h / 2.0, radius, 0.0, 2.0 * PI);
                let _ = cr.stroke();

                let fraction = self.fraction.get();
                let (start, end) = if fraction < 0.0 {
                    let start = self.angle.get();
                    (start, start + PI / 2.0)
                } else {
                    (-PI / 2.0, -PI / 2.0 + 2.0 * PI * fraction.min(1.0))
                };
                cr.set_source_rgba(
                    color.red() as f64,
                    color.green() as f64,
                    color.blue() as f64,
                    color.alpha() as f64,
                );
                cr.arc(w / 2.0, h / 2.0, radius, start, end);
                let _ = cr.stroke();
            }
        }

        impl ProgressRing {
            fn set_fraction(&self, fraction: f64) {
                if self.fraction.replace(fraction) != fraction {
                    self.obj().queue_draw();
                    self.update_animation();
                }
            }

            /// Spins while the progress is unknown.
            fn update_animation(&self) {
                let obj = self.obj();
                let spinning = self.fraction.get() < 0.0 && obj.is_mapped();
                if spinning == self.tick.borrow().is_some() {
                    return;
                }
                if !spinning {
                    if let Some(tick) = self.tick.take() {
                        tick.remove();
                    }
                    return;
                }
                let tick = obj.add_tick_callback(|ring, clock| {
                    let seconds = clock.frame_time() as f64 / 1_000_000.0;
                    ring.imp().angle.set((seconds * 1.5 * PI) % (2.0 * PI));
                    ring.queue_draw();
                    glib::ControlFlow::Continue
                });
                self.tick.replace(Some(tick));
            }
        }
    }

    glib::wrapper! {
        pub struct ProgressRing(ObjectSubclass<imp::ProgressRing>)
            @extends gtk::Widget,
            @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
    }

    impl Default for ProgressRing {
        fn default() -> Self {
            glib::Object::new()
        }
    }
}

use ring::ProgressRing;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct OperationsButton {
        pub(super) revealer: OnceCell<gtk::Revealer>,
        pub(super) button: OnceCell<gtk::MenuButton>,
        pub(super) ring: OnceCell<ProgressRing>,
        pub(super) reveal_timer: RefCell<Option<glib::SourceId>>,
        pub(super) handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for OperationsButton {
        const NAME: &'static str = "LiaOperationsButton";
        type Type = super::OperationsButton;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("operations-button");
        }
    }

    impl ObjectImpl for OperationsButton {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().build();
        }

        fn dispose(&self) {
            for (object, handler) in self.handlers.take() {
                object.disconnect(handler);
            }
            if let Some(timer) = self.reveal_timer.take() {
                timer.remove();
            }
            if let Some(revealer) = self.revealer.get() {
                revealer.unparent();
            }
        }
    }

    impl WidgetImpl for OperationsButton {}
}

glib::wrapper! {
    pub struct OperationsButton(ObjectSubclass<imp::OperationsButton>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for OperationsButton {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl OperationsButton {
    fn build(&self) {
        let imp = self.imp();
        let queue = queue();

        let ring = ProgressRing::default();
        ring.add_css_class("accent");
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["operations-list"])
            .build();
        list.bind_model(Some(&queue.jobs()), |object| {
            job_row(object.downcast_ref::<FileOpJob>().unwrap()).upcast()
        });
        let scroller = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(420)
            .build();
        let title = gtk::Label::builder()
            .label(gettext("File Operations"))
            .xalign(0.0)
            .css_classes(["heading", "operations-title"])
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .width_request(360)
            .build();
        content.append(&title);
        content.append(&scroller);
        let popover = gtk::Popover::builder()
            .child(&content)
            .css_classes(["operations-popover"])
            .build();

        let button = gtk::MenuButton::builder()
            .child(&ring)
            .popover(&popover)
            .tooltip_text(gettext("File Operations"))
            .css_classes(["flat"])
            .build();
        let revealer = gtk::Revealer::builder()
            .child(&button)
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .transition_duration(200)
            .build();
        revealer.set_parent(self);
        // Hidden revealers still take no space but keep the header tidy.
        revealer
            .bind_property("child-revealed", self, "visible")
            .sync_create()
            .build();
        revealer.connect_reveal_child_notify(clone!(
            #[weak(rename_to = this)]
            self,
            move |revealer| {
                if revealer.reveals_child() {
                    this.set_visible(true);
                }
            }
        ));

        queue
            .bind_property("progress", &ring, "fraction")
            .sync_create()
            .build();
        let jobs = queue.jobs();
        let handler = jobs.connect_items_changed(clone!(
            #[weak(rename_to = this)]
            self,
            move |jobs, _, _, _| this.on_jobs_changed(jobs.n_items())
        ));
        imp.handlers.replace(vec![(jobs.upcast(), handler)]);

        imp.revealer.set(revealer).unwrap();
        imp.button.set(button).unwrap();
        imp.ring.set(ring).unwrap();
        self.on_jobs_changed(queue.jobs().n_items());
    }

    fn on_jobs_changed(&self, n_jobs: u32) {
        let imp = self.imp();
        let revealer = imp.revealer.get().unwrap();
        if n_jobs == 0 {
            if let Some(timer) = imp.reveal_timer.take() {
                timer.remove();
            }
            imp.button.get().unwrap().popdown();
            revealer.set_reveal_child(false);
            return;
        }
        if revealer.reveals_child() || imp.reveal_timer.borrow().is_some() {
            return;
        }
        let timer = glib::timeout_add_local_once(
            REVEAL_DELAY,
            clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.imp().reveal_timer.take();
                    if queue().jobs().n_items() > 0 {
                        this.imp().revealer.get().unwrap().set_reveal_child(true);
                    }
                }
            ),
        );
        imp.reveal_timer.replace(Some(timer));
    }

    /// Opens the popover (e.g. when a long operation starts).
    pub fn popup(&self) {
        if let Some(button) = self.imp().button.get() {
            button.popup();
        }
    }
}

/// One job in the list.
fn job_row(job: &FileOpJob) -> gtk::ListBoxRow {
    let title = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .css_classes(["job-title"])
        .build();
    let pause = gtk::Button::builder()
        .icon_name("media-playback-pause-symbolic")
        .tooltip_text(gettext("Pause"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    let cancel = gtk::Button::builder()
        .icon_name("process-stop-symbolic")
        .tooltip_text(gettext("Cancel"))
        .valign(gtk::Align::Center)
        .css_classes(["flat", "circular"])
        .build();
    let header = gtk::Box::builder().spacing(4).build();
    header.append(&title);
    header.append(&pause);
    header.append(&cancel);

    let bar = gtk::ProgressBar::builder()
        .css_classes(["job-progress"])
        .build();
    let status = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["caption", "numeric", "dim-label"])
        .build();
    let current = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .css_classes(["caption", "dim-label"])
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .build();
    content.append(&header);
    content.append(&bar);
    content.append(&status);
    content.append(&current);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .activatable(false)
        .css_classes(["job-row"])
        .build();

    job.bind_property("title", &title, "label")
        .sync_create()
        .build();
    job.bind_property("title", &title, "tooltip-text")
        .sync_create()
        .build();
    job.bind_property("status-text", &status, "label")
        .sync_create()
        .build();
    job.bind_property("current-file", &current, "label")
        .sync_create()
        .build();

    // An unknown fraction pulses while the job is active.
    let pulse: std::rc::Rc<RefCell<Option<glib::SourceId>>> = Default::default();
    let update = clone!(
        #[weak]
        bar,
        #[weak]
        pause,
        #[weak]
        cancel,
        #[weak]
        current,
        #[weak]
        row,
        #[strong]
        pulse,
        move |job: &FileOpJob| {
            let state = job.state();
            let finished = state.is_finished();
            let fraction = job.progress();
            if fraction >= 0.0 || finished {
                if let Some(id) = pulse.take() {
                    id.remove();
                }
                bar.set_fraction(if finished && state == JobState::Done {
                    1.0
                } else {
                    fraction.max(0.0)
                });
            } else if pulse.borrow().is_none() {
                let id = glib::timeout_add_local(
                    std::time::Duration::from_millis(120),
                    clone!(
                        #[weak]
                        bar,
                        #[upgrade_or]
                        glib::ControlFlow::Break,
                        move || {
                            bar.pulse();
                            glib::ControlFlow::Continue
                        }
                    ),
                );
                pulse.replace(Some(id));
            }
            pause.set_visible(!finished && state != JobState::Queued);
            cancel.set_visible(!finished);
            current.set_visible(!finished && !job.current_file().is_empty());
            let paused = state == JobState::Paused;
            pause.set_icon_name(if paused {
                "media-playback-start-symbolic"
            } else {
                "media-playback-pause-symbolic"
            });
            pause.set_tooltip_text(Some(&if paused {
                gettext("Resume")
            } else {
                gettext("Pause")
            }));
            if finished {
                row.add_css_class("finished");
            }
            if state == JobState::Failed {
                row.add_css_class("failed");
            }
        }
    );
    update(job);
    job.connect_state_notify(clone!(
        #[strong]
        update,
        move |job| update(job)
    ));
    job.connect_progress_notify(clone!(
        #[strong]
        update,
        move |job| update(job)
    ));
    job.connect_current_file_notify(move |job| update(job));
    row.connect_destroy(move |_| {
        if let Some(id) = pulse.take() {
            id.remove();
        }
    });

    pause.connect_clicked(clone!(
        #[weak]
        job,
        move |_| job.set_paused(job.state() != JobState::Paused)
    ));
    cancel.connect_clicked(clone!(
        #[weak]
        job,
        move |_| queue().cancel(&job)
    ));
    row
}
