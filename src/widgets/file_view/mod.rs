//! `FileView`: shows a selection model of `FileItem`s either as a details
//! table (`gtk::ColumnView`) or as an icon grid (`gtk::GridView`). Both are
//! virtualized, so only visible rows are instantiated.

use std::cell::{Cell, OnceCell, RefCell};
use std::sync::OnceLock;

use gettextrs::gettext;
use gtk::{gdk, gio, glib, glib::clone, prelude::*, subclass::prelude::*};

use crate::model::{FileItem, sorting};
use crate::util::format;

/// Grid icon sizes cycled through by zooming.
pub const ICON_SIZES: [i32; 3] = [48, 96, 160];
pub const DEFAULT_ICON_SIZE: i32 = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Details,
    Grid,
}

impl ViewMode {
    pub fn from_nick(nick: &str) -> Self {
        match nick {
            "grid" => Self::Grid,
            _ => Self::Details,
        }
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FileView {
        pub(super) stack: OnceCell<gtk::Stack>,
        pub(super) column_view: OnceCell<gtk::ColumnView>,
        pub(super) grid_view: OnceCell<gtk::GridView>,
        pub(super) name_column: OnceCell<gtk::ColumnViewColumn>,
        pub(super) selection: RefCell<Option<gtk::MultiSelection>>,
        pub(super) context_menu: OnceCell<gtk::PopoverMenu>,
        pub(super) icon_size: Cell<i32>,
        pub(super) mode: Cell<ViewMode>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FileView {
        const NAME: &'static str = "LiaFileView";
        type Type = super::FileView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("lia-file-view");
        }
    }

    impl ObjectImpl for FileView {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    // Emitted on double-click / Enter.
                    glib::subclass::Signal::builder("item-activated")
                        .param_types([FileItem::static_type()])
                        .build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.icon_size.set(DEFAULT_ICON_SIZE);
            self.obj().build();
        }

        fn dispose(&self) {
            if let Some(menu) = self.context_menu.get() {
                menu.unparent();
            }
            if let Some(stack) = self.stack.get() {
                stack.unparent();
            }
        }
    }

    impl WidgetImpl for FileView {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            if let Some(menu) = self.context_menu.get() {
                menu.present();
            }
        }

        fn grab_focus(&self) -> bool {
            match self.mode.get() {
                ViewMode::Details => self.column_view.get().unwrap().grab_focus(),
                ViewMode::Grid => self.grid_view.get().unwrap().grab_focus(),
            }
        }
    }
}

