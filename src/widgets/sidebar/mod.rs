//! `Sidebar`: the navigation pane.
//!
//! Sections: Home & Recent · Pinned (XDG user folders, then the GTK
//! bookmarks) · This PC (file system, drives and unmounted volumes from the
//! `gio::VolumeMonitor`, with usage bars and eject buttons) · Network
//! (network mounts) · Trash (with its item count).
//!
//! The list is rebuilt from scratch whenever bookmarks or mounts change;
//! there are only a few dozen rows. The row matching the active tab's
//! folder is highlighted.

use std::cell::{Cell, OnceCell, RefCell};
use std::sync::OnceLock;

use gettextrs::gettext;
use gtk::{gdk, gio, glib, glib::clone, prelude::*, subclass::prelude::*};

use crate::model::places::{self, Bookmark};
use crate::util::format;
use crate::widgets::folder_pane::display_name;

const TRASH_URI: &str = "trash:///";
const NETWORK_SCHEMES: [&str; 10] = [
    "smb",
    "sftp",
    "ssh",
    "ftp",
    "ftps",
    "dav",
    "davs",
    "afp",
    "nfs",
    "google-drive",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Section {
    Places,
    Pinned,
    Drives,
    Network,
    Trash,
}

#[derive(Clone)]
enum Target {
    File(gio::File),
    Mount(gio::Mount),
    /// A volume that isn't mounted yet; activating it mounts it.
    Volume(gio::Volume),
}

#[derive(Clone)]
struct Place {
    section: Section,
    label: String,
    icon: gio::Icon,
    target: Target,
    /// Set for entries that come from the bookmarks file (can be unpinned).
    bookmark_uri: Option<String>,
}

impl Place {
    fn file(section: Section, label: String, icon: &str, file: gio::File) -> Self {
        Self {
            section,
            label,
            icon: gio::ThemedIcon::new(icon).upcast(),
            target: Target::File(file),
            bookmark_uri: None,
        }
    }

    fn location(&self) -> Option<gio::File> {
        match &self.target {
            Target::File(file) => Some(file.clone()),
            Target::Mount(mount) => Some(mount.root()),
            Target::Volume(volume) => volume.get_mount().map(|m| m.root()),
        }
    }

    /// The mount that the eject button / menu item acts on.
    fn ejectable_mount(&self) -> Option<gio::Mount> {
        match &self.target {
            Target::Mount(mount) if mount.can_eject() || mount.can_unmount() => Some(mount.clone()),
            _ => None,
        }
    }
}

fn is_network(file: &gio::File) -> bool {
    file.uri_scheme()
        .is_some_and(|scheme| NETWORK_SCHEMES.contains(&scheme.as_str()))
}

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/lia/Files/ui/sidebar.ui")]
    pub struct Sidebar {
        #[template_child]
        pub(super) list_box: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub(super) row_menu: TemplateChild<gio::MenuModel>,
        /// One entry per row, in order.
        pub(super) places: RefCell<Vec<Place>>,
        pub(super) bookmarks: RefCell<Vec<Bookmark>>,
        /// The active tab's folder.
        pub(super) location: RefCell<Option<gio::File>>,
        pub(super) volume_monitor: OnceCell<gio::VolumeMonitor>,
        pub(super) volume_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub(super) monitors: RefCell<Vec<gio::FileMonitor>>,
        /// The trash row's icon and count label.
        pub(super) trash_widgets: RefCell<Option<(gtk::Image, gtk::Label)>>,
        pub(super) rebuild_queued: Cell<bool>,
        pub(super) menu: OnceCell<gtk::PopoverMenu>,
        /// The row the context menu was opened on.
        pub(super) menu_row: Cell<Option<usize>>,
        pub(super) actions: gio::SimpleActionGroup,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Sidebar {
        const NAME: &'static str = "LiaSidebar";
        type Type = super::Sidebar;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("lia-sidebar");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Sidebar {
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
            if let Some(monitor) = self.volume_monitor.get() {
                for handler in self.volume_handlers.take() {
                    monitor.disconnect(handler);
                }
            }
            for monitor in self.monitors.take() {
                monitor.cancel();
            }
            if let Some(menu) = self.menu.get() {
                menu.unparent();
            }
            self.dispose_template();
        }
    }

    impl WidgetImpl for Sidebar {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if let Some(menu) = self.menu.get() {
                menu.present();
            }
        }
    }
}

