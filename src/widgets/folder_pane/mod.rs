//! `FolderPane`: the contents of one tab — the file view and its
//! empty/error states. The window owns the address bar, search entry and
//! status bar and binds them to the active pane's properties.
//!
//! Model chain:
//! `DirectoryModel → FilterListModel (hidden, search, filter chips) →
//!  SortListModel (folders first + column sorter) → MultiSelection → FileView`
//!
//! Searching in subfolders swaps the `DirectoryModel` for a list of search
//! results; the rest of the chain (filters, sorting, selection) stays.
//!
//! File operations started from the pane (drops, renames…) go to the job
//! queue; once done, what they created in this folder gets selected.
//!
//! The view mode, sort and grouping are per folder: changing them in a
//! folder remembers them for it (see `model::view_settings`); other folders
//! use the defaults.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use gettextrs::{gettext, ngettext};
use gtk::{gdk, gio, glib, glib::clone, prelude::*, subclass::prelude::*};

use crate::dialogs;
use crate::model::directory_model::ATTRIBUTES;
use crate::model::grouping::{self, GroupBy, Periods};
use crate::model::places;
use crate::model::search_filters::{DateRange, Filters, Kind, SizeRange};
use crate::model::view_settings::{self, FolderView};
use crate::model::{DirectoryModel, FileItem, History, sorting};
use crate::services::file_ops::{FileOpJob, Operation, naming, queue};
use crate::services::search::{self, Search};
use crate::services::volumes;
use crate::util::{format, sandbox, settings};
use crate::widgets::dnd;
use crate::widgets::file_view::{DEFAULT_ICON_SIZE, FileView, ICON_SIZES, ViewMode};
use crate::widgets::this_pc::ThisPcPage;

/// How long to wait for the items an operation created to show up.
const SELECT_TIMEOUT: Duration = Duration::from_secs(3);

/// State read by the filter closure.
#[derive(Default)]
struct FilterState {
    directories_only: Cell<bool>,
    file_filter: RefCell<Option<gtk::FileFilter>>,
    show_hidden: Cell<bool>,
    /// Lower-cased search text.
    query: RefCell<String>,
    filters: Cell<Filters>,
    /// The earliest modification time the date filter accepts.
    earliest: Cell<Option<i64>>,
}