glib::wrapper! {
    pub struct FileView(ObjectSubclass<imp::FileView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for FileView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl FileView {
    fn build(&self) {
        let imp = self.imp();

        let column_view = gtk::ColumnView::builder()
            .reorderable(true)
            .enable_rubberband(true)
            .show_row_separators(false)
            .show_column_separators(false)
            .css_classes(["details-view"])
            .build();
        column_view.connect_activate(clone!(
            #[weak(rename_to = view)]
            self,
            move |_, position| view.activate_position(position)
        ));
        self.add_columns(&column_view);

        let grid_view = gtk::GridView::builder()
            .enable_rubberband(true)
            .max_columns(64)
            .css_classes(["grid-view"])
            .build();
        grid_view.set_factory(Some(&self.grid_factory()));
        grid_view.connect_activate(clone!(
            #[weak(rename_to = view)]
            self,
            move |_, position| view.activate_position(position)
        ));

        let details_scroller = gtk::ScrolledWindow::builder()
            .child(&column_view)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .build();
        let grid_scroller = gtk::ScrolledWindow::builder()
            .child(&grid_view)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        self.add_background_menu_gesture(&details_scroller);
        self.add_background_menu_gesture(&grid_scroller);
        self.add_zoom_controller(&grid_scroller);

        let stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(150)
            .build();
        stack.add_named(&details_scroller, Some("details"));
        stack.add_named(&grid_scroller, Some("grid"));
        stack.set_parent(self);

        // File shortcuts are local to the view so that Ctrl+C, Delete…
        // keep working normally in the address bar and search entry.
        let shortcuts = gtk::ShortcutController::new();
        for (trigger, action) in VIEW_SHORTCUTS {
            shortcuts.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        self.add_controller(shortcuts);

        let menu = gtk::PopoverMenu::builder()
            .has_arrow(false)
            .halign(gtk::Align::Start)
            .build();
        menu.set_parent(self);

        imp.stack.set(stack).unwrap();
        imp.column_view.set(column_view).unwrap();
        imp.grid_view.set(grid_view).unwrap();
        imp.context_menu.set(menu).unwrap();
    }

    // ── Public API ─────────────────────────────────────────────────────────

    pub fn set_model(&self, model: &gtk::MultiSelection) {
        let imp = self.imp();
        imp.column_view.get().unwrap().set_model(Some(model));
        imp.grid_view.get().unwrap().set_model(Some(model));
        imp.selection.replace(Some(model.clone()));
    }

    /// The column-header sorter of the details view. The grid view shares it,
    /// so both views always use the same order.
    pub fn sorter(&self) -> Option<gtk::Sorter> {
        self.imp().column_view.get().unwrap().sorter()
    }

    pub fn mode(&self) -> ViewMode {
        self.imp().mode.get()
    }

    pub fn set_mode(&self, mode: ViewMode) {
        let imp = self.imp();
        if imp.mode.get() == mode {
            return;
        }
        let had_focus = self.focus_child().is_some();
        imp.mode.set(mode);
        let name = match mode {
            ViewMode::Details => "details",
            ViewMode::Grid => "grid",
        };
        imp.stack.get().unwrap().set_visible_child_name(name);
        if had_focus {
            self.grab_focus();
        }
    }

    /// The id of the sorted column (`name`, `modified`, `type`, `size`) and
    /// whether the order is descending.
    pub fn sort_state(&self) -> (String, bool) {
        let sorter = self.column_sorter();
        let id = sorter
            .primary_sort_column()
            .and_then(|c| c.id())
            .map_or_else(|| "name".to_owned(), String::from);
        let descending = sorter.primary_sort_order() == gtk::SortType::Descending;
        (id, descending)
    }

    pub fn set_sort(&self, id: &str, descending: bool) {
        let column_view = self.imp().column_view.get().unwrap();
        let columns = column_view.columns();
        let column = (0..columns.n_items())
            .filter_map(|i| columns.item(i).and_downcast::<gtk::ColumnViewColumn>())
            .find(|c| c.id().as_deref() == Some(id));
        if let Some(column) = column {
            let order = if descending {
                gtk::SortType::Descending
            } else {
                gtk::SortType::Ascending
            };
            column_view.sort_by_column(Some(&column), order);
        }
    }

    /// Called when the sort column or order changes (menu or header click).
    pub fn connect_sort_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        let view = self.downgrade();
        self.column_sorter().connect_changed(move |_, _| {
            if let Some(view) = view.upgrade() {
                f(&view);
            }
        })
    }

    fn column_sorter(&self) -> gtk::ColumnViewSorter {
        self.sorter().and_downcast().unwrap()
    }

    pub fn icon_size(&self) -> i32 {
        self.imp().icon_size.get()
    }

    pub fn set_icon_size(&self, size: i32) {
        let imp = self.imp();
        if imp.icon_size.get() == size {
            return;
        }
        imp.icon_size.set(size);
        // Recreate the visible children with the new size.
        imp.grid_view
            .get()
            .unwrap()
            .set_factory(Some(&self.grid_factory()));
    }

    pub fn scroll_to(&self, position: u32, select: bool) {
        let imp = self.imp();
        let mut flags = gtk::ListScrollFlags::FOCUS;
        if select {
            flags |= gtk::ListScrollFlags::SELECT;
        }
        match imp.mode.get() {
            ViewMode::Details => imp
                .column_view
                .get()
                .unwrap()
                .scroll_to(position, None, flags, None),
            ViewMode::Grid => imp
                .grid_view
                .get()
                .unwrap()
                .scroll_to(position, flags, None),
        }
    }

    /// Scrolls to the first item without moving focus or selection.
    pub fn scroll_to_top(&self) {
        let imp = self.imp();
        let adjustment = match imp.mode.get() {
            ViewMode::Details => imp.column_view.get().unwrap().vadjustment(),
            ViewMode::Grid => imp.grid_view.get().unwrap().vadjustment(),
        };
        if let Some(adjustment) = adjustment {
            adjustment.set_value(adjustment.lower());
        }
    }

    pub fn connect_item_activated<F: Fn(&Self, &FileItem) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "item-activated",
            false,
            glib::closure_local!(move |view: &Self, item: &FileItem| f(view, item)),
        )
    }

    // ── Internals ──────────────────────────────────────────────────────────

    fn activate_position(&self, position: u32) {
        let item = self
            .imp()
            .selection
            .borrow()
            .as_ref()
            .and_then(|model| model.item(position))
            .and_downcast::<FileItem>();
        if let Some(item) = item {
            self.emit_by_name::<()>("item-activated", &[&item]);
        }
    }

    fn add_columns(&self, column_view: &gtk::ColumnView) {
        let name = gtk::ColumnViewColumn::builder()
            .id("name")
            .title(gettext("Name"))
            .factory(&self.name_factory())
            .sorter(&sorting::name_sorter())
            .expand(true)
            .resizable(true)
            .build();
        let modified = gtk::ColumnViewColumn::builder()
            .id("modified")
            .title(gettext("Date modified"))
            .factory(&self.text_factory(0.0, |item| format::date(item.modified())))
            .sorter(&sorting::modified_sorter())
            .fixed_width(170)
            .resizable(true)
            .build();
        let kind = gtk::ColumnViewColumn::builder()
            .id("type")
            .title(gettext("Type"))
            .factory(&self.text_factory(0.0, |item| item.type_description()))
            .sorter(&sorting::type_sorter())
            .fixed_width(160)
            .resizable(true)
            .build();
        let size = gtk::ColumnViewColumn::builder()
            .id("size")
            .title(gettext("Size"))
            .factory(&self.text_factory(1.0, |item| {
                if item.is_dir() {
                    String::new()
                } else {
                    format::size(item.size())
                }
            }))
            .sorter(&sorting::size_sorter())
            .fixed_width(100)
            .resizable(true)
            .build();

        for column in [&name, &modified, &kind, &size] {
            column_view.append_column(column);
        }
        column_view.sort_by_column(Some(&name), gtk::SortType::Ascending);
        self.imp().name_column.set(name).unwrap();
    }

    fn name_factory(&self) -> gtk::SignalListItemFactory {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(clone!(
            #[weak(rename_to = view)]
            self,
            move |_, list_item| {
                let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
                let image = gtk::Image::builder().pixel_size(20).build();
                let label = gtk::Label::builder()
                    .xalign(0.0)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .hexpand(true)
                    .build();
                let row = gtk::Box::builder().spacing(10).build();
                row.append(&image);
                row.append(&label);
                view.add_item_menu_gesture(&row, list_item);
                list_item.set_child(Some(&row));
            }
        ));
        factory.connect_bind(|_, list_item| {
            let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
            let (Some(item), Some(row)) = (
                list_item.item().and_downcast::<FileItem>(),
                list_item.child(),
            ) else {
                return;
            };
            let image = row.first_child().and_downcast::<gtk::Image>().unwrap();
            let label = row.last_child().and_downcast::<gtk::Label>().unwrap();
            image.set_from_gicon(&item.icon().unwrap_or_else(fallback_icon));
            label.set_label(&item.display_name());
            row.set_opacity(if item.is_hidden() { 0.6 } else { 1.0 });
        });
        factory
    }

    fn text_factory(
        &self,
        xalign: f32,
        text: impl Fn(&FileItem) -> String + 'static,
    ) -> gtk::SignalListItemFactory {
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(clone!(
            #[weak(rename_to = view)]
            self,
            move |_, list_item| {
                let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
                let label = gtk::Label::builder()
                    .xalign(xalign)
                    .ellipsize(gtk::pango::EllipsizeMode::End)
                    .css_classes(["secondary", "numeric"])
                    .build();
                view.add_item_menu_gesture(&label, list_item);
                list_item.set_child(Some(&label));
            }
        ));
        factory.connect_bind(move |_, list_item| {
            let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
            let (Some(item), Some(label)) = (
                list_item.item().and_downcast::<FileItem>(),
                list_item.child().and_downcast::<gtk::Label>(),
            ) else {
                return;
            };
            label.set_label(&text(&item));
        });
        factory
    }

    fn grid_factory(&self) -> gtk::SignalListItemFactory {
        let icon_size = self.icon_size();
        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(clone!(
            #[weak(rename_to = view)]
            self,
            move |_, list_item| {
                let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
                let image = gtk::Image::builder()
                    .pixel_size(icon_size)
                    .halign(gtk::Align::Center)
                    .build();
                let label = gtk::Label::builder()
                    .wrap(true)
                    .wrap_mode(gtk::pango::WrapMode::WordChar)
                    .lines(2)
                    .ellipsize(gtk::pango::EllipsizeMode::Middle)
                    .justify(gtk::Justification::Center)
                    .max_width_chars(1)
                    .valign(gtk::Align::Start)
                    .build();
                let cell = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(6)
                    .width_request(icon_size + 32)
                    .build();
                cell.append(&image);
                cell.append(&label);
                view.add_item_menu_gesture(&cell, list_item);
                list_item.set_child(Some(&cell));
            }
        ));
        factory.connect_bind(|_, list_item| {
            let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
            let (Some(item), Some(cell)) = (
                list_item.item().and_downcast::<FileItem>(),
                list_item.child(),
            ) else {
                return;
            };
            let image = cell.first_child().and_downcast::<gtk::Image>().unwrap();
            let label = cell.last_child().and_downcast::<gtk::Label>().unwrap();
            image.set_from_gicon(&item.icon().unwrap_or_else(fallback_icon));
            label.set_label(&item.display_name());
            cell.set_opacity(if item.is_hidden() { 0.6 } else { 1.0 });
            cell.set_tooltip_text(Some(&item.display_name()));
        });
        factory
    }

    /// Right-click on an item: select it (unless already selected) and show
    /// the item menu. Middle-click: open it in a new tab.
    fn add_item_menu_gesture(&self, widget: &impl IsA<gtk::Widget>, list_item: &gtk::ListItem) {
        let gesture = gtk::GestureClick::builder().button(0).build();
        gesture.connect_pressed(clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            list_item,
            move |gesture, _, x, y| {
                let button = gesture.current_button();
                if button != gdk::BUTTON_SECONDARY && button != gdk::BUTTON_MIDDLE {
                    return;
                }
                gesture.set_state(gtk::EventSequenceState::Claimed);
                let position = list_item.position();
                // Middle-click acts on the clicked item only.
                if let Some(selection) = view.imp().selection.borrow().as_ref()
                    && (button == gdk::BUTTON_MIDDLE || !selection.is_selected(position))
                {
                    selection.select_item(position, true);
                }
                if button == gdk::BUTTON_MIDDLE {
                    let _ = view.activate_action("win.open-in-new-tab", None);
                } else {
                    let widget = gesture.widget().unwrap();
                    view.popup_menu(&item_menu(), &widget, x, y);
                }
            }
        ));
        widget.add_controller(gesture);
    }

    /// Right-click on empty space: clear the selection and show the folder menu.
    fn add_background_menu_gesture(&self, widget: &impl IsA<gtk::Widget>) {
        let gesture = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .build();
        gesture.connect_pressed(clone!(
            #[weak(rename_to = view)]
            self,
            move |gesture, _, x, y| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                if let Some(selection) = view.imp().selection.borrow().as_ref() {
                    selection.unselect_all();
                }
                let widget = gesture.widget().unwrap();
                view.popup_menu(&background_menu(), &widget, x, y);
            }
        ));
        widget.add_controller(gesture);
    }

    fn popup_menu(&self, model: &gio::Menu, relative_to: &gtk::Widget, x: f64, y: f64) {
        let menu = self.imp().context_menu.get().unwrap();
        let point = relative_to
            .compute_point(self, &gtk::graphene::Point::new(x as f32, y as f32))
            .unwrap_or_else(|| gtk::graphene::Point::new(x as f32, y as f32));
        menu.set_menu_model(Some(model));
        menu.set_pointing_to(Some(&gdk::Rectangle::new(
            point.x() as i32,
            point.y() as i32,
            1,
            1,
        )));
        menu.popup();
    }

    /// Ctrl + scroll changes the icon size.
    fn add_zoom_controller(&self, widget: &impl IsA<gtk::Widget>) {
        let controller = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        controller.set_propagation_phase(gtk::PropagationPhase::Capture);
        controller.connect_scroll(clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |controller, _, dy| {
                if !controller
                    .current_event_state()
                    .contains(gdk::ModifierType::CONTROL_MASK)
                {
                    return glib::Propagation::Proceed;
                }
                let action = if dy < 0.0 {
                    "win.zoom-in"
                } else {
                    "win.zoom-out"
                };
                let _ = view.activate_action(action, None);
                glib::Propagation::Stop
            }
        ));
        widget.add_controller(controller);
    }
}

