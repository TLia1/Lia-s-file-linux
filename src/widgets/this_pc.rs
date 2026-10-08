//! `ThisPcPage`: the "This PC" overview (`computer:///`), Explorer style —
//! a card per drive with its usage bar and free space, network locations
//! below. Unmounted volumes are mounted when opened; removable drives get
//! an eject button. The cards follow the `gio::VolumeMonitor` live.

use std::cell::{Cell, OnceCell, RefCell};
use std::sync::OnceLock;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gdk, gio, glib, glib::clone, subclass::prelude::*};

use crate::services::volumes::{self, Device};

/// What a card opens.
#[derive(Clone)]
enum Entry {
    FileSystem,
    Device(Device),
}

impl Entry {
    fn location(&self) -> Option<gio::File> {
        match self {
            Self::FileSystem => Some(gio::File::for_path("/")),
            Self::Device(device) => device.location(),
        }
    }
}

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::ThisPcPage)]
    pub struct ThisPcPage {
        pub(super) scroller: gtk::ScrolledWindow,
        pub(super) drives_box: gtk::FlowBox,
        pub(super) network_heading: gtk::Label,
        pub(super) network_box: gtk::FlowBox,
        /// The entries of both boxes, by card widget.
        pub(super) entries: RefCell<Vec<(gtk::FlowBoxChild, Entry)>>,
        /// Number of cards, for the status bar.
        #[property(get)]
        pub(super) n_devices: Cell<u32>,
        pub(super) monitor: OnceCell<gio::VolumeMonitor>,
        pub(super) handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub(super) rebuild_queued: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ThisPcPage {
        const NAME: &'static str = "LiaThisPcPage";
        type Type = super::ThisPcPage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("this-pc");
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for ThisPcPage {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    // (location, in a new tab)
                    glib::subclass::Signal::builder("open-location")
                        .param_types([gio::File::static_type(), bool::static_type()])
                        .build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }

        fn dispose(&self) {
            if let Some(monitor) = self.monitor.get() {
                for handler in self.handlers.take() {
                    monitor.disconnect(handler);
                }
            }
            self.scroller.unparent();
        }
    }

    impl WidgetImpl for ThisPcPage {
        fn map(&self) {
            self.parent_map();
            // Free space changes while the page isn't shown.
            self.obj().rebuild();
        }
    }
}

