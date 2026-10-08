//! `PropertiesDialog` (Alt+Enter): what a file, a folder or a selection
//! is, in three tabs.
//!
//! - **General**: type, location, size, dates. Folder sizes and contents
//!   are counted on a worker thread and update live while counting.
//! - **Permissions** (one item with UNIX permissions): owner, group and
//!   read/write/execute toggles applied right away through `unix::mode`.
//! - **Open With** (files of one type): the default app for the type.

use std::cell::{Cell, OnceCell, RefCell};

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{gio, glib, glib::clone, subclass::prelude::*};

use crate::model::FileItem;
use crate::model::permissions::{self, Access, Class};
use crate::services::disk_usage::{Measurement, Totals};
use crate::services::volumes;
use crate::util::format;
use crate::widgets::app_chooser::AppChooser;
use crate::widgets::folder_pane;
use crate::widgets::thumbnail_image::ThumbnailImage;

/// Attributes shown by the dialog.
const ATTRIBUTES: &str = "standard::*,time::*,unix::mode,owner::*,access::*,\
                          thumbnail::*,trash::orig-path,trash::deletion-date";

mod imp {
    use super::*;
    use adw::subclass::prelude::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/lia/Files/ui/properties-dialog.ui")]
    pub struct PropertiesDialog {
        #[template_child]
        pub(super) toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub(super) icon_image: TemplateChild<ThumbnailImage>,
        #[template_child]
        pub(super) name_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) type_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) general_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub(super) dates_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub(super) permissions_page: TemplateChild<adw::ViewStackPage>,
        #[template_child]
        pub(super) owner_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub(super) group_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub(super) access_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub(super) exec_row: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub(super) mode_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub(super) open_with_page: TemplateChild<adw::ViewStackPage>,
        #[template_child]
        pub(super) open_with_group: TemplateChild<adw::PreferencesGroup>,
        #[template_child]
        pub(super) reset_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) set_default_button: TemplateChild<gtk::Button>,

        /// The single item shown, if only one.
        pub(super) file: RefCell<Option<gio::File>>,
        /// Its permission bits.
        pub(super) mode: Cell<u32>,
        /// Set while the toggles are updated from `mode`.
        pub(super) syncing: Cell<bool>,
        pub(super) toggles: RefCell<Vec<(Class, Access, gtk::ToggleButton)>>,
        pub(super) chooser: OnceCell<AppChooser>,
        pub(super) tasks: RefCell<Vec<glib::JoinHandle<()>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PropertiesDialog {
        const NAME: &'static str = "LiaPropertiesDialog";
        type Type = super::PropertiesDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            ThumbnailImage::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PropertiesDialog {
        fn dispose(&self) {
            self.obj().cancel_tasks();
        }
    }

    impl WidgetImpl for PropertiesDialog {}

    impl AdwDialogImpl for PropertiesDialog {
        fn closed(&self) {
            // Stops counting a large folder as soon as the dialog goes.
            self.obj().cancel_tasks();
            self.parent_closed();
        }
    }
}