fn fallback_icon() -> gio::Icon {
    gio::ThemedIcon::new("text-x-generic").upcast()
}

/// Shortcuts that act on the file view's selection.
const VIEW_SHORTCUTS: [(&str, &str); 9] = [
    ("BackSpace", "win.back"),
    ("<Control>Return", "win.open-in-new-tab"),
    ("<Control>x", "win.cut"),
    ("<Control>c", "win.copy"),
    ("<Control>v", "win.paste"),
    ("F2", "win.rename"),
    ("Delete", "win.trash"),
    ("<Shift>Delete", "win.delete"),
    ("<Alt>Return", "win.properties"),
];

fn section(items: &[(String, &str)]) -> gio::Menu {
    let menu = gio::Menu::new();
    for (label, action) in items {
        menu.append(Some(label), Some(action));
    }
    menu
}

fn item_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    menu.append_section(
        None,
        &section(&[
            (gettext("_Open"), "win.open"),
            (gettext("Open _With…"), "win.open-with"),
            (gettext("Open in New _Tab"), "win.open-in-new-tab"),
        ]),
    );
    menu.append_section(
        None,
        &section(&[
            (gettext("Cu_t"), "win.cut"),
            (gettext("_Copy"), "win.copy"),
            (gettext("_Rename…"), "win.rename"),
            (gettext("Move to _Trash"), "win.trash"),
        ]),
    );
    menu.append_section(
        None,
        &section(&[
            (gettext("_Pin to Sidebar"), "win.pin"),
            (gettext("Copy _Path"), "win.copy-path"),
        ]),
    );
    menu.append_section(
        None,
        &section(&[(gettext("P_roperties"), "win.properties")]),
    );
    menu
}

