//! `Sidebar`: the navigation pane.
//!
//! Sections: Home & Recent · Pinned (XDG user folders, then the GTK
//! bookmarks) · This PC (file system, drives and unmounted volumes from the
//! `gio::VolumeMonitor`, with usage bars and eject buttons; its heading
//! opens the "This PC" page) · Network (network mounts and "Connect to
//! Server…") · Trash (with its item count).
//!
//! The list is rebuilt from scratch whenever bookmarks or mounts change;
//! there are only a few dozen rows. The row matching the active tab's
//! folder is highlighted.
//!
//! Files dropped on an entry are emitted as `drop-files` (dropping on the
//! Trash trashes them); hovering an entry during a drag opens it.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;

use gettextrs::gettext;
use gtk::{gdk, gio, glib, glib::clone, prelude::*, subclass::prelude::*};

use crate::model::places::{self, Bookmark};
use crate::services::volumes::{self, Device};
use crate::widgets::dnd;
use crate::widgets::folder_pane::display_name;

const TRASH_URI: &str = "trash:///";

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
    Device(Device),
    /// Opens the Connect to Server dialog.
    ConnectToServer,
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
            Target::Device(device) => device.location(),
            Target::ConnectToServer => None,
        }
    }

    /// The mount that the eject button and menu items act on.
    fn removable_mount(&self) -> Option<gio::Mount> {
        match &self.target {
            Target::Device(device) => device.removable_mount(),
            _ => None,
        }
    }

    /// A volume that can be mounted.
    fn unmounted_volume(&self) -> Option<gio::Volume> {
        match &self.target {
            Target::Device(device) => device.volume().filter(|v| v.get_mount().is_none()),
            _ => None,
        }
    }
}

