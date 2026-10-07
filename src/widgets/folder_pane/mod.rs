//! `FolderPane`: the contents of one tab — the file view and its
//! empty/error states. The window owns the address bar, search entry and
//! status bar and binds them to the active pane's properties.
//!
//! Model chain:
//! `DirectoryModel → FilterListModel (hidden, search) → SortListModel
//!  (folders first + column sorter) → MultiSelection → FileView`
//!
//! File operations started from the pane (drops, renames…) go to the job
//! queue; once done, what they created in this folder gets selected.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use gettextrs::{gettext, ngettext};
use gtk::{gdk, gio, glib, glib::clone, prelude::*, subclass::prelude::*};

use crate::model::{DirectoryModel, FileItem, History, sorting};
use crate::services::file_ops::{FileOpJob, Operation, naming, queue};
use crate::util::{format, settings};
use crate::widgets::dnd;
use crate::widgets::file_view::{DEFAULT_ICON_SIZE, FileView, ICON_SIZES, ViewMode};

/// How long to wait for the items an operation created to show up.
const SELECT_TIMEOUT: Duration = Duration::from_secs(3);

/// State read by the filter closure.
#[derive(Default)]
struct FilterState {
    show_hidden: Cell<bool>,
    /// Lower-cased search text.
    query: RefCell<String>,
}

impl FilterState {
    fn matches(&self, item: &FileItem) -> bool {
        if !self.show_hidden.get() && item.is_hidden() {
            return false;
        }
        let query = self.query.borrow();
        query.is_empty() || item.display_name().to_lowercase().contains(query.as_str())
    }
}

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate, glib::Properties)]
    #[template(resource = "/io/github/lia/Files/ui/folder-pane.ui")]
    #[properties(wrapper_type = super::FolderPane)]
    pub struct FolderPane {
        #[template_child]
        pub(super) content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(super) file_view: TemplateChild<FileView>,
        #[template_child]
        pub(super) status_page: TemplateChild<adw::StatusPage>,

        /// The folder being shown.
        #[property(get, nullable)]
        pub(super) location: RefCell<Option<gio::File>>,
        /// Short name of the folder, for the tab and window title.
        #[property(get)]
        pub(super) title: RefCell<String>,
        #[property(get)]
        pub(super) loading: Cell<bool>,
        /// The instant-filter text. Reset when navigating.
        #[property(get, set = Self::set_search_text)]
        pub(super) search_text: RefCell<String>,
        // Status bar texts.
        #[property(get)]
        pub(super) item_count_text: RefCell<String>,
        #[property(get)]
        pub(super) selection_text: RefCell<String>,
        #[property(get)]
        pub(super) free_space_text: RefCell<String>,
        #[property(get)]
        pub(super) n_selected: Cell<u32>,
        /// Files can be created in the folder.
        #[property(get)]
        pub(super) writable: Cell<bool>,
        /// The folder is the trash (or inside it).
        #[property(get)]
        pub(super) is_trash: Cell<bool>,
        /// The folder's file system needs Windows-compatible names.
        pub(super) portable_names: Cell<bool>,

        pub(super) history: RefCell<History<gio::File>>,
        pub(super) directory: DirectoryModel,
        pub(super) filter_state: Rc<FilterState>,
        pub(super) filter: OnceCell<gtk::CustomFilter>,
        pub(super) filter_model: OnceCell<gtk::FilterListModel>,
        pub(super) folders_first_sorter: OnceCell<gtk::NumericSorter>,
        pub(super) sort_model: OnceCell<gtk::SortListModel>,
        pub(super) selection: OnceCell<gtk::MultiSelection>,
        /// Item to select once loading finishes (e.g. the folder we came
        /// from when going up or back).
        pub(super) select_after_load: RefCell<Option<gio::File>>,
        pub(super) free_space_task: RefCell<Option<glib::JoinHandle<()>>>,
        pub(super) info_task: RefCell<Option<glib::JoinHandle<()>>>,
        pub(super) settings_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub(super) queue_handler: RefCell<Option<glib::SignalHandlerId>>,
        /// Items to select once they appear (created by an operation), and
        /// whether to rename the first one.
        pub(super) pending_select: RefCell<Vec<gio::File>>,
        pub(super) pending_rename: Cell<bool>,
        pub(super) pending_timeout: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FolderPane {
        const NAME: &'static str = "LiaFolderPane";
        type Type = super::FolderPane;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            FileView::ensure_type();
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("lia-folder-pane");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for FolderPane {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    // The sort column or order changed (menu or column header).
                    glib::subclass::Signal::builder("sort-changed").build(),
                    // The selected items (or the items themselves) changed.
                    glib::subclass::Signal::builder("selection-changed").build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }

        fn dispose(&self) {
            let settings = settings();
            for handler in self.settings_handlers.take() {
                settings.disconnect(handler);
            }
            for task in [self.free_space_task.take(), self.info_task.take()]
                .into_iter()
                .flatten()
            {
                task.abort();
            }
            if let Some(handler) = self.queue_handler.take() {
                queue().disconnect(handler);
            }
            if let Some(timeout) = self.pending_timeout.take() {
                timeout.remove();
            }
            self.directory.stop();
            self.dispose_template();
        }
    }

    impl WidgetImpl for FolderPane {
        fn grab_focus(&self) -> bool {
            self.file_view.grab_focus()
        }
    }

    impl FolderPane {
        fn set_search_text(&self, text: String) {
            if *self.search_text.borrow() == text {
                return;
            }
            self.search_text.replace(text);
            self.obj().apply_search_text();
            self.obj().notify_search_text();
        }
    }
}