fn background_menu() -> gio::Menu {
    let menu = gio::Menu::new();

    let submenus = gio::Menu::new();
    submenus.append_submenu(
        Some(&gettext("_View")),
        &section(&[
            (gettext("_Details"), "win.default-view::details"),
            (gettext("_Grid"), "win.default-view::grid"),
        ]),
    );
    let sort = gio::Menu::new();
    sort.append_section(None, &sort_by_section());
    sort.append_section(None, &sort_order_section());
    submenus.append_submenu(Some(&gettext("_Sort By")), &sort);
    submenus.append_submenu(
        Some(&gettext("_New")),
        &section(&[
            (gettext("_Folder"), "win.new-folder"),
            (gettext("_Empty File"), "win.new-file"),
        ]),
    );
    menu.append_section(None, &submenus);

    menu.append_section(
        None,
        &section(&[
            (gettext("_Paste"), "win.paste"),
            (gettext("Select _All"), "win.select-all"),
        ]),
    );
    menu.append_section(
        None,
        &section(&[
            (gettext("Show _Hidden Files"), "win.show-hidden"),
            (
                gettext("Sort _Folders Before Files"),
                "win.sort-folders-first",
            ),
        ]),
    );
    menu.append_section(
        None,
        &section(&[
            (gettext("_Refresh"), "win.reload"),
            (gettext("Copy _Path"), "win.copy-path"),
            (gettext("P_roperties"), "win.properties"),
        ]),
    );
    menu
}

fn sort_by_section() -> gio::Menu {
    section(&[
        (gettext("_Name"), "win.sort-by::name"),
        (gettext("_Date Modified"), "win.sort-by::modified"),
        (gettext("_Type"), "win.sort-by::type"),
        (gettext("_Size"), "win.sort-by::size"),
    ])
}

fn sort_order_section() -> gio::Menu {
    section(&[
        (gettext("_Ascending"), "win.sort-order::ascending"),
        (gettext("D_escending"), "win.sort-order::descending"),
    ])
}