glib::wrapper! {
    pub struct ThisPcPage(ObjectSubclass<imp::ThisPcPage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ThisPcPage {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ThisPcPage {
    fn setup(&self) {
        let imp = self.imp();
        let heading = |label: &str| {
            gtk::Label::builder()
                .label(label)
                .xalign(0.0)
                .css_classes(["heading", "this-pc-heading"])
                .build()
        };
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(16)
            .margin_bottom(24)
            .margin_start(16)
            .margin_end(16)
            .build();
        content.append(&heading(&gettext("Devices and Drives")));
        content.append(&imp.drives_box);
        imp.network_heading.set_label(&gettext("Network Locations"));
        imp.network_heading.set_xalign(0.0);
        imp.network_heading
            .set_css_classes(&["heading", "this-pc-heading"]);
        content.append(&imp.network_heading);
        content.append(&imp.network_box);

        for flow_box in [&imp.drives_box, &imp.network_box] {
            flow_box.set_selection_mode(gtk::SelectionMode::None);
            flow_box.set_homogeneous(true);
            flow_box.set_min_children_per_line(1);
            flow_box.set_max_children_per_line(4);
            flow_box.set_column_spacing(8);
            flow_box.set_row_spacing(8);
            flow_box.set_valign(gtk::Align::Start);
            flow_box.connect_child_activated(clone!(
                #[weak(rename_to = page)]
                self,
                move |_, child| page.open(child, false)
            ));
            // Middle-click opens in a new tab.
            let click = gtk::GestureClick::builder()
                .button(gdk::BUTTON_MIDDLE)
                .build();
            click.connect_pressed(clone!(
                #[weak(rename_to = page)]
                self,
                #[weak]
                flow_box,
                move |gesture, _, x, y| {
                    if let Some(child) = flow_box.child_at_pos(x as i32, y as i32) {
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        page.open(&child, true);
                    }
                }
            ));
            flow_box.add_controller(click);
        }

        imp.scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
        imp.scroller.set_child(Some(&content));
        imp.scroller.set_parent(self);

        let monitor = gio::VolumeMonitor::get();
        let rebuild = clone!(
            #[weak(rename_to = page)]
            self,
            move || page.queue_rebuild()
        );
        let handlers = vec![
            monitor.connect_mount_added(clone!(
                #[strong]
                rebuild,
                move |_, _| rebuild()
            )),
            monitor.connect_mount_removed(clone!(
                #[strong]
                rebuild,
                move |_, _| rebuild()
            )),
            monitor.connect_mount_changed(clone!(
                #[strong]
                rebuild,
                move |_, _| rebuild()
            )),
            monitor.connect_volume_added(clone!(
                #[strong]
                rebuild,
                move |_, _| rebuild()
            )),
            monitor.connect_volume_removed(clone!(
                #[strong]
                rebuild,
                move |_, _| rebuild()
            )),
            monitor.connect_volume_changed(move |_, _| rebuild()),
        ];
        imp.handlers.replace(handlers);
        imp.monitor.set(monitor).unwrap();
        self.rebuild();
    }

    pub fn connect_open_location<F: Fn(&Self, &gio::File, bool) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "open-location",
            false,
            glib::closure_local!(move |page: &Self, file: &gio::File, new_tab: bool| {
                f(page, file, new_tab)
            }),
        )
    }

    fn queue_rebuild(&self) {
        if self.imp().rebuild_queued.replace(true) {
            return;
        }
        glib::idle_add_local_once(clone!(
            #[weak(rename_to = page)]
            self,
            move || {
                page.imp().rebuild_queued.set(false);
                page.rebuild();
            }
        ));
    }

    fn rebuild(&self) {
        let imp = self.imp();
        imp.drives_box.remove_all();
        imp.network_box.remove_all();
        let mut entries = vec![Entry::FileSystem];
        entries.extend(
            volumes::devices(imp.monitor.get().unwrap())
                .into_iter()
                .map(Entry::Device),
        );
        let mut cards = Vec::new();
        let mut network = 0;
        for entry in entries {
            let card = self.build_card(&entry);
            if matches!(&entry, Entry::Device(d) if d.is_network()) {
                imp.network_box.append(&card);
                network += 1;
            } else {
                imp.drives_box.append(&card);
            }
            cards.push((card, entry));
        }
        imp.network_heading.set_visible(network > 0);
        imp.network_box.set_visible(network > 0);
        let count = cards.len() as u32;
        imp.entries.replace(cards);
        if imp.n_devices.replace(count) != count {
            self.notify_n_devices();
        }
    }

    fn build_card(&self, entry: &Entry) -> gtk::FlowBoxChild {
        let (name, icon) = match entry {
            Entry::FileSystem => (
                gettext("File System"),
                gio::ThemedIcon::from_names(&["drive-harddisk-system", "drive-harddisk"]).upcast(),
            ),
            Entry::Device(device) => (device.name(), device.icon()),
        };
        let image = gtk::Image::builder()
            .gicon(&icon)
            .pixel_size(48)
            .css_classes(["lowres-icon"])
            .build();
        let name_label = gtk::Label::builder()
            .label(&name)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["heading"])
            .build();
        let bar = volumes::usage_bar();
        let caption = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .css_classes(["caption", "secondary", "numeric"])
            .build();
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .build();
        text.append(&name_label);
        text.append(&bar);
        text.append(&caption);
        let content = gtk::Box::builder()
            .spacing(12)
            .css_classes(["card", "drive-card"])
            .build();
        content.append(&image);
        content.append(&text);

        let device = match entry {
            Entry::Device(device) => Some(device),
            Entry::FileSystem => None,
        };
        if let Some(mount) = device.and_then(Device::removable_mount) {
            let button = gtk::Button::builder()
                .icon_name("media-eject-symbolic")
                .valign(gtk::Align::Center)
                .tooltip_text(if mount.can_eject() {
                    gettext("Eject")
                } else {
                    gettext("Unmount")
                })
                .css_classes(["flat", "circular"])
                .build();
            button.connect_clicked(clone!(
                #[weak(rename_to = page)]
                self,
                move |_| page.eject(&mount)
            ));
            content.append(&button);
        }

        match entry.location() {
            Some(location) => {
                caption.set_label(&location.parse_name());
                fill_usage(&bar, &caption, location);
            }
            None => caption.set_label(&gettext("Not mounted — open to mount")),
        }

        let child = gtk::FlowBoxChild::builder().child(&content).build();
        child.update_property(&[gtk::accessible::Property::Label(&name)]);
        child.set_tooltip_text(Some(&name));
        child
    }

    fn entry_for(&self, child: &gtk::FlowBoxChild) -> Option<Entry> {
        self.imp()
            .entries
            .borrow()
            .iter()
            .find(|(card, _)| card == child)
            .map(|(_, entry)| entry.clone())
    }

    fn open(&self, child: &gtk::FlowBoxChild, new_tab: bool) {
        let Some(entry) = self.entry_for(child) else {
            return;
        };
        if let Some(location) = entry.location() {
            self.emit_by_name::<()>("open-location", &[&location, &new_tab]);
            return;
        }
        let Some(volume) = (match &entry {
            Entry::Device(device) => device.volume(),
            Entry::FileSystem => None,
        }) else {
            return;
        };
        glib::spawn_future_local(clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                match volumes::mount(&volume, Some(page.upcast_ref())).await {
                    Ok(root) => page.emit_by_name::<()>("open-location", &[&root, &new_tab]),
                    Err(err) if volumes::is_handled(&err) => {}
                    Err(err) => {
                        tracing::warn!("cannot mount {}: {err}", volume.name());
                        page.show_toast(
                            &gettext("Unable to access “{}”").replace("{}", &volume.name()),
                        );
                    }
                }
            }
        ));
    }

    fn eject(&self, mount: &gio::Mount) {
        let mount = mount.clone();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = page)]
            self,
            async move {
                match volumes::eject(&mount, Some(page.upcast_ref())).await {
                    Ok(Some(message)) => page.show_toast(&message),
                    Ok(None) => {}
                    Err(err) if volumes::is_handled(&err) => {}
                    Err(err) => {
                        tracing::warn!("cannot eject {}: {err}", mount.name());
                        page.show_toast(err.message());
                    }
                }
            }
        ));
    }

    fn show_toast(&self, message: &str) {
        if let Some(window) = self.root().and_downcast::<crate::window::Window>() {
            window.show_toast(message);
        }
    }
}

/// Shows the drive's usage once known: "120 GB free of 512 GB".
fn fill_usage(bar: &gtk::LevelBar, caption: &gtk::Label, location: gio::File) {
    glib::spawn_future_local(clone!(
        #[weak]
        bar,
        #[weak]
        caption,
        async move {
            if let Some((size, free)) = volumes::usage(&location).await {
                bar.set_value((size - free) as f64 / size as f64);
                bar.set_visible(true);
                caption.set_label(&volumes::usage_text(size, free));
                caption.set_tooltip_text(Some(&location.parse_name()));
            }
        }
    ));
}
