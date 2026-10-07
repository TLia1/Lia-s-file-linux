//! `FolderPane`: everything shown for one folder (one tab, later on):
//! navigation row, file view, empty/error states and status bar.
//!
//! Model chain:
//! `DirectoryModel → FilterListModel (hidden, search) → SortListModel
//!  (folders first + column sorter) → MultiSelection → FileView`

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;

use gettextrs::{gettext, ngettext};
use gtk::{gio, glib, glib::clone, prelude::*, subclass::prelude::*};

use crate::model::{DirectoryModel, FileItem, History, sorting};
use crate::util::{format, settings};
use crate::widgets::address_bar::AddressBar;
use crate::widgets::file_view::{DEFAULT_ICON_SIZE, FileView, ICON_SIZES, ViewMode};

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
        pub(super) address_bar: TemplateChild<AddressBar>,
        #[template_child]
        pub(super) search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub(super) content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(super) file_view: TemplateChild<FileView>,
        #[template_child]
        pub(super) status_page: TemplateChild<adw::StatusPage>,
        #[template_child]
        pub(super) item_count_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) selection_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) free_space_label: TemplateChild<gtk::Label>,

        /// The folder being shown.
        #[property(get, nullable)]
        pub(super) location: RefCell<Option<gio::File>>,

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
        pub(super) settings_handlers: RefCell<Vec<glib::SignalHandlerId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FolderPane {
        const NAME: &'static str = "LiaFolderPane";
        type Type = super::FolderPane;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            AddressBar::ensure_type();
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
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }

        fn dispose(&self) {
            let settings = settings();
            for handler in self.settings_handlers.take() {
                settings.disconnect(handler);
            }
            if let Some(task) = self.free_space_task.take() {
                task.abort();
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
            }
        ));
        selection.connect_selection_changed(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_, _, _| pane.update_status()
        ));

        imp.address_bar.connect_navigate(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_, file| {
                pane.navigate_to(file);
                pane.grab_focus();
            }
        ));
        imp.file_view.connect_item_activated(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_, item| pane.open_item(item, false)
        ));

        imp.search_entry.connect_search_changed(clone!(
            #[weak(rename_to = pane)]
            self,
            move |entry| pane.set_search_query(&entry.text())
        ));
        imp.search_entry.connect_stop_search(clone!(
            #[weak(rename_to = pane)]
            self,
            move |entry| {
                entry.set_text("");
                pane.grab_focus();
            }
        ));
        imp.search_entry.connect_activate(clone!(
            #[weak(rename_to = pane)]
            self,
            move |_| {
                pane.grab_focus();
            }
        ));

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
        if !imp.search_entry.text().is_empty() {
            imp.search_entry.set_text("");
        }
        imp.search_entry.set_placeholder_text(Some(
            &gettext("Search {}").replace("{}", &display_name(location)),
        ));

        imp.address_bar.set_location(location);
        imp.directory.load(location);
        self.update_free_space();
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

    // ── View options ───────────────────────────────────────────────────────

    pub fn focus_location(&self) {
        self.imp().address_bar.start_editing();
    }

    pub fn focus_search(&self) {
        let entry = &self.imp().search_entry;
        entry.grab_focus();
        entry.select_region(0, -1);
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

    fn set_search_query(&self, text: &str) {
        let imp = self.imp();
        let query = text.trim().to_lowercase();
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
            let (icon, title) = if err.matches(gio::IOErrorEnum::PermissionDenied) {
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
            page.set_description(Some(&glib::markup_escape_text(err.message())));
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
        imp.item_count_label.set_label(&format::item_count(visible));

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
        imp.selection_label.set_label(&label);
    }

    fn update_free_space(&self) {
        let imp = self.imp();
        if let Some(task) = imp.free_space_task.take() {
            task.abort();
        }
        imp.free_space_label.set_label("");
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
                    pane.imp()
                        .free_space_label
                        .set_label(&gettext("{} free").replace("{}", &format::size(free)));
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