glib::wrapper! {
    pub struct FolderPane(ObjectSubclass<imp::FolderPane>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for FolderPane {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl FolderPane {
    pub fn new() -> Self {
        Self::default()
    }

    fn setup(&self) {
        let imp = self.imp();
        let settings = settings();

        // ── Model chain ────────────────────────────────────────────────────
        imp.filter_state
            .show_hidden
            .set(settings.boolean("show-hidden"));
        let filter_state = imp.filter_state.clone();
        let filter = gtk::CustomFilter::new(move |obj| {
            obj.downcast_ref::<FileItem>()
                .is_some_and(|item| filter_state.matches(item))
        });
        let filter_model =
            gtk::FilterListModel::new(Some(imp.directory.clone()), Some(filter.clone()));
        filter_model.set_incremental(false);

        let folders_first = sorting::folders_first_sorter(settings.boolean("sort-folders-first"));
        let sorter = gtk::MultiSorter::new();
        sorter.append(folders_first.clone());
        if let Some(column_sorter) = imp.file_view.sorter() {
            sorter.append(column_sorter);
        }
        let sort_model = gtk::SortListModel::new(Some(filter_model.clone()), Some(sorter));
        // Not incremental: an incremental sort restarts on every streamed
        // batch and the view re-processes each step (12 s vs 2 s for 100k
        // files). With key-based sorters a full sort is cheap anyway.
        sort_model.set_incremental(false);

        let selection = gtk::MultiSelection::new(Some(sort_model.clone()));
        imp.file_view.set_model(&selection);

        // ── Signals ────────────────────────────────────────────────────────
        imp.directory.connect_loading_notify(clone!(
            #[weak(rename_to = pane)]
            self,
            move |model| {
                if !model.loading() {
                    pane.on_load_finished();
                }
                pane.imp().loading.set(model.loading());
                pane.notify_loading();
                pane.update_content_page();
            }
        ));
        sort_model.connect_items_changed(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_, _, _, _| {
                pane.keep_at_top_while_loading();
                pane.update_content_page();
                pane.update_status();
                pane.apply_pending_selection(false);
                pane.emit_by_name::<()>("selection-changed", &[]);
            }
        ));
        imp.file_view.connect_sort_changed(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_| pane.emit_by_name::<()>("sort-changed", &[])
        ));
        selection.connect_selection_changed(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_, _, _| {
                pane.update_status();
                pane.emit_by_name::<()>("selection-changed", &[]);
            }
        ));

        imp.file_view.connect_item_activated(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_, item| pane.open_item(item, false)
        ));
        imp.file_view.connect_drop_files(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_, dest, files, action| pane.drop_files(dest, &files, action)
        ));
        let handler = queue().connect_job_finished(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_, job| pane.on_job_finished(job)
        ));
        imp.queue_handler.replace(Some(handler));

        // ── Settings ───────────────────────────────────────────────────────
        imp.file_view
            .set_mode(ViewMode::from_nick(&settings.string("default-view")));
        imp.file_view.set_icon_size(settings.int("icon-size"));

        let handlers = vec![
            settings.connect_changed(
                Some("show-hidden"),
                clone!(
                    #[weak(rename_to = pane)]
                    self,
                    move |settings, key| pane.set_show_hidden(settings.boolean(key))
                ),
            ),
            settings.connect_changed(
                Some("sort-folders-first"),
                clone!(
                    #[weak(rename_to = pane)]
                    self,
                    move |settings, key| sorting::set_folders_first(
                        pane.imp().folders_first_sorter.get().unwrap(),
                        settings.boolean(key),
                    )
                ),
            ),
            settings.connect_changed(
                Some("default-view"),
                clone!(
                    #[weak(rename_to = pane)]
                    self,
                    move |settings, key| pane
                        .imp()
                        .file_view
                        .set_mode(ViewMode::from_nick(&settings.string(key)))
                ),
            ),
            settings.connect_changed(
                Some("icon-size"),
                clone!(
                    #[weak(rename_to = pane)]
                    self,
                    move |settings, key| pane.imp().file_view.set_icon_size(settings.int(key))
                ),
            ),
        ];
        imp.settings_handlers.replace(handlers);

        imp.filter.set(filter).unwrap();
        imp.filter_model.set(filter_model).unwrap();
        imp.folders_first_sorter.set(folders_first).unwrap();
        imp.sort_model.set(sort_model).unwrap();
        imp.selection.set(selection).unwrap();
    }

    // ── Navigation ─────────────────────────────────────────────────────────

    /// Opens `location`, recording the current folder in the history.
    pub fn navigate_to(&self, location: &gio::File) {
        let current = self.location();
        if current.as_ref().is_some_and(|c| c.equal(location)) {
            return;
        }
        if let Some(current) = current {
            self.imp().history.borrow_mut().visit(current);
        }
        self.load(location, None);
    }

    pub fn go_back(&self) {
        let Some(current) = self.location() else {
            return;
        };
        let previous = self.imp().history.borrow_mut().go_back(current.clone());
        if let Some(previous) = previous {
            self.load(&previous, Some(current));
        }
    }

    pub fn go_forward(&self) {
        let Some(current) = self.location() else {
            return;
        };
        let next = self.imp().history.borrow_mut().go_forward(current.clone());
        if let Some(next) = next {
            self.load(&next, Some(current));
        }
    }

    pub fn go_up(&self) {
        let Some(current) = self.location() else {
            return;
        };
        if let Some(parent) = current.parent() {
            self.imp().history.borrow_mut().visit(current.clone());
            self.load(&parent, Some(current));
        }
    }

    pub fn reload(&self) {
        let imp = self.imp();
        // Keep the selection's first item focused after reloading.
        let selected = self.selected_items().into_iter().next();
        imp.select_after_load
            .replace(selected.map(|item| item.file().clone()));
        imp.directory.reload();
        self.update_free_space();
    }

    pub fn can_go_back(&self) -> bool {
        self.imp().history.borrow().can_go_back()
    }

    pub fn can_go_forward(&self) -> bool {
        self.imp().history.borrow().can_go_forward()
    }

    pub fn can_go_up(&self) -> bool {
        self.location().and_then(|l| l.parent()).is_some()
    }

    fn load(&self, location: &gio::File, select: Option<gio::File>) {
        let imp = self.imp();
        imp.select_after_load.replace(select);
        imp.location.replace(Some(location.clone()));

        // A new folder starts with an empty search.
        imp.filter_state.query.borrow_mut().clear();
        self.set_search_text("");

        imp.title.replace(display_name(location));
        imp.pending_select.borrow_mut().clear();
        imp.directory.load(location);
        imp.file_view.set_location(Some(location));
        let is_trash = dnd::is_trash(location);
        imp.file_view.set_trash_mode(is_trash);
        if imp.is_trash.replace(is_trash) != is_trash {
            self.notify_is_trash();
        }
        self.update_free_space();
        self.update_location_info();
        self.notify_title();
        self.notify_location();
    }

    /// While a folder streams in, sorted batches insert items above the
    /// visible ones; the view would follow its anchor item downwards.
    fn keep_at_top_while_loading(&self) {
        let imp = self.imp();
        if imp.directory.loading()
            && imp.select_after_load.borrow().is_none()
            && imp
                .selection
                .get()
                .is_some_and(|s| s.selection().is_empty())
            && imp.sort_model.get().is_some_and(|m| m.n_items() > 0)
        {
            imp.file_view.scroll_to_top();
        }
    }

    fn on_load_finished(&self) {
        let imp = self.imp();
        let Some(target) = imp.select_after_load.take() else {
            if imp.sort_model.get().unwrap().n_items() > 0 {
                imp.file_view.scroll_to(0, false);
            }
            return;
        };
        let sort_model = imp.sort_model.get().unwrap();
        let position = (0..sort_model.n_items()).find(|&i| {
            sort_model
                .item(i)
                .and_downcast::<FileItem>()
                .is_some_and(|item| item.file().equal(&target))
        });
        if let Some(position) = position {
            imp.file_view.scroll_to(position, true);
        }
        self.apply_pending_selection(false);
    }

    // ── Opening ────────────────────────────────────────────────────────────

    /// Opens folders in place and files with their default application
    /// (or an app chooser when `choose_app` is set).
    pub fn open_item(&self, item: &FileItem, choose_app: bool) {
        if let Some(target) = item.target_uri() {
            self.navigate_to(&gio::File::for_uri(&target));
        } else if item.is_dir() && !choose_app {
            self.navigate_to(item.file());
        } else {
            self.launch(item.file(), choose_app);
        }
    }

    /// The folder an item leads to, if it is one (following shortcut
    /// targets such as mounts in `network://`).
    pub fn folder_target(item: &FileItem) -> Option<gio::File> {
        if !item.is_dir() {
            return None;
        }
        Some(
            item.target_uri()
                .map_or_else(|| item.file().clone(), |uri| gio::File::for_uri(&uri)),
        )
    }

    pub fn open_selection(&self, choose_app: bool) {
        let items = self.selected_items();
        // Several folders: open the first one here; files open normally.
        let mut navigated = false;
        for item in &items {
            if item.is_dir() && !choose_app {
                if !navigated {
                    navigated = true;
                    self.open_item(item, false);
                }
            } else {
                self.open_item(item, choose_app);
            }
        }
    }

    fn launch(&self, file: &gio::File, choose_app: bool) {
        let launcher = gtk::FileLauncher::new(Some(file));
        launcher.set_always_ask(choose_app);
        let window = self.root().and_downcast::<gtk::Window>();
        let name = display_name(file);
        launcher.launch(
            window.as_ref(),
            gio::Cancellable::NONE,
            clone!(
                #[weak(rename_to = pane)]
                self,
                move |result| {
                    if let Err(err) = result
                        && !err.matches(gtk::DialogError::Dismissed)
                    {
                        tracing::warn!("failed to open {name}: {err}");
                        pane.show_toast(&gettext("Could not open “{}”").replace("{}", &name));
                    }
                }
            ),
        );
    }

    // ── Selection & clipboard ──────────────────────────────────────────────

    pub fn selected_items(&self) -> Vec<FileItem> {
        let Some(selection) = self.imp().selection.get() else {
            return Vec::new();
        };
        let bitset = selection.selection();
        let Some((iter, first)) = gtk::BitsetIter::init_first(&bitset) else {
            return Vec::new();
        };
        std::iter::once(first)
            .chain(iter)
            .filter_map(|i| selection.item(i).and_downcast::<FileItem>())
            .collect()
    }

    /// The files operations on the selection act on (shortcut entries
    /// resolved to their targets).
    pub fn selected_files(&self) -> Vec<gio::File> {
        self.selected_items()
            .iter()
            .map(FileItem::target_file)
            .collect()
    }

    /// Copies the paths of the selected items (or of the current folder) as
    /// text.
    pub fn copy_path(&self) {
        let selected = self.selected_items();
        let paths: Vec<String> = if selected.is_empty() {
            self.location()
                .map(|l| vec![l.parse_name().into()])
                .unwrap_or_default()
        } else {
            selected
                .iter()
                .map(|item| item.file().parse_name().into())
                .collect()
        };
        if paths.is_empty() {
            return;
        }
        self.clipboard().set_text(&paths.join("\n"));
        self.show_toast(&if paths.len() == 1 {
            gettext("Path copied")
        } else {
            gettext("Paths copied")
        });
    }

    // ── File operations ────────────────────────────────────────────────────

    /// Starts `operation`; once done, selects what it created in this
    /// folder (and renames the first item if `rename`).
    pub fn run_operation(&self, operation: Operation, rename: bool) -> FileOpJob {
        let job = queue().submit(operation, Some(self));
        job.connect_finished(clone!(
            #[weak(rename_to = pane)]
            self,
            move |job| {
                let created = job.with_outcome(|o| o.created.clone()).unwrap_or_default();
                pane.select_files(created, rename);
            }
        ));
        job
    }

    /// Copies, moves or links dropped files into `dest`.
    pub fn drop_files(&self, dest: &gio::File, files: &[gio::File], action: gdk::DragAction) {
        if let Some(operation) = dnd::operation_for(files, dest, action) {
            self.run_operation(operation, false);
        }
    }

    /// Selects `files` as soon as they're listed (they may still be on
    /// their way through the file monitor).
    pub fn select_files(&self, files: Vec<gio::File>, rename: bool) {
        let imp = self.imp();
        let Some(location) = self.location() else {
            return;
        };
        let files: Vec<gio::File> = files
            .into_iter()
            .filter(|f| f.has_parent(Some(&location)))
            .collect();
        if files.is_empty() {
            return;
        }
        imp.pending_select.replace(files);
        imp.pending_rename.set(rename);
        if let Some(timeout) = imp.pending_timeout.take() {
            timeout.remove();
        }
        let timeout = glib::timeout_add_local_once(
            SELECT_TIMEOUT,
            clone!(
                #[weak(rename_to = pane)]
                self,
                move || {
                    pane.imp().pending_timeout.take();
                    pane.apply_pending_selection(true);
                }
            ),
        );
        imp.pending_timeout.replace(Some(timeout));
        self.apply_pending_selection(false);
    }

    /// Selects the pending items once they're all listed (or, when
    /// `give_up`, those that are).
    fn apply_pending_selection(&self, give_up: bool) {
        let imp = self.imp();
        if imp.pending_select.borrow().is_empty() || imp.directory.loading() {
            return;
        }
        let items: Vec<Option<FileItem>> = imp
            .pending_select
            .borrow()
            .iter()
            .map(|file| imp.directory.find(file))
            .collect();
        if !give_up && items.iter().any(Option::is_none) {
            return;
        }
        let items: std::collections::HashSet<FileItem> = items.into_iter().flatten().collect();
        imp.pending_select.borrow_mut().clear();
        if let Some(timeout) = imp.pending_timeout.take() {
            timeout.remove();
        }

        let sort_model = imp.sort_model.get().unwrap();
        let selected = gtk::Bitset::new_empty();
        let mut first = None;
        for i in 0..sort_model.n_items() {
            if let Some(item) = sort_model.item(i).and_downcast::<FileItem>()
                && items.contains(&item)
            {
                selected.add(i);
                first = first.or(Some((i, item)));
            }
        }
        let Some((position, item)) = first else {
            return;
        };
        let selection = imp.selection.get().unwrap();
        selection.set_selection(&selected, &gtk::Bitset::new_range(0, sort_model.n_items()));
        imp.file_view.scroll_to(position, false);
        if imp.pending_rename.get() {
            self.rename_item(&item, position);
        }
    }

    /// Renames the first selected item in place.
    pub fn start_rename(&self) {
        let imp = self.imp();
        let Some(item) = self.selected_items().into_iter().next() else {
            return;
        };
        let sort_model = imp.sort_model.get().unwrap();
        let position = (0..sort_model.n_items())
            .find(|&i| sort_model.item(i).and_downcast::<FileItem>().as_ref() == Some(&item));
        if let Some(position) = position {
            self.rename_item(&item, position);
        }
    }

    fn rename_item(&self, item: &FileItem, position: u32) {
        let imp = self.imp();
        let name = item.display_name().to_string();
        let select = naming::stem_char_count(&name, item.is_dir());
        let portable = imp.portable_names.get();
        let directory = imp.directory.clone();
        let file = item.file().clone();
        let original = name.clone();
        let validate = move |text: &str| -> Option<String> {
            if let Err(err) = naming::validate_name(text, portable) {
                return Some(err.message());
            }
            let parent = file.parent()?;
            directory
                .find(&parent.child(text))
                .filter(|existing| text != original && !existing.file().equal(&file))
                .map(|_| gettext("An item named “{}” already exists").replace("{}", text))
        };
        let file = item.file().clone();
        let commit = clone!(
            #[weak(rename_to = pane)]
            self,
            move |name: String| {
                pane.run_operation(
                    Operation::Rename {
                        file: file.clone(),
                        name,
                    },
                    false,
                );
            }
        );
        imp.file_view
            .start_rename(position, &name, select, validate, commit);
    }

    /// Folders that aren't watched live (some network locations) are
    /// reloaded after an operation changed them.
    fn on_job_finished(&self, job: &FileOpJob) {
        let Some(location) = self.location() else {
            return;
        };
        if self.imp().directory.is_monitored() && location.is_native() {
            return;
        }
        let operation = job.operation();
        let touches = operation.destination().is_some_and(|d| d.equal(&location))
            || operation
                .sources()
                .iter()
                .any(|s| s.parent().is_some_and(|p| p.equal(&location)));
        if touches {
            self.reload();
        }
    }

    /// Finds out whether the folder is writable and needs portable names.
    fn update_location_info(&self) {
        let imp = self.imp();
        if let Some(task) = imp.info_task.take() {
            task.abort();
        }
        let Some(location) = self.location() else {
            return;
        };
        // Until we know, assume local folders are writable.
        self.set_writable(!imp.is_trash.get() && location.is_native());
        imp.portable_names.set(false);
        let task = glib::spawn_future_local(clone!(
            #[weak(rename_to = pane)]
            self,
            async move {
                let can_write = location
                    .query_info_future(
                        gio::FILE_ATTRIBUTE_ACCESS_CAN_WRITE,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::LOW,
                    )
                    .await
                    .map(|info| {
                        !info.has_attribute(gio::FILE_ATTRIBUTE_ACCESS_CAN_WRITE)
                            || info.boolean(gio::FILE_ATTRIBUTE_ACCESS_CAN_WRITE)
                    })
                    .unwrap_or(false);
                pane.set_writable(can_write && !pane.is_trash());
                let fs_type = location
                    .query_filesystem_info_future(
                        gio::FILE_ATTRIBUTE_FILESYSTEM_TYPE,
                        glib::Priority::LOW,
                    )
                    .await
                    .ok()
                    .and_then(|info| info.attribute_string(gio::FILE_ATTRIBUTE_FILESYSTEM_TYPE));
                pane.imp()
                    .portable_names
                    .set(fs_type.is_some_and(|t| naming::needs_portable_names(&t)));
            }
        ));
        imp.info_task.replace(Some(task));
    }

    fn set_writable(&self, writable: bool) {
        let imp = self.imp();
        imp.file_view
            .set_accepts_drops(writable || imp.is_trash.get());
        if imp.writable.replace(writable) != writable {
            self.notify_writable();
        }
    }

    // ── View options ───────────────────────────────────────────────────────

    pub fn select_all(&self) {
        if let Some(selection) = self.imp().selection.get() {
            selection.select_all();
        }
    }

    pub fn invert_selection(&self) {
        let Some(selection) = self.imp().selection.get() else {
            return;
        };
        let n = selection.n_items();
        let selected = selection.selection();
        let inverted = gtk::Bitset::new_range(0, n);
        inverted.subtract(&selected);
        selection.set_selection(&inverted, &gtk::Bitset::new_range(0, n));
    }

    pub fn sort_state(&self) -> (String, bool) {
        self.imp().file_view.sort_state()
    }

    pub fn set_sort(&self, id: &str, descending: bool) {
        self.imp().file_view.set_sort(id, descending);
    }

    pub fn connect_sort_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "sort-changed",
            false,
            glib::closure_local!(move |pane: &Self| f(pane)),
        )
    }

    pub fn connect_selection_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "selection-changed",
            false,
            glib::closure_local!(move |pane: &Self| f(pane)),
        )
    }

    pub fn zoom(&self, step: i32) {
        let current = self.imp().file_view.icon_size();
        let index = ICON_SIZES
            .iter()
            .position(|&s| s >= current)
            .unwrap_or(ICON_SIZES.len() - 1) as i32;
        let next = ICON_SIZES[(index + step).clamp(0, ICON_SIZES.len() as i32 - 1) as usize];
        let _ = settings().set_int("icon-size", next);
    }

    pub fn zoom_reset(&self) {
        let _ = settings().set_int("icon-size", DEFAULT_ICON_SIZE);
    }

    fn set_show_hidden(&self, show: bool) {
        let imp = self.imp();
        imp.filter_state.show_hidden.set(show);
        imp.filter.get().unwrap().changed(if show {
            gtk::FilterChange::LessStrict
        } else {
            gtk::FilterChange::MoreStrict
        });
    }

    fn apply_search_text(&self) {
        let imp = self.imp();
        let query = imp.search_text.borrow().trim().to_lowercase();
        let previous = imp.filter_state.query.replace(query.clone());
        if previous == query {
            return;
        }
        let change = if query.contains(previous.as_str()) {
            gtk::FilterChange::MoreStrict
        } else if previous.contains(query.as_str()) {
            gtk::FilterChange::LessStrict
        } else {
            gtk::FilterChange::Different
        };
        imp.filter.get().unwrap().changed(change);
        self.update_content_page();
    }

    // ── Status & placeholder pages ─────────────────────────────────────────

    fn update_content_page(&self) {
        let imp = self.imp();
        let directory = &imp.directory;
        let visible = imp.sort_model.get().map_or(0, |m| m.n_items());
        let page = &imp.status_page;

        let name = if let Some(err) = directory.error().filter(|_| !directory.loading()) {
            let trash_unavailable =
                err.matches(gio::IOErrorEnum::NotSupported) && imp.is_trash.get();
            let (icon, title) = if trash_unavailable {
                ("user-trash-symbolic", gettext("Trash Unavailable"))
            } else if err.matches(gio::IOErrorEnum::PermissionDenied) {
                ("action-unavailable-symbolic", gettext("Access Denied"))
            } else if err.matches(gio::IOErrorEnum::NotFound) {
                ("folder-symbolic", gettext("Folder Not Found"))
            } else if err.matches(gio::IOErrorEnum::NotDirectory) {
                ("folder-symbolic", gettext("Not a Folder"))
            } else {
                ("dialog-warning-symbolic", gettext("Unable to Open Folder"))
            };
            page.set_icon_name(Some(icon));
            page.set_title(&title);
            let description = if trash_unavailable {
                gettext(
                    "Browsing the Trash needs GVfs: install the “gvfs” package. \
                     Moving files to the Trash and undoing it work without it.",
                )
            } else {
                err.message().to_owned()
            };
            page.set_description(Some(&glib::markup_escape_text(&description)));
            "status"
        } else if visible > 0 {
            "view"
        } else if directory.loading() {
            "loading"
        } else if !imp.filter_state.query.borrow().is_empty() {
            page.set_icon_name(Some("system-search-symbolic"));
            page.set_title(&gettext("No Results Found"));
            page.set_description(Some(&gettext("Try a different search")));
            "status"
        } else {
            page.set_icon_name(Some("folder-symbolic"));
            page.set_title(&gettext("Folder Is Empty"));
            page.set_description(
                if directory.n_items() > 0 {
                    Some(gettext(
                        "Hidden files are not shown. Press Ctrl+H to show them.",
                    ))
                } else {
                    None
                }
                .as_deref(),
            );
            "status"
        };
        if imp.content_stack.visible_child_name().as_deref() != Some(name) {
            imp.content_stack.set_visible_child_name(name);
        }
    }

    fn update_status(&self) {
        let imp = self.imp();
        let visible = imp.sort_model.get().map_or(0, |m| m.n_items());
        self.set_text(
            &imp.item_count_text,
            format::item_count(visible),
            Self::notify_item_count_text,
        );

        let selected = self.selected_items();
        let label = if selected.is_empty() {
            String::new()
        } else {
            let count = selected.len() as u32;
            let mut text =
                ngettext("{} selected", "{} selected", count).replace("{}", &count.to_string());
            let files: Vec<&FileItem> = selected.iter().filter(|i| !i.is_dir()).collect();
            if !files.is_empty() {
                let total: u64 = files.iter().map(|i| i.size()).sum();
                text.push_str(&format!(" ({})", format::size(total)));
            }
            text
        };
        self.set_text(&imp.selection_text, label, Self::notify_selection_text);
        if imp.n_selected.get() != selected.len() as u32 {
            imp.n_selected.set(selected.len() as u32);
            self.notify_n_selected();
        }
    }

    /// Updates one of the status text properties, notifying on change.
    fn set_text(&self, field: &RefCell<String>, text: String, notify: fn(&Self)) {
        if *field.borrow() == text {
            return;
        }
        field.replace(text);
        notify(self);
    }

    fn update_free_space(&self) {
        let imp = self.imp();
        if let Some(task) = imp.free_space_task.take() {
            task.abort();
        }
        self.set_text(
            &imp.free_space_text,
            String::new(),
            Self::notify_free_space_text,
        );
        let Some(location) = self.location() else {
            return;
        };
        let task = glib::spawn_future_local(clone!(
            #[weak(rename_to = pane)]
            self,
            async move {
                let Ok(info) = location
                    .query_filesystem_info_future(
                        gio::FILE_ATTRIBUTE_FILESYSTEM_FREE,
                        glib::Priority::LOW,
                    )
                    .await
                else {
                    return;
                };
                if info.has_attribute(gio::FILE_ATTRIBUTE_FILESYSTEM_FREE) {
                    let free = info.attribute_uint64(gio::FILE_ATTRIBUTE_FILESYSTEM_FREE);
                    pane.set_text(
                        &pane.imp().free_space_text,
                        gettext("{} free").replace("{}", &format::size(free)),
                        Self::notify_free_space_text,
                    );
                }
            }
        ));
        imp.free_space_task.replace(Some(task));
    }

    fn show_toast(&self, message: &str) {
        if let Some(window) = self.root().and_downcast::<crate::window::Window>() {
            window.show_toast(message);
        }
    }
}

/// A short user-facing name for a location ("Home", "Documents", "/").
pub fn display_name(file: &gio::File) -> String {
    if file.equal(&gio::File::for_path(glib::home_dir())) {
        return gettext("Home");
    }
    match file.uri_scheme().as_deref() {
        Some("trash") if file.parent().is_none() => return gettext("Trash"),
        Some("recent") if file.parent().is_none() => return gettext("Recent"),
        _ => {}
    }
    file.basename()
        .map(|b| b.to_string_lossy().into_owned())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| file.parse_name().into())
}
