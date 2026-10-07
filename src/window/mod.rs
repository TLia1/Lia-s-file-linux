//! The main window: tab strip, navigation row, command bar, sidebar, tabs
//! and status bar.
//!
//! Each tab holds a `FolderPane`. The address bar, search entry, status bar
//! and window actions are shared and always act on the *active* pane: when
//! the selected tab changes, the window re-binds them (see
//! [`Window::on_active_pane_changed`]).

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gdk, gio, glib, glib::clone, subclass::prelude::*};

use crate::application::Application;
use crate::dialogs;
use crate::model::places;
use crate::services::clipboard::clipboard_service;
use crate::services::file_ops::{FileOpJob, JobState, Operation, queue};
use crate::services::templates::templates_menu;
use crate::util::settings;
use crate::widgets::address_bar::AddressBar;
use crate::widgets::dnd;
use crate::widgets::folder_pane::{self, FolderPane};
use crate::widgets::operations_popover::OperationsButton;
use crate::widgets::sidebar::Sidebar;

/// Sidebar width limits (pixels), matching the GSettings range.
const SIDEBAR_MIN_WIDTH: f64 = 180.0;
const SIDEBAR_MAX_WIDTH: f64 = 360.0;

/// Actions that arrive in later phases (properties in phase 6). They exist
/// now so that buttons, menus and shortcuts are wired, but stay disabled.
const PENDING_ACTIONS: [&str; 1] = ["properties"];

/// The connections made to the active pane, undone when it changes.
#[derive(Default)]
struct ActivePane {
    pane: glib::WeakRef<FolderPane>,
    handlers: Vec<glib::SignalHandlerId>,
    bindings: Vec<glib::Binding>,
}

mod imp {
    use super::*;
    use adw::subclass::prelude::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/lia/Files/ui/window.ui")]
    pub struct Window {
        #[template_child]
        pub(super) toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub(super) tab_view: TemplateChild<adw::TabView>,
        #[template_child]
        pub(super) tab_bar: TemplateChild<adw::TabBar>,
        #[template_child]
        pub(super) split_view: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub(super) sidebar: TemplateChild<Sidebar>,
        #[template_child]
        pub(super) sidebar_handle: TemplateChild<gtk::Box>,
        #[template_child]
        pub(super) address_bar: TemplateChild<AddressBar>,
        #[template_child]
        pub(super) search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub(super) item_count_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) selection_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) free_space_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) new_menu: TemplateChild<gio::Menu>,
        // Command bar buttons that change in the trash.
        #[template_child]
        pub(super) new_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub(super) restore_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) empty_trash_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) paste_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) rename_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) trash_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub(super) delete_button: TemplateChild<gtk::Button>,

        /// Handlers on app-wide services, removed when the window goes.
        pub(super) service_handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,

        pub(super) active: RefCell<ActivePane>,
        /// The tab whose context menu is open.
        pub(super) menu_page: RefCell<Option<adw::TabPage>>,
        /// Set once the session has been saved for this close, so that a
        /// later save with fewer tabs doesn't overwrite it.
        pub(super) session_saved: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "LiaWindow";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            AddressBar::ensure_type();
            Sidebar::ensure_type();
            OperationsButton::ensure_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Window {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.setup_tabs();
            obj.setup_shared_widgets();
            obj.setup_sidebar();
            obj.setup_actions();
            obj.setup_file_operations();
            obj.restore_state();
        }

        fn dispose(&self) {
            for (object, handler) in self.service_handlers.take() {
                object.disconnect(handler);
            }
        }
    }

    impl WidgetImpl for Window {}

    impl WindowImpl for Window {
        fn close_request(&self) -> glib::Propagation {
            let obj = self.obj();
            obj.save_state();
            if !self.session_saved.get() {
                obj.save_session();
            }
            self.parent_close_request()
        }
    }

    impl ApplicationWindowImpl for Window {}
    impl AdwApplicationWindowImpl for Window {}
}

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

impl Window {
    /// A window without tabs; call [`Window::open_tab`] next.
    pub fn new(app: &Application) -> Self {
        glib::Object::builder().property("application", app).build()
    }