/// During a drag: the row under the pointer, and whether the dragged files
/// are on its device (once known).
type Hovered = Option<(i32, Option<bool>)>;

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
                    // (destination folder, files, action)
                    glib::subclass::Signal::builder("drop-files")
                        .param_types([
                            gio::File::static_type(),
                            gdk::FileList::static_type(),
                            gdk::DragAction::static_type(),
                        ])
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
        self.setup_drop_target();

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
            gio::ActionEntry::builder("empty-trash")
                .activate(clone!(
                    #[weak(rename_to = sidebar)]
                    self,
                    move |_, _, _| {
                        let _ = sidebar.activate_action("win.empty-trash", None);
                    }
                ))
                .build(),
            gio::ActionEntry::builder("eject")
                .activate(clone!(
                    #[weak(rename_to = sidebar)]
                    self,
                    move |_, _, _| {
                        let mount = sidebar.menu_place().and_then(|p| p.removable_mount());
                        if let Some(mount) = mount {
                            sidebar.eject(&mount);
                        }
                    }
                ))
                .build(),
            gio::ActionEntry::builder("unmount")
                .activate(clone!(
                    #[weak(rename_to = sidebar)]
                    self,
                    move |_, _, _| {
                        let mount = sidebar.menu_place().and_then(|p| p.removable_mount());
                        if let Some(mount) = mount {
                            sidebar.unmount(&mount);
                        }
                    }
                ))
                .build(),
            gio::ActionEntry::builder("mount")
                .activate(clone!(
                    #[weak(rename_to = sidebar)]
                    self,
                    move |_, _, _| {
                        if let Some(index) = sidebar.imp().menu_row.get() {
                            sidebar.activate(index, false);
                        }
                    }
                ))
                .build(),
            gio::ActionEntry::builder("properties")
                .activate(clone!(
                    #[weak(rename_to = sidebar)]
                    self,
                    move |_, _, _| {
                        let location = sidebar.menu_place().and_then(|p| p.location());
                        if let Some(location) = location {
                            let uri = location.uri().to_variant();
                            let _ = sidebar.activate_action("win.location-properties", Some(&uri));
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

    pub fn connect_drop_files<
        F: Fn(&Self, &gio::File, Vec<gio::File>, gdk::DragAction) + 'static,
    >(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "drop-files",
            false,
            glib::closure_local!(move |sidebar: &Self,
                                       dest: &gio::File,
                                       files: &gdk::FileList,
                                       action: gdk::DragAction| {
                f(sidebar, dest, files.files(), action)
            }),
        )
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
        for device in volumes::devices(monitor) {
            list.push(Place {
                section: if device.is_network() {
                    Section::Network
                } else {
                    Section::Drives
                },
                label: device.name(),
                icon: device.symbolic_icon(),
                target: Target::Device(device),
                bookmark_uri: None,
            });
        }
        list.push(Place {
            section: Section::Network,
            label: gettext("Connect to Server…"),
            icon: gio::ThemedIcon::new("network-server-symbolic").upcast(),
            target: Target::ConnectToServer,
            bookmark_uri: None,
        });
        // Keep each section contiguous (mounts and volumes interleave
        // network and local entries). The sort is stable.
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
            let bar = volumes::usage_bar();
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

        if let Some(mount) = place.removable_mount() {
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
            Section::Drives => {
                row.set_header(Some(&self.this_pc_heading()));
                return;
            }
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

    /// The "This PC" heading, a button that opens the drives page.
    fn this_pc_heading(&self) -> gtk::Button {
        let label = gtk::Label::builder()
            .label(gettext("This PC"))
            .xalign(0.0)
            .build();
        let button = gtk::Button::builder()
            .child(&label)
            .tooltip_text(gettext("Show All Drives"))
            .css_classes(["flat", "sidebar-heading", "sidebar-heading-button"])
            .build();
        button.connect_clicked(clone!(
            #[weak(rename_to = sidebar)]
            self,
            move |_| sidebar.emit_open(&gio::File::for_uri(places::THIS_PC_URI), false)
        ));
        button
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

    // ── Drag and drop ──────────────────────────────────────────────────────

    fn setup_drop_target(&self) {
        let list_box = &self.imp().list_box;
        let target = gtk::DropTarget::new(gdk::FileList::static_type(), dnd::ACTIONS);
        target.set_preload(true);
        // The hovered row, whether the files are on its device, and the
        // timer that opens it.
        let hovered: Rc<Cell<Hovered>> = Rc::default();
        let spring: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
        let stop = clone!(
            #[weak]
            list_box,
            #[strong]
            hovered,
            #[strong]
            spring,
            move || {
                list_box.drag_unhighlight_row();
                hovered.set(None);
                if let Some(id) = spring.take() {
                    id.remove();
                }
            }
        );

        target.connect_motion(clone!(
            #[weak(rename_to = sidebar)]
            self,
            #[strong]
            hovered,
            #[strong]
            spring,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |target, _, y| {
                let list_box = &sidebar.imp().list_box;
                let Some((row, dest)) = sidebar.drop_destination(y) else {
                    list_box.drag_unhighlight_row();
                    hovered.set(None);
                    if let Some(id) = spring.take() {
                        id.remove();
                    }
                    return gdk::DragAction::empty();
                };
                if hovered.get().map(|(index, _)| index) != Some(row.index()) {
                    list_box.drag_highlight_row(&row);
                    hovered.set(Some((row.index(), None)));
                    if let Some(id) = spring.take() {
                        id.remove();
                    }
                    let id = glib::timeout_add_local_once(
                        dnd::SPRING_DELAY,
                        clone!(
                            #[weak]
                            sidebar,
                            #[strong]
                            spring,
                            #[strong]
                            dest,
                            move || {
                                spring.take();
                                if !dnd::is_trash(&dest) {
                                    sidebar.emit_open(&dest, false);
                                }
                            }
                        ),
                    );
                    spring.replace(Some(id));
                }
                sidebar.drop_action(target, &dest, &hovered)
            }
        ));
        target.connect_leave(clone!(
            #[strong]
            stop,
            move |_| stop()
        ));
        target.connect_drop(clone!(
            #[weak(rename_to = sidebar)]
            self,
            #[strong]
            hovered,
            #[upgrade_or]
            false,
            move |target, value, _, y| {
                let action = sidebar
                    .drop_destination(y)
                    .map(|(_, dest)| (sidebar.drop_action(target, &dest, &hovered), dest));
                stop();
                let (Ok(files), Some((action, dest))) = (value.get::<gdk::FileList>(), action)
                else {
                    return false;
                };
                if action.is_empty() {
                    return false;
                }
                sidebar.emit_by_name::<()>("drop-files", &[&dest, &files, &action]);
                true
            }
        ));
        list_box.add_controller(target);
    }

    /// The entry under `y` that files can be dropped on, and its folder.
    fn drop_destination(&self, y: f64) -> Option<(gtk::ListBoxRow, gio::File)> {
        let row = self.imp().list_box.row_at_y(y as i32)?;
        let dest = self
            .imp()
            .places
            .borrow()
            .get(row.index() as usize)?
            .location()?;
        if matches!(dest.uri_scheme().as_deref(), Some("recent" | "network")) {
            return None;
        }
        Some((row, dest))
    }

    fn drop_action(
        &self,
        target: &gtk::DropTarget,
        dest: &gio::File,
        hovered: &Cell<Hovered>,
    ) -> gdk::DragAction {
        let files = target
            .value()
            .and_then(|v| v.get::<gdk::FileList>().ok())
            .map(|list| list.files())
            .unwrap_or_default();
        let Some(first) = files.first() else {
            return gdk::DragAction::COPY;
        };
        dnd::action_for(&files, dest, target.current_event_state(), || {
            if let Some((_, Some(same))) = hovered.get() {
                return same;
            }
            let same = dnd::same_device(first, dest);
            if let Some((index, _)) = hovered.get() {
                hovered.set(Some((index, Some(same))));
            }
            same
        })
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
        if let Target::ConnectToServer = place.target {
            let _ = self.activate_action("win.connect-to-server", None);
            return;
        }
        let Some(volume) = place.unmounted_volume() else {
            return;
        };
        glib::spawn_future_local(clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                match volumes::mount(&volume, Some(sidebar.upcast_ref())).await {
                    Ok(root) => sidebar.emit_open(&root, new_tab),
                    Err(err) if volumes::is_handled(&err) => {}
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

    /// Ejects (or unmounts and powers off) a drive.
    fn eject(&self, mount: &gio::Mount) {
        let mount = mount.clone();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                match volumes::eject(&mount, Some(sidebar.upcast_ref())).await {
                    Ok(Some(message)) => sidebar.show_toast(&message),
                    Ok(None) => {}
                    Err(err) if volumes::is_handled(&err) => {}
                    Err(err) => {
                        tracing::warn!("cannot eject {}: {err}", mount.name());
                        sidebar.show_toast(err.message());
                    }
                }
            }
        ));
    }

    fn unmount(&self, mount: &gio::Mount) {
        let mount = mount.clone();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = sidebar)]
            self,
            async move {
                match volumes::unmount(&mount, Some(sidebar.upcast_ref())).await {
                    Ok(()) => {}
                    Err(err) if volumes::is_handled(&err) => {}
                    Err(err) => {
                        tracing::warn!("cannot unmount {}: {err}", mount.name());
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
            "empty-trash",
            place.as_ref().is_some_and(|p| p.section == Section::Trash),
        );
        let mount = place.as_ref().and_then(Place::removable_mount);
        set_enabled("eject", mount.as_ref().is_some_and(|m| m.can_eject()));
        set_enabled("unmount", mount.as_ref().is_some_and(|m| m.can_unmount()));
        set_enabled(
            "mount",
            place
                .as_ref()
                .is_some_and(|p| p.unmounted_volume().is_some()),
        );
        let location = place.as_ref().and_then(Place::location);
        set_enabled("open-in-new-tab", location.is_some());
        set_enabled(
            "properties",
            location.is_some_and(|l| !matches!(l.uri_scheme().as_deref(), Some("recent"))),
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

fn fill_usage(row: &gtk::ListBoxRow, bar: &gtk::LevelBar, location: gio::File) {
    glib::spawn_future_local(clone!(
        #[weak]
        row,
        #[weak]
        bar,
        async move {
            if let Some((size, free)) = volumes::usage(&location).await {
                bar.set_value((size - free) as f64 / size as f64);
                bar.set_visible(true);
                row.set_tooltip_text(Some(&volumes::usage_text(size, free)));
            }
        }
    ));
}