impl FilterState {
    fn matches(&self, item: &FileItem) -> bool {
        if !item.is_dir()
            && (self.directories_only.get()
                || self
                    .file_filter
                    .borrow()
                    .as_ref()
                    .is_some_and(|f| !f.match_(item.info())))
        {
            return false;
        }
        if !self.show_hidden.get() && item.is_hidden() {
            return false;
        }
        let filters = self.filters.get();
        if filters.is_active()
            && !filters.matches(
                item.content_type().as_deref().unwrap_or_default(),
                item.is_dir(),
                item.size(),
                item.modified(),
                self.earliest.get(),
            )
        {
            return false;
        }
        let query = self.query.borrow();
        query.is_empty() || search::name_matches(&item.display_name(), &query)
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
        #[template_child]
        pub(super) this_pc_page: TemplateChild<ThisPcPage>,
        #[template_child]
        pub(super) search_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub(super) subfolders_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) search_status: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) search_spinner: TemplateChild<adw::Spinner>,
        #[template_child]
        pub(super) search_status_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) kind_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(super) date_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(super) size_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(super) clear_filters_button: TemplateChild<gtk::Button>,

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
        /// Showing search results from the folder and its subfolders
        /// rather than the folder's contents.
        #[property(get)]
        pub(super) recursive: Cell<bool>,
        /// A search in subfolders is running.
        pub(super) searching: Cell<bool>,
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
        /// Showing the "This PC" page rather than a folder.
        pub(super) this_pc: Cell<bool>,
        /// The location that wasn't mounted and that mounting was tried
        /// for (once), and whether it's being mounted now.
        pub(super) mount_attempted: RefCell<Option<gio::File>>,
        pub(super) mounting: Cell<bool>,

        pub(super) history: RefCell<History<gio::File>>,
        pub(super) directory: DirectoryModel,
        pub(super) search_results: OnceCell<gio::ListStore>,
        pub(super) search_task: RefCell<Option<glib::JoinHandle<()>>>,
        pub(super) search_actions: OnceCell<gio::SimpleActionGroup>,
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
        /// Applying a folder's view settings: changes aren't remembered.
        pub(super) restoring_view: Cell<bool>,
        pub(super) picker: Cell<bool>,
        pub(super) picker_multiple: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FolderPane {
        const NAME: &'static str = "LiaFolderPane";
        type Type = super::FolderPane;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            FileView::ensure_type();
            ThisPcPage::ensure_type();
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
                    // The view mode or grouping changed.
                    glib::subclass::Signal::builder("view-changed").build(),
                    glib::subclass::Signal::builder("file-picked")
                        .param_types([gio::File::static_type()])
                        .build(),
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
            for task in [
                self.free_space_task.take(),
                self.info_task.take(),
                self.search_task.take(),
            ]
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
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
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
                    pane.mount_if_needed();
                }
                pane.update_loading();
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
            move |_| {
                pane.remember_view();
                pane.emit_by_name::<()>("sort-changed", &[]);
            }
        ));
        selection.connect_selection_changed(clone!(
            #[weak(rename_to = pane)]
            self,
            move |selection, position, _| {
                if pane.imp().picker.get()
                    && !pane.imp().picker_multiple.get()
                    && selection.selection().size() > 1
                {
                    selection.select_item(position, true);
                    return;
                }
                pane.update_status();
                pane.emit_by_name::<()>("selection-changed", &[]);
            }
        ));

        imp.file_view.connect_item_activated(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_, item| pane.open_item(item, false)
        ));
        imp.this_pc_page.connect_open_location(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_, location, new_tab| {
                match pane.root().and_downcast::<crate::window::Window>() {
                    Some(window) if new_tab => {
                        window.open_tab(location, false);
                    }
                    _ => pane.navigate_to(location),
                }
            }
        ));
        imp.this_pc_page.connect_n_devices_notify(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_| pane.update_status()
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
        imp.file_view.set_mode(default_view_mode());
        imp.file_view.set_icon_size(settings.int("icon-size"));
        imp.file_view
            .set_single_click(settings.boolean("single-click"));

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
            // Folders with their own view keep it.
            settings.connect_changed(
                Some("default-view"),
                clone!(
                    #[weak(rename_to = pane)]
                    self,
                    move |_, _| {
                        if pane
                            .location()
                            .is_some_and(|l| view_settings::lookup(&l).is_none())
                        {
                            pane.apply_view_settings();
                        }
                    }
                ),
            ),
            settings.connect_changed(
                Some("single-click"),
                clone!(
                    #[weak(rename_to = pane)]
                    self,
                    move |settings, key| pane
                        .imp()
                        .file_view
                        .set_single_click(settings.boolean(key))
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
        imp.search_results
            .set(gio::ListStore::new::<FileItem>())
            .unwrap();
        self.setup_search_bar();
    }

    /// The `search.*` actions behind the search bar: searching in
    /// subfolders and the Kind / Date / Size filter chips.
    fn setup_search_bar(&self) {
        let imp = self.imp();
        let group = gio::SimpleActionGroup::new();
        for name in ["kind", "date", "size"] {
            let action =
                gio::SimpleAction::new_stateful(name, Some(glib::VariantTy::STRING), &"any".into());
            action.connect_change_state(clone!(
                #[weak(rename_to = pane)]
                self,
                move |action, state| {
                    if let Some(state) = state {
                        action.set_state(state);
                        pane.on_filters_changed();
                    }
                }
            ));
            group.add_action(&action);
        }
        let subfolders = gio::SimpleAction::new("subfolders", None);
        subfolders.connect_activate(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_, _| pane.search_subfolders()
        ));
        group.add_action(&subfolders);
        let clear = gio::SimpleAction::new("clear-filters", None);
        clear.connect_activate(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_, _| pane.clear_filters()
        ));
        group.add_action(&clear);
        self.insert_action_group("search", Some(&group));

        let menu = |action: &str, choices: Vec<(&'static str, String)>| {
            let menu = gio::Menu::new();
            for (nick, label) in choices {
                let item = gio::MenuItem::new(Some(&label), None);
                item.set_action_and_target_value(
                    Some(&format!("search.{action}")),
                    Some(&nick.to_variant()),
                );
                menu.append_item(&item);
            }
            menu
        };
        imp.kind_button.set_menu_model(Some(&menu(
            "kind",
            Kind::ALL.map(|k| (k.nick(), k.label())).to_vec(),
        )));
        imp.date_button.set_menu_model(Some(&menu(
            "date",
            DateRange::ALL.map(|d| (d.nick(), d.label())).to_vec(),
        )));
        imp.size_button.set_menu_model(Some(&menu(
            "size",
            SizeRange::ALL.map(|s| (s.nick(), s.label())).to_vec(),
        )));

        imp.search_actions.set(group).unwrap();
        self.update_search_bar();
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
        if imp.this_pc.get() {
            return;
        }
        if imp.recursive.get() {
            self.start_search();
            return;
        }
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

        // A new folder starts with an empty search and no filters.
        self.exit_search();
        imp.filter_state.query.borrow_mut().clear();
        self.set_search_text("");
        self.clear_filters();

        imp.pending_select.borrow_mut().clear();
        imp.mount_attempted.replace(None);
        imp.mounting.set(false);
        let this_pc = places::is_this_pc(location);
        imp.this_pc.set(this_pc);
        // Before listing, so that items arrive in the right order.
        self.apply_view_settings();
        if this_pc {
            imp.directory.clear();
        } else {
            imp.directory.load(location);
        }
        imp.file_view.set_location(Some(location));
        let is_trash = dnd::is_trash(location);
        imp.file_view.set_trash_mode(is_trash);
        if imp.is_trash.replace(is_trash) != is_trash {
            self.notify_is_trash();
        }
        self.update_free_space();
        self.update_location_info();
        self.update_title();
        self.update_content_page();
        self.update_status();
        self.notify_location();
    }

    /// The folder's name, or "Search Results in …" when searching
    /// subfolders.
    fn update_title(&self) {
        let imp = self.imp();
        let Some(location) = self.location() else {
            return;
        };
        let name = display_name(&location);
        let title = if imp.recursive.get() {
            gettext("Search Results in {}").replace("{}", &name)
        } else {
            name
        };
        if *imp.title.borrow() != title {
            imp.title.replace(title);
            self.notify_title();
        }
    }

    /// The tab shows a spinner while the folder loads or a search runs.
    fn update_loading(&self) {
        let imp = self.imp();
        let loading = imp.directory.loading() || imp.searching.get();
        if imp.loading.replace(loading) != loading {
            self.notify_loading();
        }
    }

    /// While a folder streams in, sorted batches insert items above the
    /// visible ones; the view would follow its anchor item downwards.
    fn keep_at_top_while_loading(&self) {
        let imp = self.imp();
        if (imp.directory.loading() || imp.searching.get())
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
            self.apply_pending_selection(false);
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

    /// A network location that isn't mounted yet (typed in the address
    /// bar, a bookmark, a restored tab…) is mounted, then listed.
    fn mount_if_needed(&self) {
        let imp = self.imp();
        let Some(location) = self.location() else {
            return;
        };
        let not_mounted = imp
            .directory
            .error()
            .is_some_and(|err| err.matches(gio::IOErrorEnum::NotMounted));
        let attempted = imp
            .mount_attempted
            .borrow()
            .as_ref()
            .is_some_and(|l| l.equal(&location));
        if !not_mounted || attempted {
            return;
        }
        imp.mount_attempted.replace(Some(location.clone()));
        imp.mounting.set(true);
        self.update_content_page();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = pane)]
            self,
            async move {
                let result = volumes::mount_location(&location, Some(pane.upcast_ref())).await;
                let imp = pane.imp();
                if !pane.location().is_some_and(|l| l.equal(&location)) {
                    return;
                }
                imp.mounting.set(false);
                match result {
                    Ok(()) => {
                        imp.directory.load(&location);
                        pane.update_free_space();
                        pane.update_location_info();
                    }
                    Err(err) => {
                        if !volumes::is_handled(&err) {
                            tracing::warn!("cannot mount {}: {err}", location.uri());
                        }
                        pane.update_content_page();
                    }
                }
            }
        ));
    }

    /// Whether the pane shows the "This PC" page.
    pub fn is_this_pc(&self) -> bool {
        self.imp().this_pc.get()
    }

    // ── Opening ────────────────────────────────────────────────────────────

    /// Opens folders in place and files with their default application
    /// (or an app chooser when `choose_app` is set).
    pub fn open_item(&self, item: &FileItem, choose_app: bool) {
        if self.imp().picker.get() {
            if let Some(folder) = Self::folder_target(item) {
                self.navigate_to(&folder);
            } else {
                self.emit_by_name::<()>("file-picked", &[&item.target_file()]);
            }
            return;
        }
        // Trashed items point at their real file, but folders are browsed
        // within the trash (where deleting is permanent).
        if dnd::is_trash(item.file()) {
            if item.is_dir() && !choose_app {
                self.navigate_to(item.file());
            } else {
                self.launch(&item.target_file(), choose_app);
            }
        } else if let Some(target) = item.target_uri() {
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
        if dnd::is_trash(item.file()) {
            return Some(item.file().clone());
        }
        Some(
            item.target_uri()
                .map_or_else(|| item.file().clone(), |uri| gio::File::for_uri(&uri)),
        )
    }

    pub fn open_selection(&self, choose_app: bool) {
        let items = self.selected_items();
        // Our own chooser lists the host's apps; a sandbox only sees its
        // own, so there the portal asks instead.
        if choose_app && !sandbox::is_flatpak() {
            let files: Vec<&FileItem> = items.iter().filter(|i| !i.is_dir()).collect();
            let targets: Vec<gio::File> = files.iter().map(|i| i.target_file()).collect();
            let types: Vec<String> = files
                .iter()
                .map(|i| {
                    i.content_type()
                        .map_or_else(|| "application/octet-stream".to_owned(), String::from)
                })
                .collect();
            dialogs::open_with::present(self, targets, &types);
            return;
        }
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

    /// Renames the selected item in place, or several in the batch rename
    /// dialog.
    pub fn start_rename(&self) {
        let imp = self.imp();
        let selected = self.selected_items();
        if selected.len() > 1 {
            dialogs::batch_rename::present(
                self,
                selected,
                self.existing_names(),
                imp.portable_names.get(),
            );
            return;
        }
        let Some(item) = selected.into_iter().next() else {
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
            // Search results may be in other folders than the one listed.
            let parent = file
                .parent()
                .filter(|p| directory.location().is_some_and(|l| l.equal(p)))?;
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

    /// The folder's file system needs Windows-compatible names.
    pub fn portable_names(&self) -> bool {
        self.imp().portable_names.get()
    }

    /// The (folder URI, name) pairs of the items listed, for checking new
    /// names.
    fn existing_names(&self) -> std::collections::HashSet<(String, String)> {
        let imp = self.imp();
        let Some(folder) = imp.directory.location().map(|l| l.uri().to_string()) else {
            return Default::default();
        };
        (0..imp.directory.n_items())
            .filter_map(|i| imp.directory.item(i).and_downcast::<FileItem>())
            .map(|item| (folder.clone(), item.display_name().to_string()))
            .collect()
    }

    /// Folders that aren't watched live (some network locations) are
    /// reloaded after an operation changed them.
    fn on_job_finished(&self, job: &FileOpJob) {
        if self.recursive() {
            self.update_search_results(job);
            return;
        }
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
        if imp.writable.replace(writable) != writable {
            self.notify_writable();
        }
        self.update_accepts_drops();
    }

    /// Files can be dropped on the background of a writable folder (or the
    /// trash), but not on search results: they come from many folders.
    fn update_accepts_drops(&self) {
        let imp = self.imp();
        imp.file_view.set_accepts_drops(
            !imp.picker.get() && (imp.writable.get() || imp.is_trash.get()) && !imp.recursive.get(),
        );
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

    /// Moves the selection `delta` items away from the first selected one
    /// (stepping through items in Quick Look) and returns the new item.
    pub fn select_relative(&self, delta: i32) -> Option<FileItem> {
        let imp = self.imp();
        let selection = imp.selection.get()?;
        let n = selection.n_items();
        if n == 0 {
            return None;
        }
        let bitset = selection.selection();
        let next = if bitset.is_empty() {
            0
        } else {
            (bitset.minimum() as i64 + delta as i64).clamp(0, n as i64 - 1) as u32
        };
        imp.file_view.scroll_to(next, true);
        selection.item(next).and_downcast()
    }

    pub fn sort_state(&self) -> (String, bool) {
        self.imp().file_view.sort_state()
    }

    pub fn view_mode(&self) -> ViewMode {
        self.imp().file_view.mode()
    }

    /// Changes the view of this folder (remembered for it).
    pub fn set_view_mode(&self, mode: ViewMode) {
        if self.view_mode() == mode {
            return;
        }
        self.imp().file_view.set_mode(mode);
        self.remember_view();
        self.emit_by_name::<()>("view-changed", &[]);
    }

    pub fn group_by(&self) -> GroupBy {
        self.imp().file_view.group_by()
    }

    /// Groups this folder's items (remembered for it).
    pub fn set_group_by(&self, group: GroupBy) {
        if self.group_by() == group {
            return;
        }
        self.apply_group_by(group);
        self.remember_view();
        self.emit_by_name::<()>("view-changed", &[]);
    }

    fn apply_group_by(&self, group: GroupBy) {
        let imp = self.imp();
        let periods = Periods::now();
        imp.sort_model
            .get()
            .unwrap()
            .set_section_sorter(grouping::section_sorter(group, periods).as_ref());
        imp.file_view.set_group_by(group, periods);
    }

    /// Shows the folder the way it was last shown, or with the defaults.
    fn apply_view_settings(&self) {
        let imp = self.imp();
        let Some(location) = self.location() else {
            return;
        };
        let saved = (!imp.this_pc.get())
            .then(|| view_settings::lookup(&location))
            .flatten();
        let (mode, sort, descending, group) = match saved {
            Some(view) => (
                ViewMode::from_nick(&view.view),
                view.sort,
                view.descending,
                GroupBy::from_nick(&view.group),
            ),
            None => (default_view_mode(), "name".to_owned(), false, GroupBy::None),
        };
        imp.restoring_view.set(true);
        imp.file_view.set_mode(mode);
        if self.sort_state() != (sort.clone(), descending) {
            imp.file_view.set_sort(&sort, descending);
        }
        if self.group_by() != group {
            self.apply_group_by(group);
        }
        imp.restoring_view.set(false);
        self.emit_by_name::<()>("view-changed", &[]);
    }

    /// Remembers the folder's view mode, sort and grouping; folders shown
    /// with the defaults are forgotten.
    fn remember_view(&self) {
        let imp = self.imp();
        if imp.restoring_view.get() || imp.this_pc.get() || imp.recursive.get() {
            return;
        }
        let Some(location) = self.location() else {
            return;
        };
        let mode = self.view_mode();
        let (sort, descending) = self.sort_state();
        let group = self.group_by();
        let is_default =
            mode == default_view_mode() && sort == "name" && !descending && group == GroupBy::None;
        let view = FolderView {
            view: mode.nick().to_owned(),
            sort,
            descending,
            group: group.nick().to_owned(),
            used: 0,
        };
        view_settings::remember(&location, (!is_default).then_some(view));
    }

    pub fn connect_view_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "view-changed",
            false,
            glib::closure_local!(move |pane: &Self| f(pane)),
        )
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

    pub fn set_show_hidden(&self, show: bool) {
        let imp = self.imp();
        imp.filter_state.show_hidden.set(show);
        // The search skipped hidden folders: search again.
        if show && imp.recursive.get() {
            self.start_search();
        }
        imp.filter.get().unwrap().changed(if show {
            gtk::FilterChange::LessStrict
        } else {
            gtk::FilterChange::MoreStrict
        });
    }

    /// Picker panes browse normally but return files instead of launching apps.
    pub fn set_picker_mode(&self, directories: bool, multiple: bool) {
        self.imp().file_view.set_picker_mode();
        self.imp().picker.set(true);
        self.imp().picker_multiple.set(multiple);
        self.imp().filter_state.directories_only.set(directories);
        self.imp().file_view.set_accepts_drops(false);
        self.imp()
            .filter
            .get()
            .unwrap()
            .changed(gtk::FilterChange::Different);
    }

    pub fn set_picker_filter(&self, filter: Option<gtk::FileFilter>) {
        self.imp().filter_state.file_filter.replace(filter);
        self.imp()
            .filter
            .get()
            .unwrap()
            .changed(gtk::FilterChange::Different);
    }

    pub fn connect_file_picked<F: Fn(&Self, &gio::File) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "file-picked",
            false,
            glib::closure_local!(move |pane: &Self, file: &gio::File| f(pane, file)),
        )
    }

    fn apply_search_text(&self) {
        let imp = self.imp();
        let query = imp.search_text.borrow().trim().to_lowercase();
        let previous = imp.filter_state.query.replace(query.clone());
        if previous == query {
            return;
        }
        // A search in subfolders follows the text; clearing it goes back
        // to the folder.
        if imp.recursive.get() {
            if query.is_empty() {
                self.exit_search();
            } else {
                self.start_search();
            }
        }
        let change = if query.contains(previous.as_str()) {
            gtk::FilterChange::MoreStrict
        } else if previous.contains(query.as_str()) {
            gtk::FilterChange::LessStrict
        } else {
            gtk::FilterChange::Different
        };
        imp.filter.get().unwrap().changed(change);
        self.update_search_bar();
        self.update_content_page();
    }

    // ── Search ─────────────────────────────────────────────────────────────

    /// Shows the files matching the search text in the folder and all its
    /// subfolders, instead of only the folder's contents.
    pub fn search_subfolders(&self) {
        let imp = self.imp();
        if imp.filter_state.query.borrow().is_empty() || self.location().is_none() {
            return;
        }
        if !imp.recursive.replace(true) {
            imp.filter_model
                .get()
                .unwrap()
                .set_model(imp.search_results.get());
            imp.file_view.set_search_mode(true);
            self.update_accepts_drops();
            self.update_title();
            self.notify_recursive();
        }
        self.start_search();
    }

    /// (Re)starts the search in subfolders with the current text.
    fn start_search(&self) {
        let imp = self.imp();
        self.cancel_search();
        let results = imp.search_results.get().unwrap();
        results.remove_all();
        let query = search::Query {
            text: imp.filter_state.query.borrow().clone(),
            show_hidden: imp.filter_state.show_hidden.get(),
        };
        let Some(root) = self.location().filter(|_| !query.text.is_empty()) else {
            return;
        };
        let search = Search::start(&root, query);
        // Only a weak reference while waiting: the search may run for a
        // long time and must not keep a closed tab alive.
        let pane = self.downgrade();
        let task = glib::spawn_future_local(async move {
            while let Some(event) = search.next().await {
                let Some(pane) = pane.upgrade() else {
                    return;
                };
                match event {
                    search::Event::Found(matches) => {
                        let items: Vec<FileItem> =
                            matches.into_iter().map(search::Match::into_item).collect();
                        pane.imp()
                            .search_results
                            .get()
                            .unwrap()
                            .extend_from_slice(&items);
                    }
                    search::Event::LimitReached => pane.show_toast(
                        &gettext("Search stopped after {} results")
                            .replace("{}", &search::MAX_RESULTS.to_string()),
                    ),
                }
            }
            if let Some(pane) = pane.upgrade() {
                pane.imp().search_task.take();
                pane.set_searching(false);
            }
        });
        imp.search_task.replace(Some(task));
        self.set_searching(true);
    }

    /// Stops the running search, keeping the results found so far.
    fn cancel_search(&self) {
        if let Some(task) = self.imp().search_task.take() {
            // Dropping the search cancels the walk.
            task.abort();
        }
        self.set_searching(false);
    }

    /// Goes back from search results to the folder's contents.
    fn exit_search(&self) {
        let imp = self.imp();
        self.cancel_search();
        if !imp.recursive.replace(false) {
            return;
        }
        imp.filter_model
            .get()
            .unwrap()
            .set_model(Some(&imp.directory));
        imp.search_results.get().unwrap().remove_all();
        imp.file_view.set_search_mode(false);
        self.update_accepts_drops();
        self.update_title();
        self.update_search_bar();
        self.update_content_page();
        self.notify_recursive();
    }

    fn set_searching(&self, searching: bool) {
        let imp = self.imp();
        if imp.searching.replace(searching) == searching {
            return;
        }
        self.update_loading();
        self.update_search_bar();
        self.update_content_page();
    }

    /// Search results aren't watched for changes: after an operation,
    /// drop the results that are gone and add new files that match
    /// (renamed, duplicated…).
    fn update_search_results(&self, job: &FileOpJob) {
        let Some(root) = self.location() else {
            return;
        };
        let sources = job.operation().sources();
        let created = job.with_outcome(|o| o.created.clone()).unwrap_or_default();
        if sources.is_empty() && created.is_empty() {
            return;
        }
        glib::spawn_future_local(clone!(
            #[weak(rename_to = pane)]
            self,
            async move {
                let results = pane.imp().search_results.get().unwrap().clone();
                let position = |file: &gio::File| {
                    (0..results.n_items()).find(|&i| {
                        results
                            .item(i)
                            .and_downcast::<FileItem>()
                            .is_some_and(|item| item.file().equal(file))
                    })
                };
                for source in sources {
                    let gone = source
                        .query_info_future(
                            gio::FILE_ATTRIBUTE_STANDARD_TYPE,
                            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
                            glib::Priority::DEFAULT,
                        )
                        .await
                        .is_err_and(|err| err.matches(gio::IOErrorEnum::NotFound));
                    if gone && let Some(i) = position(&source) {
                        results.remove(i);
                    }
                }
                for file in created {
                    let query = pane.imp().filter_state.query.borrow().clone();
                    let inside = file.has_prefix(&root);
                    let name = file
                        .basename()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if !pane.recursive()
                        || !inside
                        || !search::name_matches(&name, &query)
                        || position(&file).is_some()
                    {
                        continue;
                    }
                    if let Ok(info) = file
                        .query_info_future(
                            ATTRIBUTES,
                            gio::FileQueryInfoFlags::NONE,
                            glib::Priority::DEFAULT,
                        )
                        .await
                    {
                        results.append(&FileItem::new(file, info));
                    }
                }
            }
        ));
    }

    fn on_filters_changed(&self) {
        let imp = self.imp();
        let state = |name: &str| {
            imp.search_actions
                .get()
                .and_then(|group| group.action_state(name))
                .and_then(|state| state.get::<String>())
                .unwrap_or_default()
        };
        let filters = Filters {
            kind: Kind::from_nick(&state("kind")),
            date: DateRange::from_nick(&state("date")),
            size: SizeRange::from_nick(&state("size")),
        };
        let now = glib::DateTime::now_local().ok();
        let earliest = now.and_then(|now| {
            let today =
                glib::DateTime::from_local(now.year(), now.month(), now.day_of_month(), 0, 0, 0.0)
                    .ok()?;
            filters.date.earliest(now.to_unix(), today.to_unix())
        });
        imp.filter_state.earliest.set(earliest);
        if imp.filter_state.filters.replace(filters) != filters {
            imp.filter
                .get()
                .unwrap()
                .changed(gtk::FilterChange::Different);
        }
        self.update_search_bar();
        self.update_content_page();
    }

    fn clear_filters(&self) {
        let Some(group) = self.imp().search_actions.get() else {
            return;
        };
        for name in ["kind", "date", "size"] {
            if let Some(action) = group
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                // `set_state` doesn't emit `change-state`: apply once below.
                action.set_state(&"any".to_variant());
            }
        }
        self.on_filters_changed();
    }

    /// Shows the search bar while there is something to search or filter,
    /// with the chips' current values.
    fn update_search_bar(&self) {
        let imp = self.imp();
        let recursive = imp.recursive.get();
        let searching = imp.searching.get();
        let filters = imp.filter_state.filters.get();
        let has_query = !imp.filter_state.query.borrow().is_empty();

        imp.search_revealer
            .set_reveal_child(has_query || recursive || filters.is_active());
        imp.subfolders_button.set_visible(!recursive);
        imp.search_status.set_visible(recursive);
        imp.search_spinner.set_visible(searching);
        imp.search_status_label.set_label(&if searching {
            gettext("Searching subfolders…")
        } else {
            gettext("Including subfolders")
        });
        imp.clear_filters_button.set_visible(filters.is_active());

        let chip = |button: &gtk::MenuButton, active: bool, label: String| {
            button.set_label(&label);
            if active {
                button.add_css_class("active");
            } else {
                button.remove_css_class("active");
            }
        };
        chip(
            &imp.kind_button,
            filters.kind != Kind::Any,
            if filters.kind == Kind::Any {
                gettext("Kind")
            } else {
                filters.kind.label()
            },
        );
        chip(
            &imp.date_button,
            filters.date != DateRange::Any,
            if filters.date == DateRange::Any {
                gettext("Date Modified")
            } else {
                filters.date.label()
            },
        );
        chip(
            &imp.size_button,
            filters.size != SizeRange::Any,
            filters.size.short_label(),
        );
    }

    // ── Status & placeholder pages ─────────────────────────────────────────

    fn update_content_page(&self) {
        let imp = self.imp();
        let directory = &imp.directory;
        let visible = imp.sort_model.get().map_or(0, |m| m.n_items());
        let page = &imp.status_page;

        let name = if imp.this_pc.get() {
            "this-pc"
        } else if imp.mounting.get() {
            "loading"
        } else if imp.recursive.get() {
            if visible > 0 {
                "view"
            } else if imp.searching.get() {
                "loading"
            } else {
                page.set_icon_name(Some("system-search-symbolic"));
                page.set_title(&gettext("No Results Found"));
                page.set_description(Some(&gettext(
                    "Nothing in this folder or its subfolders matches",
                )));
                "status"
            }
        } else if let Some(err) = directory.error().filter(|_| !directory.loading()) {
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
        } else if !imp.filter_state.query.borrow().is_empty()
            || imp.filter_state.filters.get().is_active()
        {
            page.set_icon_name(Some("system-search-symbolic"));
            page.set_title(&gettext("No Results Found"));
            page.set_description(Some(&gettext("Press Enter to search in subfolders too")));
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
        let visible = if imp.this_pc.get() {
            imp.this_pc_page.n_devices()
        } else {
            imp.sort_model.get().map_or(0, |m| m.n_items())
        };
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

fn default_view_mode() -> ViewMode {
    ViewMode::from_nick(&settings().string("default-view"))
}

/// A short user-facing name for a location ("Home", "Documents", "/").
pub fn display_name(file: &gio::File) -> String {
    if file.equal(&gio::File::for_path(glib::home_dir())) {
        return gettext("Home");
    }
    match file.uri_scheme().as_deref() {
        Some("trash") if file.parent().is_none() => return gettext("Trash"),
        Some("recent") if file.parent().is_none() => return gettext("Recent"),
        Some("computer") => return gettext("This PC"),
        _ => {}
    }
    file.basename()
        .map(|b| b.to_string_lossy().into_owned())
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| file.parse_name().into())
}

/// Where a search result is, relative to the folder searched: "Documents"
/// for a file directly in Documents, "Documents/Projects/src" deeper down.
pub fn relative_folder(root: &gio::File, file: &gio::File) -> String {
    let Some(parent) = file.parent() else {
        return String::new();
    };
    let root_name = display_name(root);
    match root.relative_path(&parent) {
        Some(path) => format!("{root_name}/{}", path.display()),
        None if parent.equal(root) => root_name,
        None => parent.parse_name().into(),
    }
}