    /// The pane of the selected tab.
    pub fn active_pane(&self) -> Option<FolderPane> {
        self.imp()
            .tab_view
            .selected_page()
            .and_downcast_ref::<adw::TabPage>()
            .map(|page| page.child())
            .and_then(|child| child.downcast().ok())
    }

    /// Opens `location` in a new tab placed after the current one.
    pub fn open_tab(&self, location: &gio::File, select: bool) -> FolderPane {
        let tab_view = &self.imp().tab_view;
        let pane = FolderPane::new();
        let page = match tab_view.selected_page() {
            Some(current) => tab_view.add_page(&pane, Some(&current)),
            None => tab_view.append(&pane),
        };
        bind_page(&page, &pane);
        pane.navigate_to(location);
        if select {
            tab_view.set_selected_page(&page);
        }
        pane
    }

    pub fn select_pane(&self, pane: &FolderPane) {
        let tab_view = &self.imp().tab_view;
        let page = tab_view.page(pane);
        tab_view.set_selected_page(&page);
    }

    /// Don't save the session when this window closes (see `app.quit`).
    pub fn skip_session_save(&self) {
        self.imp().session_saved.set(true);
    }

    pub fn show_toast(&self, message: &str) {
        let toast = adw::Toast::builder()
            .title(message)
            .use_markup(false)
            .timeout(3)
            .build();
        self.imp().toast_overlay.add_toast(toast);
    }

    /// Writes the open tabs to GSettings, for restoring on the next start.
    pub fn save_session(&self) {
        let tab_view = &self.imp().tab_view;
        let pages = tab_view.pages();
        let uris: Vec<String> = (0..pages.n_items())
            .filter_map(|i| pages.item(i).and_downcast::<adw::TabPage>())
            .filter_map(|page| page.child().downcast::<FolderPane>().ok())
            .filter_map(|pane| pane.location())
            .map(|location| location.uri().into())
            .collect();
        if uris.is_empty() {
            return;
        }
        let active = tab_view
            .selected_page()
            .map_or(0, |page| tab_view.page_position(&page));
        let settings = settings();
        let _ = settings.set_strv("session-tabs", uris);
        let _ = settings.set_int("session-active-tab", active);
        self.imp().session_saved.set(true);
    }

    // ── Tabs ───────────────────────────────────────────────────────────────