glib::wrapper! {
    pub struct Sidebar(ObjectSubclass<imp::Sidebar>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Sidebar {
    fn setup(&self) {
        let imp = self.imp();
        let list_box = &imp.list_box;

        list_box.set_header_func(clone!(
            #[weak(rename_to = sidebar)]
            self,
            move |row, before| sidebar.update_header(row, before)
        ));
        list_box.connect_row_activated(clone!(
            #[weak(rename_to = sidebar)]
            self,
            move |_, row| sidebar.activate(row.index() as usize, false)
        ));

        // Middle-click opens in a new tab, right-click shows the menu.
        let click = gtk::GestureClick::builder().button(0).build();
        click.connect_pressed(clone!(
            #[weak(rename_to = sidebar)]
            self,
            move |gesture, _, x, y| {
                let button = gesture.current_button();
                if button != gdk::BUTTON_MIDDLE && button != gdk::BUTTON_SECONDARY {
                    return;
                }
                let Some(row) = sidebar.imp().list_box.row_at_y(y as i32) else {
                    return;
                };
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if button == gdk::BUTTON_MIDDLE {
                    sidebar.activate(row.index() as usize, true);
                } else {
                    sidebar.popup_menu(row.index() as usize, x, y);
                }
            }
        ));
        list_box.add_controller(click);

        let menu = gtk::PopoverMenu::builder()
            .menu_model(&*imp.row_menu)
            .has_arrow(false)
            .halign(gtk::Align::Start)
            .build();
        menu.set_parent(self);
        imp.menu.set(menu).unwrap();
        self.setup_actions();

        // Mounts and volumes.
        let monitor = gio::VolumeMonitor::get();
        let rebuild = clone!(
            #[weak(rename_to = sidebar)]
            self,
            move || sidebar.queue_rebuild()
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
        imp.volume_handlers.replace(handlers);
        imp.volume_monitor.set(monitor).unwrap();

        // Bookmarks file and trash, watched for changes.
        let mut monitors = Vec::new();
        match places::bookmarks_file()
            .monitor_file(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        {
            Ok(monitor) => {
                monitor.connect_changed(clone!(
                    #[weak(rename_to = sidebar)]
                    self,
                    move |_, _, _, _| sidebar.reload_bookmarks()
                ));
                monitors.push(monitor);
            }
            Err(err) => tracing::warn!("cannot watch the bookmarks file: {err}"),
        }
        match gio::File::for_uri(TRASH_URI)
            .monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        {
            Ok(monitor) => {
                monitor.connect_changed(clone!(
                    #[weak(rename_to = sidebar)]
                    self,
                    move |_, _, _, _| sidebar.update_trash()
                ));
                monitors.push(monitor);
            }
            Err(err) => tracing::debug!("cannot watch the trash: {err}"),
        }
        imp.monitors.replace(monitors);

        // Show everything but the bookmarks right away.
        self.rebuild();
        self.reload_bookmarks();
    }

    fn setup_actions(&self) {
        let actions = &self.imp().actions;
        actions.add_action_entries([
            gio::ActionEntry::builder("open-in-new-tab")
                .activate(clone!(
                    #[weak(rename_to = sidebar)]
                    self,
                    move |_, _, _| {
                        if let Some(index) = sidebar.imp().menu_row.get() {
                            sidebar.activate(index, true);
                        }
                    }
                ))
                .build(),
            gio::ActionEntry::builder("unpin")
                .activate(clone!(
                    #[weak(rename_to = sidebar)]
                    self,
                    move |_, _, _| sidebar.unpin_menu_row()
                ))
                .build(),
            gio::ActionEntry::builder("eject")
                .activate(clone!(
                    #[weak(rename_to = sidebar)]
                    self,
                    move |_, _, _| {
                        let mount = sidebar.menu_place().and_then(|p| p.ejectable_mount());
                        if let Some(mount) = mount {
                            sidebar.eject(&mount);
                        }
                    }
                ))
                .build(),
        ]);
        self.insert_action_group("sidebar", Some(actions));
    }

    // ── Public API ─────────────────────────────────────────────────────────

    /// Highlights the entry for `location` (the active tab's folder).
    pub fn set_location(&self, location: Option<&gio::File>) {
        self.imp().location.replace(location.cloned());
        self.sync_selection();
    }

    /// Adds folders to the bookmarks file.
    pub fn pin(&self, files: &[gio::File]) {
        let uris: Vec<String> = files.iter().map(|f| f.uri().into()).collect();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                places::edit_bookmarks(|b| places::add_bookmarks(b, &uris)).await;
                sidebar.reload_bookmarks();
            }
        ));
    }

    pub fn connect_open_location<F: Fn(&Self, &gio::File, bool) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "open-location",
            false,
            glib::closure_local!(move |sidebar: &Self, file: &gio::File, new_tab: bool| {
                f(sidebar, file, new_tab)
            }),
        )
    }

    // ── Building the list ──────────────────────────────────────────────────

    fn collect_places(&self) -> Vec<Place> {
        let mut list = vec![
            Place::file(
                Section::Places,
                gettext("Home"),
                "user-home-symbolic",
                gio::File::for_path(glib::home_dir()),
            ),
            Place::file(
                Section::Places,
                gettext("Recent"),
                "document-open-recent-symbolic",
                gio::File::for_uri("recent:///"),
            ),
        ];

        for (path, icon) in places::user_folders() {
            let file = gio::File::for_path(&path);
            list.push(Place::file(
                Section::Pinned,
                display_name(&file),
                icon,
                file,
            ));
        }
        for bookmark in self.imp().bookmarks.borrow().iter() {
            let file = gio::File::for_uri(&bookmark.uri);
            if list
                .iter()
                .any(|p| p.location().is_some_and(|l| l.equal(&file)))
            {
                continue;
            }
            let label = bookmark
                .label
                .clone()
                .unwrap_or_else(|| display_name(&file));
            let icon = places::icon_name_for(&file);
            let mut place = Place::file(Section::Pinned, label, icon, file);
            place.bookmark_uri = Some(bookmark.uri.clone());
            list.push(place);
        }

        list.push(Place::file(
            Section::Drives,
            gettext("File System"),
            "drive-harddisk-symbolic",
            gio::File::for_path("/"),
        ));
        let monitor = self.imp().volume_monitor.get().unwrap();
        for mount in monitor.mounts() {
            let root = mount.root();
            if mount.is_shadowed() || root.path().is_some_and(|p| p.parent().is_none()) {
                continue;
            }
            list.push(Place {
                section: if is_network(&root) {
                    Section::Network
                } else {
                    Section::Drives
                },
                label: mount.name().into(),
                icon: mount.symbolic_icon(),
                target: Target::Mount(mount),
                bookmark_uri: None,
            });
        }
        for volume in monitor.volumes() {
            if volume.get_mount().is_some() || !volume.can_mount() {
                continue;
            }
            let network = volume
                .identifier(gio::VOLUME_IDENTIFIER_KIND_CLASS)
                .as_deref()
                == Some("network");
            list.push(Place {
                section: if network {
                    Section::Network
                } else {
                    Section::Drives
                },
                label: volume.name().into(),
                icon: volume.symbolic_icon(),
                target: Target::Volume(volume),
                bookmark_uri: None,
            });
        }
        // Keep each section contiguous (mounts and volumes interleave
        // network and local entries).
        list.sort_by_key(|p| p.section as u8);

        list.push(Place::file(
            Section::Trash,
            gettext("Trash"),
            "user-trash-symbolic",
            gio::File::for_uri(TRASH_URI),
        ));
        list
    }

    fn queue_rebuild(&self) {
        let imp = self.imp();
        if imp.rebuild_queued.replace(true) {
            return;
        }
        glib::idle_add_local_once(clone!(
            #[weak(rename_to = sidebar)]
            self,
            move || {
                sidebar.imp().rebuild_queued.set(false);
                sidebar.rebuild();
            }
        ));
    }

    fn rebuild(&self) {
        let imp = self.imp();
        let places = self.collect_places();
        imp.list_box.remove_all();
        imp.trash_widgets.replace(None);
        imp.places.replace(places);
        // The header function reads `places` while rows are appended.
        let rows: Vec<gtk::ListBoxRow> = imp
            .places
            .borrow()
            .iter()
            .map(|place| self.build_row(place))
            .collect();
        for row in rows {
            imp.list_box.append(&row);
        }
        self.update_trash();
        self.sync_selection();
    }

    fn build_row(&self, place: &Place) -> gtk::ListBoxRow {
        let image = gtk::Image::from_gicon(&place.icon);
        let label = gtk::Label::builder()
            .label(&place.label)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let text = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .hexpand(true)
            .spacing(4)
            .build();
        text.append(&label);
        let content = gtk::Box::builder().spacing(12).build();
        content.append(&image);
        content.append(&text);

        let row = gtk::ListBoxRow::builder().child(&content).build();
        let location = place.location();
        if let Some(location) = &location {
            row.set_tooltip_text(Some(&location.parse_name()));
        }

        // Usage bar for local drives.
        if place.section == Section::Drives
            && let Some(location) = location.filter(|l| l.path().is_some())
        {
            let bar = usage_bar();
            text.append(&bar);
            fill_usage(&row, &bar, location);
        }

        if place.section == Section::Trash {
            let count = gtk::Label::builder()
                .css_classes(["dim-label", "numeric", "caption"])
                .build();
            content.append(&count);
            self.imp().trash_widgets.replace(Some((image, count)));
        }

        if let Some(mount) = place.ejectable_mount() {
            let button = gtk::Button::builder()
                .icon_name("media-eject-symbolic")
                .valign(gtk::Align::Center)
                .tooltip_text(if mount.can_eject() {
                    gettext("Eject")
                } else {
                    gettext("Unmount")
                })
                .css_classes(["flat", "circular", "eject-button"])
                .build();
            button.connect_clicked(clone!(
                #[weak(rename_to = sidebar)]
                self,
                move |_| sidebar.eject(&mount)
            ));
            content.append(&button);
        }
        row
    }

    fn update_header(&self, row: &gtk::ListBoxRow, before: Option<&gtk::ListBoxRow>) {
        let places = self.imp().places.borrow();
        let section = places.get(row.index() as usize).map(|p| p.section);
        let previous = before
            .and_then(|b| places.get(b.index() as usize))
            .map(|p| p.section);
        if section.is_none() || section == previous {
            row.set_header(None::<&gtk::Widget>);
            return;
        }
        let title = match section.unwrap() {
            Section::Places => None,
            Section::Pinned => Some(gettext("Pinned")),
            Section::Drives => Some(gettext("This PC")),
            Section::Network => Some(gettext("Network")),
            Section::Trash => {
                let separator = gtk::Separator::new(gtk::Orientation::Horizontal);
                separator.add_css_class("sidebar-separator");
                row.set_header(Some(&separator));
                return;
            }
        };
        let header = title.map(|title| {
            gtk::Label::builder()
                .label(title)
                .xalign(0.0)
                .css_classes(["sidebar-heading"])
                .build()
        });
        row.set_header(header.as_ref());
    }

    fn sync_selection(&self) {
        let imp = self.imp();
        let index = imp.location.borrow().as_ref().and_then(|location| {
            imp.places
                .borrow()
                .iter()
                .position(|p| p.location().is_some_and(|l| l.equal(location)))
        });
        match index.and_then(|i| imp.list_box.row_at_index(i as i32)) {
            Some(row) => imp.list_box.select_row(Some(&row)),
            None => imp.list_box.unselect_all(),
        }
    }

    fn reload_bookmarks(&self) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                let bookmarks = places::load_bookmarks().await;
                if *sidebar.imp().bookmarks.borrow() != bookmarks {
                    sidebar.imp().bookmarks.replace(bookmarks);
                    sidebar.queue_rebuild();
                }
            }
        ));
    }

    fn update_trash(&self) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                let count = gio::File::for_uri(TRASH_URI)
                    .query_info_future(
                        gio::FILE_ATTRIBUTE_TRASH_ITEM_COUNT,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::LOW,
                    )
                    .await
                    .map(|info| info.attribute_uint32(gio::FILE_ATTRIBUTE_TRASH_ITEM_COUNT))
                    .unwrap_or(0);
                if let Some((image, label)) = sidebar.imp().trash_widgets.borrow().as_ref() {
                    image.set_icon_name(Some(if count > 0 {
                        "user-trash-full-symbolic"
                    } else {
                        "user-trash-symbolic"
                    }));
                    label.set_label(&if count > 0 {
                        count.to_string()
                    } else {
                        String::new()
                    });
                }
            }
        ));
    }

    // ── Actions ────────────────────────────────────────────────────────────

    fn activate(&self, index: usize, new_tab: bool) {
        let Some(place) = self.imp().places.borrow().get(index).cloned() else {
            return;
        };
        if let Some(location) = place.location() {
            self.emit_open(&location, new_tab);
            return;
        }
        let Target::Volume(volume) = place.target else {
            return;
        };
        let operation = gtk::MountOperation::new(self.root().and_downcast_ref::<gtk::Window>());
        glib::spawn_future_local(clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                match volume
                    .mount_future(gio::MountMountFlags::NONE, Some(&operation))
                    .await
                {
                    Ok(()) => {
                        if let Some(mount) = volume.get_mount() {
                            sidebar.emit_open(&mount.root(), new_tab);
                        }
                    }
                    Err(err) if err.matches(gio::IOErrorEnum::FailedHandled) => {}
                    Err(err) => {
                        tracing::warn!("cannot mount {}: {err}", place.label);
                        sidebar.show_toast(
                            &gettext("Unable to access “{}”").replace("{}", &place.label),
                        );
                    }
                }
            }
        ));
    }

    fn emit_open(&self, location: &gio::File, new_tab: bool) {
        self.emit_by_name::<()>("open-location", &[location, &new_tab]);
    }

    fn eject(&self, mount: &gio::Mount) {
        let operation = gtk::MountOperation::new(self.root().and_downcast_ref::<gtk::Window>());
        let mount = mount.clone();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                let name = mount.name();
                let ejecting = mount.can_eject();
                let result = if ejecting {
                    mount
                        .eject_with_operation_future(gio::MountUnmountFlags::NONE, Some(&operation))
                        .await
                } else {
                    mount
                        .unmount_with_operation_future(
                            gio::MountUnmountFlags::NONE,
                            Some(&operation),
                        )
                        .await
                };
                match result {
                    Ok(()) if ejecting => sidebar
                        .show_toast(&gettext("“{}” can be safely removed").replace("{}", &name)),
                    Ok(()) => {}
                    Err(err) if err.matches(gio::IOErrorEnum::FailedHandled) => {}
                    Err(err) => {
                        tracing::warn!("cannot eject {name}: {err}");
                        sidebar.show_toast(err.message());
                    }
                }
            }
        ));
    }

    fn menu_place(&self) -> Option<Place> {
        let index = self.imp().menu_row.get()?;
        self.imp().places.borrow().get(index).cloned()
    }

    fn unpin_menu_row(&self) {
        let Some(uri) = self.menu_place().and_then(|p| p.bookmark_uri) else {
            return;
        };
        glib::spawn_future_local(clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                places::edit_bookmarks(|b| places::remove_bookmark(b, &uri)).await;
                sidebar.reload_bookmarks();
            }
        ));
    }

    fn popup_menu(&self, index: usize, x: f64, y: f64) {
        let imp = self.imp();
        imp.menu_row.set(Some(index));
        let place = self.menu_place();
        let set_enabled = |name: &str, enabled: bool| {
            if let Some(action) = imp
                .actions
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(enabled);
            }
        };
        set_enabled(
            "unpin",
            place.as_ref().is_some_and(|p| p.bookmark_uri.is_some()),
        );
        set_enabled(
            "eject",
            place
                .as_ref()
                .is_some_and(|p| p.ejectable_mount().is_some()),
        );

        let point = imp
            .list_box
            .compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32))
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        let menu = imp.menu.get().unwrap();
        menu.set_pointing_to(Some(&gdk::Rectangle::new(
            point.x() as i32,
            point.y() as i32,
            1,
            1,
        )));
        menu.popup();
    }

    fn show_toast(&self, message: &str) {
        if let Some(window) = self.root().and_downcast::<crate::window::Window>() {
            window.show_toast(message);
        }
    }
}