glib::wrapper! {
    pub struct PropertiesDialog(ObjectSubclass<imp::PropertiesDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl PropertiesDialog {
    /// Shows the properties of `files` (one or more).
    pub fn present_for(parent: &impl IsA<gtk::Widget>, files: Vec<gio::File>) {
        if files.is_empty() {
            return;
        }
        let dialog: Self = glib::Object::new();
        dialog.setup_signals();
        dialog.present(Some(parent));
        let task = glib::spawn_future_local(clone!(
            #[weak]
            dialog,
            async move {
                let mut items = Vec::new();
                for file in files {
                    match file
                        .query_info_future(
                            ATTRIBUTES,
                            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                            glib::Priority::DEFAULT,
                        )
                        .await
                    {
                        Ok(info) => items.push(FileItem::new(file, info)),
                        Err(err) => tracing::warn!("cannot query {}: {err}", file.uri()),
                    }
                }
                match items.as_slice() {
                    [] => dialog.show_error(),
                    [item] => dialog.show_item(item),
                    items => dialog.show_selection(items),
                }
            }
        ));
        dialog.imp().tasks.borrow_mut().push(task);
    }

    fn setup_signals(&self) {
        let imp = self.imp();
        imp.exec_row.connect_active_notify(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |row| {
                if !dialog.imp().syncing.get() {
                    let mode =
                        permissions::set_executable(dialog.imp().mode.get(), row.is_active());
                    dialog.apply_mode(mode);
                }
            }
        ));
        imp.reset_button.connect_clicked(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| {
                if let Some(chooser) = dialog.imp().chooser.get() {
                    gio::AppInfo::reset_type_associations(&chooser.content_type());
                    chooser.reload();
                    dialog.update_default_button();
                }
            }
        ));
        imp.set_default_button.connect_clicked(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.set_selected_app_as_default()
        ));
    }

    fn cancel_tasks(&self) {
        for task in self.imp().tasks.take() {
            task.abort();
        }
    }

    fn spawn(&self, future: impl std::future::Future<Output = ()> + 'static) {
        self.imp()
            .tasks
            .borrow_mut()
            .push(glib::spawn_future_local(future));
    }

    // ── General ────────────────────────────────────────────────────────────

    fn show_error(&self) {
        let imp = self.imp();
        imp.icon_image
            .set_icon(&gio::ThemedIcon::new("dialog-warning-symbolic").upcast());
        imp.name_label.set_label(&gettext("Not Available"));
        imp.type_label
            .set_label(&gettext("The item may have been moved or deleted"));
    }

    fn show_item(&self, item: &FileItem) {
        let imp = self.imp();
        let file = item.file();
        let info = item.info();
        let is_dir = info.file_type() == gio::FileType::Directory;
        imp.file.replace(Some(file.clone()));
        let in_trash = file.uri_scheme().as_deref() == Some("trash");

        imp.icon_image.set_item(Some(item));
        let name = if file.parent().is_none() {
            folder_pane::display_name(file)
        } else {
            format::one_line(&item.display_name()).into_owned()
        };
        imp.name_label.set_label(&name);
        self.set_title(&gettext("“{}” Properties").replace("{}", &name));
        let content_type = item.content_type().map(String::from).unwrap_or_default();
        let description = if is_dir {
            gettext("Folder")
        } else {
            item.type_description()
        };
        imp.type_label.set_label(&description);

        // ── What and where
        if !is_dir && !content_type.is_empty() {
            self.add_row(
                &imp.general_group,
                &gettext("Type"),
                &format!("{description} ({content_type})"),
            );
        }
        if let Some(original) = item.trash_orig_path() {
            let folder = original.parent().unwrap_or(&original);
            self.add_row(
                &imp.general_group,
                &gettext("Original location"),
                &gio::File::for_path(folder).parse_name(),
            );
        } else if let Some(parent) = file.parent() {
            self.add_row(
                &imp.general_group,
                &gettext("Location"),
                &parent.parse_name(),
            );
        }
        if let Some(target) = item.symlink_target() {
            self.add_row(
                &imp.general_group,
                &gettext("Link target"),
                &target.to_string_lossy(),
            );
        }

        // ── How big
        if is_dir {
            self.measure(vec![file.clone()], &gettext("Size"));
            let disk = self.add_pending_row(&imp.general_group, &gettext("Free space"));
            let location = file.clone();
            self.spawn(async move {
                if let Some((size, free)) = volumes::usage(&location).await {
                    fill_row(&disk, &volumes::usage_text(size, free));
                }
            });
        } else {
            self.add_row(
                &imp.general_group,
                &gettext("Size"),
                &glib::format_size_full(item.size(), glib::FormatSizeFlags::LONG_FORMAT),
            );
        }

        // ── When
        let time = |attribute: &str| {
            info.has_attribute(attribute)
                .then(|| info.attribute_uint64(attribute) as i64)
                .filter(|&t| t > 0)
        };
        for (title, attribute) in [
            (gettext("Modified"), gio::FILE_ATTRIBUTE_TIME_MODIFIED),
            (gettext("Accessed"), gio::FILE_ATTRIBUTE_TIME_ACCESS),
            (gettext("Created"), gio::FILE_ATTRIBUTE_TIME_CREATED),
        ] {
            if let Some(time) = time(attribute) {
                self.add_row(&imp.dates_group, &title, &format::full_date(time));
            }
        }
        if let Some(deleted) = item.trash_deletion_date() {
            let deleted = glib::DateTime::from_iso8601(&deleted, Some(&glib::TimeZone::local()))
                .map(|date| format::full_date(date.to_unix()))
                .unwrap_or_else(|_| deleted.to_string());
            self.add_row(&imp.dates_group, &gettext("Deleted"), &deleted);
        }

        if !in_trash {
            self.show_permissions(info, is_dir);
            if !is_dir && !content_type.is_empty() {
                self.show_open_with(&content_type, &description);
            }
        }
        hide_if_empty(&imp.general_group);
        hide_if_empty(&imp.dates_group);
    }

    fn show_selection(&self, items: &[FileItem]) {
        let imp = self.imp();
        let count = items.len() as u32;
        let folders = items.iter().filter(|i| i.is_dir()).count() as u32;
        let files = count - folders;

        imp.icon_image
            .set_icon(&gio::ThemedIcon::from_names(&["folder-documents", "folder"]).upcast());
        let title = ngettext("{} Item", "{} Items", count).replace("{}", &count.to_string());
        imp.name_label.set_label(&title);
        self.set_title(&gettext("Properties of {}").replace("{}", &title));
        imp.type_label
            .set_label(&folders_and_files(folders as u64, files as u64));

        let parent = items[0].file().parent();
        if let Some(parent) = parent
            && items[1..]
                .iter()
                .all(|i| i.file().parent().is_some_and(|p| p.equal(&parent)))
        {
            self.add_row(
                &imp.general_group,
                &gettext("Location"),
                &parent.parse_name(),
            );
        }
        let files: Vec<gio::File> = items.iter().map(|i| i.file().clone()).collect();
        self.measure(files, &gettext("Total size"));
        hide_if_empty(&imp.dates_group);

        // Files that all have the same type: the same default app.
        let types: Vec<String> = items
            .iter()
            .filter(|i| !i.is_dir())
            .filter_map(|i| i.content_type().map(String::from))
            .collect();
        let in_trash = items[0].file().uri_scheme().as_deref() == Some("trash");
        if folders == 0
            && !in_trash
            && types.len() == items.len()
            && types.iter().all(|t| *t == types[0])
        {
            self.show_open_with(&types[0], &items[0].type_description());
        }
    }

    /// Adds size and contents rows that count up while `files` are measured.
    fn measure(&self, files: Vec<gio::File>, size_title: &str) {
        let imp = self.imp();
        let size_row = self.add_row(&imp.general_group, size_title, &gettext("Counting…"));
        let contents_row = self.add_row(&imp.general_group, &gettext("Contents"), "");
        let spinner = adw::Spinner::new();
        size_row.add_suffix(&spinner);
        let single_folder = files.len() == 1;

        self.spawn(async move {
            let measurement = Measurement::start(files);
            let mut last = Totals::default();
            let show = |totals: &Totals| {
                size_row.set_subtitle(&glib::format_size_full(
                    totals.size,
                    glib::FormatSizeFlags::LONG_FORMAT,
                ));
                let mut contents = folders_and_files(totals.folders, totals.files);
                if single_folder && totals.files == 0 && totals.folders == 0 {
                    contents = gettext("Empty folder");
                }
                if totals.unreadable > 0 {
                    // Translators: appended when some folders couldn't be read
                    contents = gettext("{} (some items couldn't be read)").replace("{}", &contents);
                }
                contents_row.set_subtitle(&contents);
            };
            while let Some(totals) = measurement.next().await {
                show(&totals);
                last = totals;
            }
            show(&last);
            spinner.set_visible(false);
        });
    }

    // ── Permissions ────────────────────────────────────────────────────────

    fn show_permissions(&self, info: &gio::FileInfo, is_dir: bool) {
        let imp = self.imp();
        if !info.has_attribute(gio::FILE_ATTRIBUTE_UNIX_MODE) {
            return;
        }
        imp.permissions_page.set_visible(true);

        let user = info.attribute_string(gio::FILE_ATTRIBUTE_OWNER_USER);
        let real_name = info.attribute_string(gio::FILE_ATTRIBUTE_OWNER_USER_REAL);
        let owner = match (&user, &real_name) {
            (Some(user), Some(real)) if !real.is_empty() && real != user => {
                format!("{real} ({user})")
            }
            (Some(user), _) => user.to_string(),
            (None, _) => gettext("Unknown"),
        };
        imp.owner_row.set_subtitle(&owner);
        imp.group_row.set_subtitle(
            &info
                .attribute_string(gio::FILE_ATTRIBUTE_OWNER_GROUP)
                .map(String::from)
                .unwrap_or_else(|| gettext("Unknown")),
        );

        // Only the owner (or root) may change permissions; without owner
        // information (some network shares), let the server decide.
        let me = glib::user_name();
        let editable = user
            .as_deref()
            .is_none_or(|user| user == me.to_string_lossy() || me == "root");
        if !editable {
            imp.access_group.set_description(Some(&gettext(
                "Only the owner can change these permissions",
            )));
        }

        let classes = [
            (Class::Owner, gettext("Owner")),
            (Class::Group, gettext("Group")),
            (Class::Others, gettext("Others")),
        ];
        let accesses = if is_dir {
            [
                (
                    Access::Read,
                    gettext("Read"),
                    gettext("See the items in the folder"),
                ),
                (
                    Access::Write,
                    gettext("Write"),
                    gettext("Create, rename and delete items in the folder"),
                ),
                (
                    Access::Execute,
                    gettext("Open"),
                    gettext("Open the folder and use its items"),
                ),
            ]
        } else {
            [
                (Access::Read, gettext("Read"), gettext("Read the file")),
                (Access::Write, gettext("Write"), gettext("Change the file")),
                (
                    Access::Execute,
                    gettext("Execute"),
                    gettext("Run the file as a program"),
                ),
            ]
        };
        let mut toggles = Vec::new();
        for (class, title) in classes {
            let row = adw::ActionRow::builder().title(title.as_str()).build();
            let buttons = gtk::Box::builder()
                .valign(gtk::Align::Center)
                .css_classes(["linked"])
                .build();
            for (access, label, tooltip) in &accesses {
                let access = *access;
                let toggle = gtk::ToggleButton::builder()
                    .label(label.as_str())
                    .tooltip_text(tooltip.as_str())
                    .sensitive(editable)
                    .build();
                toggle.update_property(&[gtk::accessible::Property::Label(&format!(
                    "{title}: {label}"
                ))]);
                toggle.connect_toggled(clone!(
                    #[weak(rename_to = dialog)]
                    self,
                    move |toggle| {
                        if !dialog.imp().syncing.get() {
                            let mode = permissions::with(
                                dialog.imp().mode.get(),
                                class,
                                access,
                                toggle.is_active(),
                            );
                            dialog.apply_mode(mode);
                        }
                    }
                ));
                buttons.append(&toggle);
                toggles.push((class, access, toggle));
            }
            row.add_suffix(&buttons);
            imp.access_group.add(&row);
        }
        imp.toggles.replace(toggles);

        imp.exec_row
            .set_visible(info.file_type() == gio::FileType::Regular);
        imp.exec_row.set_sensitive(editable);
        let mode = info.attribute_uint32(gio::FILE_ATTRIBUTE_UNIX_MODE);
        self.sync_mode(mode & permissions::PERMISSION_BITS);
    }

    /// Shows `mode` in the toggles, switch and mode row.
    fn sync_mode(&self, mode: u32) {
        let imp = self.imp();
        imp.mode.set(mode);
        imp.syncing.set(true);
        for (class, access, toggle) in imp.toggles.borrow().iter() {
            toggle.set_active(permissions::has(mode, *class, *access));
        }
        imp.exec_row.set_active(permissions::is_executable(mode));
        imp.syncing.set(false);
        imp.mode_row.set_subtitle(&format!(
            "{} ({})",
            permissions::symbolic(mode),
            permissions::octal(mode)
        ));
    }

    /// Writes `mode` to the file; on failure, shows the actual mode again.
    fn apply_mode(&self, mode: u32) {
        let Some(file) = self.imp().file.borrow().clone() else {
            return;
        };
        let previous = self.imp().mode.get();
        self.sync_mode(mode);
        self.spawn(clone!(
            #[weak(rename_to = dialog)]
            self,
            async move {
                let info = gio::FileInfo::new();
                info.set_attribute_uint32(gio::FILE_ATTRIBUTE_UNIX_MODE, mode);
                let result = file
                    .set_attributes_future(
                        &info,
                        gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                        glib::Priority::DEFAULT,
                    )
                    .await;
                if let Err(err) = result {
                    tracing::warn!("cannot change permissions of {}: {err}", file.uri());
                    dialog.sync_mode(previous);
                    dialog.add_toast(
                        &gettext("Couldn't change the permissions: {}")
                            .replace("{}", err.message()),
                    );
                }
            }
        ));
    }

    // ── Open with ──────────────────────────────────────────────────────────

    fn show_open_with(&self, content_type: &str, description: &str) {
        let imp = self.imp();
        imp.open_with_page.set_visible(true);
        imp.open_with_group.set_title(&gettext("Default App"));
        imp.open_with_group.set_description(Some(
            &gettext("The app that opens “{}” files").replace("{}", description),
        ));
        let chooser = AppChooser::new(content_type);
        chooser.connect_selection_changed(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_| dialog.update_default_button()
        ));
        chooser.connect_app_activated(clone!(
            #[weak(rename_to = dialog)]
            self,
            move |_, _| dialog.set_selected_app_as_default()
        ));
        imp.open_with_group.add(&chooser);
        imp.chooser.set(chooser).unwrap();
        self.update_default_button();
    }

    fn update_default_button(&self) {
        let Some(chooser) = self.imp().chooser.get() else {
            return;
        };
        let selected = chooser.selected_app();
        let is_default = selected
            .as_ref()
            .zip(chooser.default_app())
            .is_some_and(|(a, b)| a.equal(&b));
        self.imp()
            .set_default_button
            .set_sensitive(selected.is_some() && !is_default);
    }

    fn set_selected_app_as_default(&self) {
        let Some(chooser) = self.imp().chooser.get() else {
            return;
        };
        let Some(app) = chooser.selected_app() else {
            return;
        };
        match app.set_as_default_for_type(&chooser.content_type()) {
            Ok(()) => {
                chooser.reload();
                self.update_default_button();
            }
            Err(err) => self.add_toast(err.message()),
        }
    }

    // ── Helpers ────────────────────────────────────────────────────────────

    fn add_row(&self, group: &adw::PreferencesGroup, title: &str, value: &str) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(value)
            .use_markup(false)
            .subtitle_selectable(true)
            .css_classes(["property"])
            .build();
        group.add(&row);
        group.set_visible(true);
        row
    }

    /// A row filled in later (see [`fill_row`]); hidden until then.
    fn add_pending_row(&self, group: &adw::PreferencesGroup, title: &str) -> adw::ActionRow {
        let row = self.add_row(group, title, "");
        row.set_visible(false);
        row
    }

    fn add_toast(&self, message: &str) {
        let toast = adw::Toast::builder()
            .title(message)
            .use_markup(false)
            .timeout(5)
            .build();
        self.imp().toast_overlay.add_toast(toast);
    }
}

fn fill_row(row: &adw::ActionRow, value: &str) {
    row.set_subtitle(value);
    row.set_visible(true);
}

/// Hides a group without rows (its only child is the title box).
fn hide_if_empty(group: &adw::PreferencesGroup) {
    let mut has_rows = false;
    let mut index = 0;
    while let Some(row) = group.row(index) {
        has_rows |= row.is_visible();
        index += 1;
    }
    group.set_visible(has_rows);
}

/// "3 folders, 12 files".
fn folders_and_files(folders: u64, files: u64) -> String {
    let folders_text =
        ngettext("{} folder", "{} folders", folders as u32).replace("{}", &folders.to_string());
    let files_text =
        ngettext("{} file", "{} files", files as u32).replace("{}", &files.to_string());
    match (folders, files) {
        (0, _) => files_text,
        (_, 0) => folders_text,
        // Translators: "2 folders, 5 files"
        _ => gettext("{folders}, {files}")
            .replace("{folders}", &folders_text)
            .replace("{files}", &files_text),
    }
}