    fn setup_tabs(&self) {
        let tab_view = &self.imp().tab_view;

        tab_view.connect_selected_page_notify(clone!(
            #[weak(rename_to = window)]
            self,
            move |_| window.on_active_pane_changed()
        ));
        // The last tab was closed or dragged to another window.
        tab_view.connect_n_pages_notify(clone!(
            #[weak(rename_to = window)]
            self,
            move |tab_view| {
                if tab_view.n_pages() == 0 {
                    window.close();
                }
            }
        ));
        // Closing the last tab closes the window: remember it as the session.
        tab_view.connect_close_page(clone!(
            #[weak(rename_to = window)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |tab_view, _| {
                if tab_view.n_pages() == 1 {
                    window.save_session();
                }
                glib::Propagation::Proceed
            }
        ));
        // A tab dropped outside any window gets a window of its own.
        tab_view.connect_create_window(clone!(
            #[weak(rename_to = window)]
            self,
            #[upgrade_or]
            None,
            move |_| {
                let app = window.application().and_downcast::<Application>()?;
                let new_window = Self::new(&app);
                new_window.present();
                Some(new_window.imp().tab_view.clone())
            }
        ));
        tab_view.connect_setup_menu(clone!(
            #[weak(rename_to = window)]
            self,
            move |_, page| {
                window.imp().menu_page.replace(page.cloned());
                window.update_tab_actions();
            }
        ));
        tab_view.connect_n_pages_notify(clone!(
            #[weak(rename_to = window)]
            self,
            move |_| window.update_tab_actions()
        ));
    }

    /// The tab a tab action applies to: the one whose menu is open, or the
    /// selected one.
    fn action_page(&self) -> Option<adw::TabPage> {
        let imp = self.imp();
        imp.menu_page
            .borrow()
            .clone()
            .or_else(|| imp.tab_view.selected_page())
    }

    fn update_tab_actions(&self) {
        let several = self.imp().tab_view.n_pages() > 1;
        self.set_action_enabled("tab-close-others", several);
        self.set_action_enabled("tab-move-to-window", several);
    }

    fn move_tab_to_new_window(&self, page: &adw::TabPage) {
        let Some(app) = self.application().and_downcast::<Application>() else {
            return;
        };
        let window = Self::new(&app);
        self.imp()
            .tab_view
            .transfer_page(page, &window.imp().tab_view, 0);
        window.present();
    }

    // ── Active pane ────────────────────────────────────────────────────────

    /// Re-binds the shared widgets and actions to the selected tab's pane.
    fn on_active_pane_changed(&self) {
        let imp = self.imp();
        let previous = imp.active.take();
        if let Some(pane) = previous.pane.upgrade() {
            for handler in previous.handlers {
                pane.disconnect(handler);
            }
        }
        for binding in previous.bindings {
            binding.unbind();
        }

        let Some(pane) = self.active_pane() else {
            return;
        };
        let handlers = vec![
            pane.connect_location_notify(clone!(
                #[weak(rename_to = window)]
                self,
                move |_| window.on_location_changed()
            )),
            pane.connect_search_text_notify(clone!(
                #[weak(rename_to = window)]
                self,
                move |pane| window.sync_search_entry(pane)
            )),
            pane.connect_n_selected_notify(clone!(
                #[weak(rename_to = window)]
                self,
                move |_| window.update_selection_actions()
            )),
            pane.connect_writable_notify(clone!(
                #[weak(rename_to = window)]
                self,
                move |_| window.update_selection_actions()
            )),
            pane.connect_is_trash_notify(clone!(
                #[weak(rename_to = window)]
                self,
                move |_| window.on_location_changed()
            )),
            pane.connect_sort_changed(clone!(
                #[weak(rename_to = window)]
                self,
                move |_| window.sync_sort_actions()
            )),
        ];
        let bind = |property: &str, label: &gtk::Label| {
            pane.bind_property(property, label, "label")
                .sync_create()
                .build()
        };
        let bindings = vec![
            bind("item-count-text", &imp.item_count_label),
            bind("selection-text", &imp.selection_label),
            bind("free-space-text", &imp.free_space_label),
        ];
        imp.active.replace(ActivePane {
            pane: pane.downgrade(),
            handlers,
            bindings,
        });

        self.on_location_changed();
        self.sync_search_entry(&pane);
        self.update_selection_actions();
        self.sync_sort_actions();
        pane.grab_focus();
    }

    fn on_location_changed(&self) {
        let imp = self.imp();
        let Some(pane) = self.active_pane() else {
            return;
        };
        let location = pane.location();
        if let Some(location) = &location {
            let name = folder_pane::display_name(location);
            self.set_title(Some(&name));
            imp.address_bar.set_location(location);
            imp.search_entry
                .set_placeholder_text(Some(&gettextrs::gettext("Search {}").replace("{}", &name)));
        }
        imp.sidebar.set_location(location.as_ref());
        self.set_action_enabled("back", pane.can_go_back());
        self.set_action_enabled("forward", pane.can_go_forward());
        self.set_action_enabled("up", pane.can_go_up());

        // The trash has its own commands.
        let trash = pane.is_trash();
        imp.new_button.set_visible(!trash);
        imp.paste_button.set_visible(!trash);
        imp.rename_button.set_visible(!trash);
        imp.trash_button.set_visible(!trash);
        imp.restore_button.set_visible(trash);
        imp.empty_trash_button.set_visible(trash);
        imp.delete_button.set_visible(trash);
        self.update_selection_actions();
    }

    fn sync_search_entry(&self, pane: &FolderPane) {
        let entry = &self.imp().search_entry;
        let text = pane.search_text();
        if entry.text() != text {
            entry.set_text(&text);
        }
    }

    fn update_selection_actions(&self) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let selected = pane.selected_items();
        let any = !selected.is_empty();
        let any_folder = selected.iter().any(|item| item.is_dir());
        let all_folders = any && selected.iter().all(|item| item.is_dir());
        self.set_action_enabled("open", any);
        self.set_action_enabled("open-with", selected.iter().any(|item| !item.is_dir()));
        self.set_action_enabled("open-in-new-tab", any_folder);
        self.set_action_enabled("pin", all_folders);

        // File operations depend on the folder being writable.
        let writable = pane.writable();
        let trash = pane.is_trash();
        let removable = any && (writable || trash);
        self.set_action_enabled("copy", any);
        self.set_action_enabled("cut", removable);
        self.set_action_enabled("trash", removable);
        self.set_action_enabled("delete", removable);
        self.set_action_enabled("rename", any && writable);
        self.set_action_enabled("duplicate", any && writable);
        self.set_action_enabled(
            "create-link",
            any && writable && selected.iter().all(|i| i.target_file().is_native()),
        );
        self.set_action_enabled("paste", writable && clipboard_service().has_files());
        for name in ["new-folder", "new-file", "new-from-template"] {
            self.set_action_enabled(name, writable);
        }
        self.set_action_enabled(
            "restore",
            trash && any && selected.iter().all(|i| i.trash_orig_path().is_some()),
        );
    }

    fn sync_sort_actions(&self) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let (column, descending) = pane.sort_state();
        let order = if descending {
            "descending"
        } else {
            "ascending"
        };
        // `set_state` doesn't emit `change-state`, so this doesn't re-sort.
        for (name, state) in [("sort-by", column.as_str()), ("sort-order", order)] {
            if let Some(action) = self.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                action.set_state(&state.to_variant());
            }
        }
    }

    // ── Shared widgets ─────────────────────────────────────────────────────

    fn setup_shared_widgets(&self) {
        let imp = self.imp();

        imp.address_bar.connect_navigate(clone!(
            #[weak(rename_to = window)]
            self,
            move |_, file| {
                if let Some(pane) = window.active_pane() {
                    pane.navigate_to(file);
                    pane.grab_focus();
                }
            }
        ));

        let search = &imp.search_entry;
        search.connect_search_changed(clone!(
            #[weak(rename_to = window)]
            self,
            move |entry| {
                if let Some(pane) = window.active_pane() {
                    pane.set_search_text(entry.text().to_string());
                }
            }
        ));
        search.connect_stop_search(clone!(
            #[weak(rename_to = window)]
            self,
            move |entry| {
                entry.set_text("");
                if let Some(pane) = window.active_pane() {
                    pane.grab_focus();
                }
            }
        ));
        search.connect_activate(clone!(
            #[weak(rename_to = window)]
            self,
            move |_| {
                if let Some(pane) = window.active_pane() {
                    pane.grab_focus();
                }
            }
        ));
    }

    // ── Sidebar ────────────────────────────────────────────────────────────

    fn setup_sidebar(&self) {
        let imp = self.imp();

        imp.sidebar.connect_drop_files(clone!(
            #[weak(rename_to = window)]
            self,
            move |_, dest, files, action| window.drop_files(dest, &files, action)
        ));
        imp.sidebar.connect_open_location(clone!(
            #[weak(rename_to = window)]
            self,
            move |_, location, new_tab| {
                if new_tab {
                    window.open_tab(location, false);
                } else if let Some(pane) = window.active_pane() {
                    pane.navigate_to(location);
                    pane.grab_focus();
                }
                let split_view = &window.imp().split_view;
                if split_view.is_collapsed() {
                    split_view.set_show_sidebar(false);
                }
            }
        ));

        // Drag the handle on the sidebar's edge to resize it.
        let drag = gtk::GestureDrag::new();
        let start_width = std::rc::Rc::new(Cell::new(0.0));
        drag.connect_drag_begin(clone!(
            #[weak(rename_to = window)]
            self,
            #[strong]
            start_width,
            move |_, _, _| start_width.set(window.imp().split_view.max_sidebar_width())
        ));
        drag.connect_drag_update(clone!(
            #[weak(rename_to = window)]
            self,
            #[strong]
            start_width,
            move |_, dx, _| {
                let dx = if window.direction() == gtk::TextDirection::Rtl {
                    -dx
                } else {
                    dx
                };
                window.set_sidebar_width(start_width.get() + dx);
            }
        ));
        drag.connect_drag_end(clone!(
            #[weak(rename_to = window)]
            self,
            move |_, _, _| {
                let width = window.imp().split_view.max_sidebar_width();
                let _ = settings().set_int("sidebar-width", width.round() as i32);
            }
        ));
        imp.sidebar_handle.add_controller(drag);
        imp.sidebar_handle.set_cursor_from_name(Some("col-resize"));
        imp.split_view
            .bind_property("collapsed", &*imp.sidebar_handle, "visible")
            .invert_boolean()
            .sync_create()
            .build();
    }

    fn set_sidebar_width(&self, width: f64) {
        let split_view = &self.imp().split_view;
        let width = width.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        split_view.set_min_sidebar_width(width);
        split_view.set_max_sidebar_width(width);
    }

    // ── Actions ────────────────────────────────────────────────────────────

    fn setup_actions(&self) {
        let pane_action = |name: &str, f: fn(&FolderPane)| {
            gio::ActionEntry::builder(name)
                .activate(move |window: &Self, _, _| {
                    if let Some(pane) = window.active_pane() {
                        f(&pane);
                    }
                })
                .build()
        };
        let window_action = |name: &str, f: fn(&Self)| {
            gio::ActionEntry::builder(name)
                .activate(move |window: &Self, _, _| f(window))
                .build()
        };
        let tab_action = |name: &str, f: fn(&Self, &adw::TabPage)| {
            gio::ActionEntry::builder(name)
                .activate(move |window: &Self, _, _| {
                    if let Some(page) = window.action_page() {
                        f(window, &page);
                    }
                    window.imp().menu_page.replace(None);
                })
                .build()
        };
        self.add_action_entries([
            // Navigation
            pane_action("back", FolderPane::go_back),
            pane_action("forward", FolderPane::go_forward),
            pane_action("up", FolderPane::go_up),
            pane_action("reload", FolderPane::reload),
            pane_action("home", |pane| {
                pane.navigate_to(&gio::File::for_path(glib::home_dir()))
            }),
            window_action("focus-location", |window| {
                window.imp().address_bar.start_editing()
            }),
            window_action("focus-search", |window| {
                let entry = &window.imp().search_entry;
                entry.grab_focus();
                entry.select_region(0, -1);
            }),
            // Opening
            pane_action("open", |pane| pane.open_selection(false)),
            pane_action("open-with", |pane| pane.open_selection(true)),
            window_action("open-in-new-tab", Self::open_selection_in_new_tabs),
            // Selection
            pane_action("select-all", FolderPane::select_all),
            pane_action("invert-selection", FolderPane::invert_selection),
            pane_action("copy-path", FolderPane::copy_path),
            // File operations
            window_action("cut", |window| window.copy_selection(true)),
            window_action("copy", |window| window.copy_selection(false)),
            window_action("paste", Self::paste),
            pane_action("rename", FolderPane::start_rename),
            window_action("trash", Self::trash_selection),
            window_action("delete", Self::delete_selection),
            pane_action("duplicate", |pane| {
                let files = pane.selected_files();
                pane.run_operation(Operation::Duplicate { files }, false);
            }),
            pane_action("create-link", |pane| {
                let Some(location) = pane.location() else {
                    return;
                };
                let pairs = pane
                    .selected_files()
                    .into_iter()
                    .filter_map(|file| {
                        let name = file.basename()?.to_string_lossy().into_owned();
                        // Translators: name of a new symbolic link
                        let link = gettext("Link to {}").replace("{}", &name);
                        Some((file, location.child(link)))
                    })
                    .collect();
                pane.run_operation(Operation::Link { pairs }, false);
            }),
            pane_action("new-folder", |pane| {
                if let Some(parent) = pane.location() {
                    let name = gettext("New Folder");
                    pane.run_operation(Operation::CreateFolder { parent, name }, true);
                }
            }),
            pane_action("new-file", |pane| {
                if let Some(parent) = pane.location() {
                    let name = gettext("New File");
                    let template = None;
                    pane.run_operation(
                        Operation::CreateFile {
                            parent,
                            name,
                            template,
                        },
                        true,
                    );
                }
            }),
            gio::ActionEntry::builder("new-from-template")
                .parameter_type(Some(glib::VariantTy::STRING))
                .activate(|window: &Self, _, parameter| {
                    let (Some(pane), Some(uri)) = (
                        window.active_pane(),
                        parameter.and_then(|p| p.get::<String>()),
                    ) else {
                        return;
                    };
                    let template = gio::File::for_uri(&uri);
                    let (Some(parent), Some(name)) = (pane.location(), template.basename()) else {
                        return;
                    };
                    let name = name.to_string_lossy().into_owned();
                    let template = Some(template);
                    pane.run_operation(
                        Operation::CreateFile {
                            parent,
                            name,
                            template,
                        },
                        true,
                    );
                })
                .build(),
            pane_action("restore", |pane| {
                let files = pane
                    .selected_items()
                    .iter()
                    .map(|i| i.file().clone())
                    .collect();
                pane.run_operation(Operation::Restore { files }, false);
            }),
            window_action("empty-trash", Self::empty_trash),
            window_action("pin", |window| {
                let folders: Vec<gio::File> = window
                    .active_pane()
                    .map(|pane| pane.selected_items())
                    .unwrap_or_default()
                    .iter()
                    .filter_map(FolderPane::folder_target)
                    .collect();
                window.imp().sidebar.pin(&folders);
            }),
            // View
            pane_action("zoom-in", |pane| pane.zoom(1)),
            pane_action("zoom-out", |pane| pane.zoom(-1)),
            pane_action("zoom-reset", FolderPane::zoom_reset),
            // Tabs
            window_action("new-tab", |window| {
                let location = window
                    .active_pane()
                    .and_then(|pane| pane.location())
                    .unwrap_or_else(|| gio::File::for_path(glib::home_dir()));
                window.open_tab(&location, true);
            }),
            window_action("close-tab", |window| {
                let tab_view = &window.imp().tab_view;
                if let Some(page) = tab_view.selected_page() {
                    tab_view.close_page(&page);
                }
            }),
            tab_action("tab-close", |window, page| {
                window.imp().tab_view.close_page(page)
            }),
            tab_action("tab-close-others", |window, page| {
                window.imp().tab_view.close_other_pages(page)
            }),
            tab_action("tab-duplicate", |window, page| {
                let location = page
                    .child()
                    .downcast::<FolderPane>()
                    .ok()
                    .and_then(|pane| pane.location());
                if let Some(location) = location {
                    window.imp().tab_view.set_selected_page(page);
                    window.open_tab(&location, true);
                }
            }),
            tab_action("tab-move-to-window", Self::move_tab_to_new_window),
        ]);

        // Implemented in later phases.
        for name in PENDING_ACTIONS {
            let action = gio::SimpleAction::new(name, None);
            action.set_enabled(false);
            self.add_action(&action);
        }

        // Sorting: radio-style string states, synced from the active pane.
        let sort_by = gio::SimpleAction::new_stateful(
            "sort-by",
            Some(glib::VariantTy::STRING),
            &"name".to_variant(),
        );
        sort_by.connect_change_state(clone!(
            #[weak(rename_to = window)]
            self,
            move |action, state| {
                let Some(state) = state else { return };
                action.set_state(state);
                window.apply_sort();
            }
        ));
        let sort_order = gio::SimpleAction::new_stateful(
            "sort-order",
            Some(glib::VariantTy::STRING),
            &"ascending".to_variant(),
        );
        sort_order.connect_change_state(clone!(
            #[weak(rename_to = window)]
            self,
            move |action, state| {
                let Some(state) = state else { return };
                action.set_state(state);
                window.apply_sort();
            }
        ));
        self.add_action(&sort_by);
        self.add_action(&sort_order);

        self.add_action(&gio::PropertyAction::new(
            "toggle-sidebar",
            &*self.imp().split_view,
            "show-sidebar",
        ));

        // Stateful actions backed directly by GSettings.
        let settings = settings();
        for key in ["show-hidden", "sort-folders-first", "default-view"] {
            self.add_action(&settings.create_action(key));
        }
    }

    /// Applies the sort action states to the active pane.
    fn apply_sort(&self) {
        let state = |name: &str| {
            self.action_state(name)
                .and_then(|v| v.get::<String>())
                .unwrap_or_default()
        };
        let column = state("sort-by");
        let descending = state("sort-order") == "descending";
        if let Some(pane) = self.active_pane()
            && pane.sort_state() != (column.clone(), descending)
        {
            pane.set_sort(&column, descending);
        }
    }

    // ── File operations ────────────────────────────────────────────────────

    fn setup_file_operations(&self) {
        let imp = self.imp();
        imp.new_menu.append_section(None, &templates_menu());

        let mut handlers: Vec<(glib::Object, glib::SignalHandlerId)> = Vec::new();
        let clipboard = clipboard_service();
        let handler = clipboard.connect_changed(clone!(
            #[weak(rename_to = window)]
            self,
            move |_| window.update_selection_actions()
        ));
        handlers.push((clipboard.upcast(), handler));
        let queue = queue();
        let handler = queue.connect_job_finished(clone!(
            #[weak(rename_to = window)]
            self,
            move |_, job| window.on_job_finished(job)
        ));
        handlers.push((queue.upcast(), handler));
        imp.service_handlers.replace(handlers);

        // Undo/redo in the bubble phase: text fields handle Ctrl+Z first.
        let shortcuts = gtk::ShortcutController::new();
        shortcuts.set_propagation_phase(gtk::PropagationPhase::Bubble);
        for (trigger, action) in [
            ("<Control>z", "app.undo"),
            ("<Control>y", "app.redo"),
            ("<Control><Shift>z", "app.redo"),
        ] {
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        self.add_controller(shortcuts);

        // Files dropped on a tab go to its folder.
        let tab_bar = &imp.tab_bar;
        tab_bar.setup_extra_drop_target(dnd::ACTIONS, &[gdk::FileList::static_type()]);
        tab_bar.connect_extra_drag_value(|tab_bar, page, value| {
            let files = value
                .and_then(|v| v.get::<gdk::FileList>().ok())
                .map(|list| list.files())
                .unwrap_or_default();
            tab_drop_action(tab_bar, page, &files).unwrap_or(gdk::DragAction::COPY)
        });
        tab_bar.connect_extra_drag_drop(|tab_bar, page, value| {
            let Ok(list) = value.get::<gdk::FileList>() else {
                return false;
            };
            let files = list.files();
            let (Some(action), Some(pane)) = (
                tab_drop_action(tab_bar, page, &files),
                page.child().downcast::<FolderPane>().ok(),
            ) else {
                return false;
            };
            if let Some(dest) = pane.location() {
                pane.drop_files(&dest, &files, action);
            }
            true
        });
    }

    /// Puts the selection on the clipboard.
    fn copy_selection(&self, cut: bool) {
        let files = self
            .active_pane()
            .map(|pane| pane.selected_files())
            .unwrap_or_default();
        if !files.is_empty() {
            clipboard_service().set(&files, cut);
        }
    }

    /// Pastes files from the clipboard into the current folder.
    fn paste(&self) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let Some(dest) = pane.location() else {
            return;
        };
        glib::spawn_future_local(async move {
            let clipboard = clipboard_service();
            let Some((cut, files)) = clipboard.read().await else {
                return;
            };
            if files.is_empty() {
                return;
            }
            let operation = if cut {
                Operation::move_into(&files, &dest)
            } else {
                Operation::copy_into(&files, &dest)
            };
            pane.run_operation(operation, false);
            if cut {
                // The files won't be where the clipboard says anymore.
                clipboard.clear_cut();
            }
        });
    }

    /// Delete: to the trash, or permanently when already in the trash.
    fn trash_selection(&self) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        if pane.is_trash() {
            self.delete_selection();
            return;
        }
        let files = pane.selected_files();
        if !files.is_empty() {
            pane.run_operation(Operation::Trash { files }, false);
        }
    }

    /// Shift+Delete: permanently, after confirmation.
    fn delete_selection(&self) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        let items = pane.selected_items();
        let Some(first) = items.first() else {
            return;
        };
        let name = first.display_name().to_string();
        let files: Vec<gio::File> = if pane.is_trash() {
            items.iter().map(|i| i.file().clone()).collect()
        } else {
            pane.selected_files()
        };
        glib::spawn_future_local(clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                if dialogs::file_ops::confirm_delete(&window, files.len(), &name).await {
                    pane.run_operation(Operation::Delete { files }, false);
                }
            }
        ));
    }

    fn empty_trash(&self) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = window)]
            self,
            async move {
                if dialogs::file_ops::confirm_empty_trash(&window).await {
                    queue().submit(Operation::EmptyTrash, Some(&window));
                }
            }
        ));
    }

    /// Files dropped on the sidebar.
    fn drop_files(&self, dest: &gio::File, files: &[gio::File], action: gdk::DragAction) {
        if let Some(pane) = self.active_pane() {
            pane.drop_files(dest, files, action);
        }
    }

    /// Announces finished jobs started from this window.
    fn on_job_finished(&self, job: &FileOpJob) {
        let origin = job.origin().and_then(|widget| widget.root());
        if origin.as_ref() != Some(self.upcast_ref::<gtk::Root>()) {
            return;
        }
        let toast = match job.state() {
            JobState::Done => {
                let Some(message) = job.done_message() else {
                    return;
                };
                let toast = adw::Toast::builder()
                    .title(message)
                    .use_markup(false)
                    .timeout(5)
                    .build();
                if job.is_undoable() {
                    toast.set_button_label(Some(&gettext("_Undo")));
                    toast.set_action_name(Some("app.undo"));
                }
                toast
            }
            JobState::Failed => adw::Toast::builder()
                .title(gettext("Failed: {}").replace("{}", &job.error()))
                .use_markup(false)
                .timeout(5)
                .build(),
            _ => return,
        };
        self.imp().toast_overlay.add_toast(toast);
    }

    fn open_selection_in_new_tabs(&self) {
        let Some(pane) = self.active_pane() else {
            return;
        };
        for folder in pane
            .selected_items()
            .iter()
            .filter_map(FolderPane::folder_target)
        {
            self.open_tab(&folder, false);
        }
    }

    fn set_action_enabled(&self, name: &str, enabled: bool) {
        if let Some(action) = self.lookup_action(name).and_downcast::<gio::SimpleAction>() {
            action.set_enabled(enabled);
        }
    }

    // ── Window state ───────────────────────────────────────────────────────

    fn restore_state(&self) {
        let settings = settings();
        self.set_default_size(settings.int("window-width"), settings.int("window-height"));
        if settings.boolean("is-maximized") {
            self.maximize();
        }
        self.set_sidebar_width(settings.int("sidebar-width") as f64);
    }

    fn save_state(&self) {
        let settings = settings();
        let (width, height) = self.default_size();
        let _ = settings.set_int("window-width", width);
        let _ = settings.set_int("window-height", height);
        let _ = settings.set_boolean("is-maximized", self.is_maximized());
    }
}