/// A thin drive usage bar: accent colour, red above 90 %.
fn usage_bar() -> gtk::LevelBar {
    let bar = gtk::LevelBar::builder()
        .min_value(0.0)
        .max_value(1.0)
        .visible(false)
        .css_classes(["usage-bar"])
        .build();
    for name in [
        gtk::LEVEL_BAR_OFFSET_LOW,
        gtk::LEVEL_BAR_OFFSET_HIGH,
        gtk::LEVEL_BAR_OFFSET_FULL,
    ] {
        bar.remove_offset_value(Some(name));
    }
    bar.add_offset_value("usage-normal", 0.9);
    bar.add_offset_value("usage-critical", 1.0);
    bar
}

fn fill_usage(row: &gtk::ListBoxRow, bar: &gtk::LevelBar, location: gio::File) {
    glib::spawn_future_local(clone!(
        #[weak]
        row,
        #[weak]
        bar,
        async move {
            let attributes = format!(
                "{},{}",
                gio::FILE_ATTRIBUTE_FILESYSTEM_SIZE,
                gio::FILE_ATTRIBUTE_FILESYSTEM_FREE
            );
            let Ok(info) = location
                .query_filesystem_info_future(&attributes, glib::Priority::LOW)
                .await
            else {
                return;
            };
            let size = info.attribute_uint64(gio::FILE_ATTRIBUTE_FILESYSTEM_SIZE);
            let free = info
                .attribute_uint64(gio::FILE_ATTRIBUTE_FILESYSTEM_FREE)
                .min(size);
            if size == 0 {
                return;
            }
            bar.set_value((size - free) as f64 / size as f64);
            bar.set_visible(true);
            row.set_tooltip_text(Some(
                &gettext("{free} free of {size}")
                    .replace("{free}", &format::size(free))
                    .replace("{size}", &format::size(size)),
            ));
        }
    ));
}