/// The action for files dragged over a tab (into its folder), or `None`
/// if they can't be dropped there.
fn tab_drop_action(
    tab_bar: &adw::TabBar,
    page: &adw::TabPage,
    files: &[gio::File],
) -> Option<gdk::DragAction> {
    let pane = page.child().downcast::<FolderPane>().ok()?;
    let dest = pane.location()?;
    if !pane.writable() && !pane.is_trash() {
        return None;
    }
    let first = files.first()?;
    let action = dnd::action_for(files, &dest, dnd::current_modifiers(tab_bar), || {
        dnd::same_device(first, &dest)
    });
    (!action.is_empty()).then_some(action)
}

/// Keeps a tab's title, icon, tooltip and spinner in sync with its pane.
/// Only the page and pane are referenced, so this survives the tab being
/// dragged to another window.
fn bind_page(page: &adw::TabPage, pane: &FolderPane) {
    pane.bind_property("title", page, "title")
        .sync_create()
        .build();
    pane.bind_property("loading", page, "loading")
        .sync_create()
        .build();
    let page = page.downgrade();
    pane.connect_location_notify(move |pane| {
        let (Some(page), Some(location)) = (page.upgrade(), pane.location()) else {
            return;
        };
        page.set_tooltip(&glib::markup_escape_text(&location.parse_name()));
        page.set_icon(Some(&gio::ThemedIcon::new(places::icon_name_for(
            &location,
        ))));
    });
}
