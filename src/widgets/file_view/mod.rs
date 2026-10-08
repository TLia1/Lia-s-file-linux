//! `FileView`: shows a selection model of `FileItem`s either as a details
//! table (`gtk::ColumnView`) or as an icon grid (`gtk::GridView`). Both are
//! virtualized, so only visible rows are instantiated.
//!
//! It also handles what happens *on* the items: context menus, dragging
//! files out, dropping files on folders or on the background (emitted as
//! `drop-files`), dimming cut files, and the inline rename popover.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;

use gettextrs::gettext;
use gtk::{gdk, gio, glib, glib::clone, prelude::*, subclass::prelude::*};

use crate::model::{FileItem, sorting};
use crate::services::clipboard::clipboard_service;
use crate::util::format;
use crate::widgets::dnd;
use crate::widgets::folder_pane;
use crate::widgets::thumbnail_image::ThumbnailImage;

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

/// A visible item's main widget (name cell or grid cell), used to dim cut
/// files and to place the rename popover.
struct BoundCell {
    list_item: glib::WeakRef<gtk::ListItem>,
    widget: glib::WeakRef<gtk::Widget>,
}

/// Checks a new name; returns an error message if it's invalid.
type Validator = Box<dyn Fn(&str) -> Option<String>>;

/// A rename in progress.
struct Rename {
    original: String,
    validate: Validator,
    commit: Box<dyn Fn(String)>,
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FileView {
        pub(super) stack: OnceCell<gtk::Stack>,
        pub(super) column_view: OnceCell<gtk::ColumnView>,
        pub(super) grid_view: OnceCell<gtk::GridView>,
        pub(super) name_column: OnceCell<gtk::ColumnViewColumn>,
        /// "Original location" and "Date deleted", shown in the trash.
        pub(super) trash_columns: OnceCell<[gtk::ColumnViewColumn; 2]>,
        /// "Folder", shown with search results.
        pub(super) folder_column: OnceCell<gtk::ColumnViewColumn>,
        pub(super) selection: RefCell<Option<gtk::MultiSelection>>,
        pub(super) context_menu: OnceCell<gtk::PopoverMenu>,
        pub(super) icon_size: Cell<i32>,
        pub(super) mode: Cell<ViewMode>,
        /// The folder shown: the target of drops on the background.
        pub(super) location: RefCell<Option<gio::File>>,
        pub(super) accepts_drops: Cell<bool>,
        pub(super) trash_mode: Cell<bool>,
        pub(super) cells: RefCell<Vec<BoundCell>>,
        pub(super) clipboard_handler: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) rename_popover: OnceCell<gtk::Popover>,
        pub(super) rename_entry: OnceCell<gtk::Entry>,
        pub(super) rename_error: OnceCell<gtk::Label>,
        pub(super) rename: RefCell<Option<Rename>>,
        pub(super) rename_cancelled: Cell<bool>,
        /// Characters to select once the entry gets focus.
        pub(super) rename_selection: Cell<Option<usize>>,
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
            self.icon_size.set(DEFAULT_ICON_SIZE);
            self.accepts_drops.set(true);
            self.obj().build();
        }

        fn dispose(&self) {
            if let Some(handler) = self.clipboard_handler.take() {
                clipboard_service().disconnect(handler);
            }
            if let Some(menu) = self.context_menu.get() {
                menu.unparent();
            }
            if let Some(popover) = self.rename_popover.get() {
                popover.unparent();
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
            if let Some(popover) = self.rename_popover.get() {
                popover.present();
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
        for scroller in [&details_scroller, &grid_scroller] {
            self.add_background_menu_gesture(scroller);
            self.add_background_drop_target(scroller);
        }
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

        // Cut files are dimmed until the clipboard changes.
        let handler = clipboard_service().connect_changed(clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.refresh_cells()
        ));
        imp.clipboard_handler.replace(Some(handler));

        imp.stack.set(stack).unwrap();
        imp.column_view.set(column_view).unwrap();
        imp.grid_view.set(grid_view).unwrap();
        imp.context_menu.set(menu).unwrap();
        self.build_rename_popover();
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

    /// The folder shown, where files dropped on the background go.
    pub fn set_location(&self, location: Option<&gio::File>) {
        self.imp().location.replace(location.cloned());
    }

    /// Whether files can be dropped on the background (the folder is
    /// writable).
    pub fn set_accepts_drops(&self, accepts: bool) {
        self.imp().accepts_drops.set(accepts);
    }

    /// The trash shows where items came from and when they were deleted,
    /// and different menus.
    pub fn set_trash_mode(&self, trash: bool) {
        let imp = self.imp();
        if imp.trash_mode.replace(trash) == trash {
            return;
        }
        for column in imp.trash_columns.get().unwrap() {
            column.set_visible(trash);
        }
    }

    /// Search results come from many folders: show which one in the
    /// details view.
    pub fn set_search_mode(&self, search: bool) {
        self.imp().folder_column.get().unwrap().set_visible(search);
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

    pub fn selected_items(&self) -> Vec<FileItem> {
        let Some(selection) = self.imp().selection.borrow().clone() else {
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

    /// The files operations on the selection act on.
    pub fn selected_files(&self) -> Vec<gio::File> {
        self.selected_items()
            .iter()
            .map(FileItem::target_file)
            .collect()
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

    pub fn connect_drop_files<
        F: Fn(&Self, &gio::File, Vec<gio::File>, gdk::DragAction) + 'static,
    >(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "drop-files",
            false,
            glib::closure_local!(move |view: &Self,
                                       dest: &gio::File,
                                       files: &gdk::FileList,
                                       action: gdk::DragAction| {
                f(view, dest, files.files(), action)
            }),
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
        let view = self.downgrade();
        let folder = gtk::ColumnViewColumn::builder()
            .id("folder")
            .title(gettext("Folder"))
            .factory(&self.text_factory(0.0, move |item| {
                view.upgrade()
                    .and_then(|view| view.imp().location.borrow().clone())
                    .map(|root| folder_pane::relative_folder(&root, item.file()))
                    .unwrap_or_default()
            }))
            .sorter(&gtk::CustomSorter::new(|a, b| {
                let parent = |o: &glib::Object| {
                    o.downcast_ref::<FileItem>()
                        .and_then(|item| item.file().parent())
                        .map(|parent| parent.uri())
                };
                parent(a).cmp(&parent(b)).into()
            }))
            .fixed_width(240)
            .resizable(true)
            .visible(false)
            .build();
        let origin = gtk::ColumnViewColumn::builder()
            .id("origin")
            .title(gettext("Original location"))
            .factory(&self.text_factory(0.0, |item| {
                item.trash_orig_path()
                    .and_then(|path| path.parent().map(|p| gio::File::for_path(p).parse_name()))
                    .map(String::from)
                    .unwrap_or_default()
            }))
            .sorter(&gtk::CustomSorter::new(|a, b| {
                let path = |o: &glib::Object| {
                    o.downcast_ref::<FileItem>()
                        .and_then(|item| item.trash_orig_path())
                };
                path(a).cmp(&path(b)).into()
            }))
            .fixed_width(220)
            .resizable(true)
            .visible(false)
            .build();
        let deleted = gtk::ColumnViewColumn::builder()
            .id("deleted")
            .title(gettext("Date deleted"))
            .factory(&self.text_factory(0.0, |item| format::date(deletion_time(item))))
            .sorter(&gtk::CustomSorter::new(|a, b| {
                let time = |o: &glib::Object| o.downcast_ref::<FileItem>().map(deletion_time);
                time(a).cmp(&time(b)).into()
            }))
            .fixed_width(170)
            .resizable(true)
            .visible(false)
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

        for column in [&name, &folder, &origin, &deleted, &modified, &kind, &size] {
            column_view.append_column(column);
        }
        column_view.sort_by_column(Some(&name), gtk::SortType::Ascending);
        self.imp().name_column.set(name).unwrap();
        self.imp().trash_columns.set([origin, deleted]).unwrap();
        self.imp().folder_column.set(folder).unwrap();
    }

    /// Controllers shared by every cell: menus, dragging, dropping.
    fn setup_cell(&self, widget: &impl IsA<gtk::Widget>, list_item: &gtk::ListItem) {
        self.add_item_menu_gesture(widget, list_item);
        self.add_drag_source(widget, list_item);
        self.add_item_drop_target(widget, list_item);
    }

    /// Remembers an item's main widget while it exists.
    fn track_cell(&self, widget: &impl IsA<gtk::Widget>, list_item: &gtk::ListItem) {
        let mut cells = self.imp().cells.borrow_mut();
        cells.retain(|cell| cell.widget.upgrade().is_some());
        cells.push(BoundCell {
            list_item: list_item.downgrade(),
            widget: widget.upcast_ref::<gtk::Widget>().downgrade(),
        });
    }

    fn untrack_cell(&self, widget: Option<gtk::Widget>) {
        self.imp().cells.borrow_mut().retain(|cell| {
            cell.widget
                .upgrade()
                .is_some_and(|w| Some(&w) != widget.as_ref())
        });
    }

    /// Re-applies dimming (hidden / cut) to the visible items.
    fn refresh_cells(&self) {
        let cells: Vec<(gtk::Widget, FileItem)> = self
            .imp()
            .cells
            .borrow()
            .iter()
            .filter_map(|cell| {
                let item = cell
                    .list_item
                    .upgrade()?
                    .item()
                    .and_downcast::<FileItem>()?;
                Some((cell.widget.upgrade()?, item))
            })
            .collect();
        for (widget, item) in cells {
            style_item(&widget, &item);
        }
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
                view.setup_cell(&row, list_item);
                view.track_cell(&row, list_item);
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
            style_item(&row, &item);
        });
        factory.connect_teardown(clone!(
            #[weak(rename_to = view)]
            self,
            move |_, list_item| {
                let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
                view.untrack_cell(list_item.child());
            }
        ));
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
                view.setup_cell(&label, list_item);
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
                let image = ThumbnailImage::new(icon_size);
                image.set_halign(gtk::Align::Center);
                image.set_valign(gtk::Align::End);
                // Thumbnails aren't square: keep names aligned.
                image.set_height_request(icon_size);
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
                view.setup_cell(&cell, list_item);
                view.track_cell(&cell, list_item);
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
            let image = cell.first_child().and_downcast::<ThumbnailImage>().unwrap();
            let label = cell.last_child().and_downcast::<gtk::Label>().unwrap();
            image.set_item(Some(&item));
            label.set_label(&item.display_name());
            style_item(&cell, &item);
            cell.set_tooltip_text(Some(&item.display_name()));
        });
        // Unbound cells stop waiting for their thumbnail.
        factory.connect_unbind(|_, list_item| {
            let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
            if let Some(image) = list_item
                .child()
                .and_then(|cell| cell.first_child())
                .and_downcast::<ThumbnailImage>()
            {
                image.set_item(None);
            }
        });
        factory.connect_teardown(clone!(
            #[weak(rename_to = view)]
            self,
            move |_, list_item| {
                let list_item = list_item.downcast_ref::<gtk::ListItem>().unwrap();
                view.untrack_cell(list_item.child());
            }
        ));
        factory
    }

    // ── Context menus ──────────────────────────────────────────────────────

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
                    let menu = item_menu(view.imp().trash_mode.get());
                    view.popup_menu(&menu, &widget, x, y);
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
                let menu = background_menu(view.imp().trash_mode.get());
                view.popup_menu(&menu, &widget, x, y);
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

    // ── Drag and drop ──────────────────────────────────────────────────────

    /// Dragging an item drags the selection (selecting the item first if
    /// it isn't selected).
    fn add_drag_source(&self, widget: &impl IsA<gtk::Widget>, list_item: &gtk::ListItem) {
        let source = gtk::DragSource::builder().actions(dnd::ACTIONS).build();
        source.connect_prepare(clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            list_item,
            #[upgrade_or]
            None,
            move |source, _, _| {
                let selection = view.imp().selection.borrow().clone()?;
                let position = list_item.position();
                if position == gtk::INVALID_LIST_POSITION {
                    return None;
                }
                if !selection.is_selected(position) {
                    selection.select_item(position, true);
                }
                let files = view.selected_files();
                if files.is_empty() {
                    return None;
                }
                if let Some(item) = list_item.item().and_downcast::<FileItem>() {
                    source.set_icon(Some(&view.drag_icon(&item)), 12, 12);
                }
                Some(gdk::ContentProvider::for_value(
                    &gdk::FileList::from_array(&files).to_value(),
                ))
            }
        ));
        widget.add_controller(source);
    }

    /// The picture under the pointer while dragging: the item's thumbnail
    /// or icon.
    fn drag_icon(&self, item: &FileItem) -> gdk::Paintable {
        const SIZE: f64 = 64.0;
        if let Some(thumbnail) = item.thumbnail() {
            let (width, height) = (thumbnail.width() as f64, thumbnail.height() as f64);
            let scale = SIZE / width.max(height).max(1.0);
            let snapshot = gtk::Snapshot::new();
            thumbnail.snapshot(&snapshot, width * scale, height * scale);
            if let Some(paintable) = snapshot.to_paintable(None) {
                return paintable;
            }
        }
        let icon = item.icon().unwrap_or_else(fallback_icon);
        gtk::IconTheme::for_display(&self.display())
            .lookup_by_gicon(
                &icon,
                48,
                self.scale_factor(),
                gtk::TextDirection::None,
                gtk::IconLookupFlags::empty(),
            )
            .upcast()
    }

    /// The action for a drop on `dest`: from the modifiers held, and
    /// whether the files are on the same device (computed once per hover).
    fn drop_action(
        &self,
        target: &gtk::DropTarget,
        dest: &gio::File,
        same_device: &Cell<Option<bool>>,
    ) -> gdk::DragAction {
        let files = target
            .value()
            .and_then(|v| v.get::<gdk::FileList>().ok())
            .map(|list| list.files())
            .unwrap_or_default();
        let Some(first) = files.first() else {
            // Not loaded yet.
            return gdk::DragAction::COPY;
        };
        dnd::action_for(&files, dest, target.current_event_state(), || {
            if let Some(same) = same_device.get() {
                return same;
            }
            let same = dnd::same_device(first, dest);
            same_device.set(Some(same));
            same
        })
    }

    /// Folders accept drops; hovering one for a moment opens it.
    fn add_item_drop_target(&self, widget: &impl IsA<gtk::Widget>, list_item: &gtk::ListItem) {
        let target = gtk::DropTarget::new(gdk::FileList::static_type(), dnd::ACTIONS);
        target.set_preload(true);
        let spring: Rc<RefCell<Option<glib::SourceId>>> = Rc::default();
        let same_device: Rc<Cell<Option<bool>>> = Rc::default();
        let folder = |list_item: &gtk::ListItem| {
            list_item
                .item()
                .and_downcast::<FileItem>()
                .filter(|item| item.is_dir())
                .map(|item| item.target_file())
        };
        let cancel_spring = |spring: &RefCell<Option<glib::SourceId>>| {
            if let Some(id) = spring.take() {
                id.remove();
            }
        };

        target.connect_accept(clone!(
            #[weak]
            list_item,
            #[upgrade_or]
            false,
            move |_, drop| {
                drop.formats().contains_type(gdk::FileList::static_type())
                    && folder(&list_item).is_some()
            }
        ));
        target.connect_enter(clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            list_item,
            #[strong]
            spring,
            #[strong]
            same_device,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |target, _, _| {
                same_device.set(None);
                dnd::highlight_row(&target.widget().unwrap(), true);
                cancel_spring(&spring);
                let id = glib::timeout_add_local_once(
                    dnd::SPRING_DELAY,
                    clone!(
                        #[weak]
                        view,
                        #[weak]
                        list_item,
                        #[strong]
                        spring,
                        move || {
                            spring.take();
                            view.activate_position(list_item.position());
                        }
                    ),
                );
                spring.replace(Some(id));
                let Some(dest) = folder(&list_item) else {
                    return gdk::DragAction::empty();
                };
                view.drop_action(target, &dest, &same_device)
            }
        ));
        target.connect_motion(clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            list_item,
            #[strong]
            same_device,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |target, _, _| match folder(&list_item) {
                Some(dest) => view.drop_action(target, &dest, &same_device),
                None => gdk::DragAction::empty(),
            }
        ));
        target.connect_leave(clone!(
            #[strong]
            spring,
            move |target| {
                dnd::highlight_row(&target.widget().unwrap(), false);
                cancel_spring(&spring);
            }
        ));
        target.connect_drop(clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            list_item,
            #[strong]
            spring,
            #[strong]
            same_device,
            #[upgrade_or]
            false,
            move |target, value, _, _| {
                dnd::highlight_row(&target.widget().unwrap(), false);
                cancel_spring(&spring);
                let (Ok(files), Some(dest)) = (value.get::<gdk::FileList>(), folder(&list_item))
                else {
                    return false;
                };
                let action = view.drop_action(target, &dest, &same_device);
                if action.is_empty() {
                    return false;
                }
                view.emit_by_name::<()>("drop-files", &[&dest, &files, &action]);
                true
            }
        ));
        widget.add_controller(target);
    }

    /// Drops on empty space (or on a file) go into the folder shown.
    fn add_background_drop_target(&self, widget: &impl IsA<gtk::Widget>) {
        let target = gtk::DropTarget::new(gdk::FileList::static_type(), dnd::ACTIONS);
        target.set_preload(true);
        let same_device: Rc<Cell<Option<bool>>> = Rc::default();

        target.connect_accept(clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            false,
            move |_, drop| {
                let imp = view.imp();
                imp.accepts_drops.get()
                    && imp.location.borrow().is_some()
                    && drop.formats().contains_type(gdk::FileList::static_type())
            }
        ));
        let motion = clone!(
            #[weak(rename_to = view)]
            self,
            #[strong]
            same_device,
            #[upgrade_or]
            gdk::DragAction::empty(),
            move |target: &gtk::DropTarget, _: f64, _: f64| {
                let location = view.imp().location.borrow().clone();
                match location {
                    Some(dest) => view.drop_action(target, &dest, &same_device),
                    None => gdk::DragAction::empty(),
                }
            }
        );
        target.connect_enter(clone!(
            #[strong]
            motion,
            #[strong]
            same_device,
            move |target, x, y| {
                same_device.set(None);
                motion(target, x, y)
            }
        ));
        target.connect_motion(motion);
        target.connect_drop(clone!(
            #[weak(rename_to = view)]
            self,
            #[strong]
            same_device,
            #[upgrade_or]
            false,
            move |target, value, _, _| {
                let location = view.imp().location.borrow().clone();
                let (Ok(files), Some(dest)) = (value.get::<gdk::FileList>(), location) else {
                    return false;
                };
                let action = view.drop_action(target, &dest, &same_device);
                if action.is_empty() {
                    return false;
                }
                view.emit_by_name::<()>("drop-files", &[&dest, &files, &action]);
                true
            }
        ));
        widget.add_controller(target);
    }

    // ── Inline rename ──────────────────────────────────────────────────────

    fn build_rename_popover(&self) {
        let imp = self.imp();
        let entry = gtk::Entry::builder().width_chars(30).build();
        let error = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .max_width_chars(34)
            .visible(false)
            .css_classes(["caption", "error"])
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .build();
        content.append(&entry);
        content.append(&error);
        let popover = gtk::Popover::builder()
            .child(&content)
            .position(gtk::PositionType::Bottom)
            .css_classes(["rename-popover"])
            .build();
        popover.set_parent(self);

        entry.connect_changed(clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                view.validate_rename();
            }
        ));
        entry.connect_activate(clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            popover,
            move |_| {
                if view.validate_rename() {
                    popover.popdown();
                }
            }
        ));
        // Escape cancels; closing any other way (Enter, clicking elsewhere)
        // commits, like Explorer.
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            popover,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, _| {
                if key != gdk::Key::Escape {
                    return glib::Propagation::Proceed;
                }
                view.imp().rename_cancelled.set(true);
                popover.popdown();
                glib::Propagation::Stop
            }
        ));
        popover.add_controller(keys);
        // The popover maps asynchronously, and focusing the entry selects
        // everything: select the name without its extension once mapped,
        // and again when the focus arrives.
        popover.connect_map(clone!(
            #[weak(rename_to = view)]
            self,
            #[weak]
            entry,
            move |_| {
                entry.grab_focus();
                view.apply_rename_selection(false);
            }
        ));
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(clone!(
            #[weak(rename_to = view)]
            self,
            move |_| {
                glib::idle_add_local_once(clone!(
                    #[weak]
                    view,
                    move || view.apply_rename_selection(true)
                ));
            }
        ));
        entry.add_controller(focus);
        popover.connect_closed(clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.finish_rename()
        ));

        imp.rename_popover.set(popover).unwrap();
        imp.rename_entry.set(entry).unwrap();
        imp.rename_error.set(error).unwrap();
    }

    /// Edits the name of the item at `position` in a popover, with the
    /// first `select_chars` characters (the name without its extension)
    /// selected. `validate` returns an error message for invalid names;
    /// `commit` receives a valid, changed name.
    pub fn start_rename(
        &self,
        position: u32,
        name: &str,
        select_chars: usize,
        validate: impl Fn(&str) -> Option<String> + 'static,
        commit: impl Fn(String) + 'static,
    ) {
        let imp = self.imp();
        if let Some(popover) = imp.rename_popover.get()
            && popover.is_visible()
        {
            imp.rename_cancelled.set(true);
            popover.popdown();
        }
        imp.rename.replace(Some(Rename {
            original: name.to_owned(),
            validate: Box::new(validate),
            commit: Box::new(commit),
        }));
        imp.rename_cancelled.set(false);
        let entry = imp.rename_entry.get().unwrap();
        entry.set_text(name);
        self.validate_rename();
        self.scroll_to(position, false);

        // Point at the item once the view has scrolled to it: two frames
        // later, the rows are allocated.
        let frames = Cell::new(0);
        self.add_tick_callback(move |view, _| {
            frames.set(frames.get() + 1);
            if frames.get() < 2 {
                return glib::ControlFlow::Continue;
            }
            view.show_rename_popover(position, select_chars);
            glib::ControlFlow::Break
        });
    }

    fn show_rename_popover(&self, position: u32, select_chars: usize) {
        let imp = self.imp();
        if imp.rename.borrow().is_none() {
            return;
        }
        let cell = imp
            .cells
            .borrow()
            .iter()
            .filter_map(|cell| Some((cell.list_item.upgrade()?, cell.widget.upgrade()?)))
            .find(|(item, widget)| item.position() == position && widget.is_mapped())
            .map(|(_, widget)| widget);
        // In the details view, point at the name rather than the icon.
        let anchor = cell.map(|cell| match imp.mode.get() {
            ViewMode::Details => cell.last_child().unwrap_or(cell),
            ViewMode::Grid => cell,
        });
        let rect = anchor
            .and_then(|anchor| anchor.compute_bounds(self))
            .map(|b| {
                gdk::Rectangle::new(
                    b.x() as i32,
                    b.y() as i32,
                    (b.width() as i32).max(1),
                    (b.height() as i32).max(1),
                )
            })
            .unwrap_or_else(|| gdk::Rectangle::new(self.width() / 2, 0, 1, 1));

        let popover = imp.rename_popover.get().unwrap();
        let entry = imp.rename_entry.get().unwrap();
        imp.rename_selection.set(Some(select_chars));
        popover.set_pointing_to(Some(&rect));
        popover.popup();
        entry.grab_focus();
    }

    /// Selects the part of the name to edit; `last` ends the pre-selection
    /// (the user can then select freely).
    fn apply_rename_selection(&self, last: bool) {
        let imp = self.imp();
        let chars = if last {
            imp.rename_selection.take()
        } else {
            imp.rename_selection.get()
        };
        if let Some(chars) = chars {
            imp.rename_entry
                .get()
                .unwrap()
                .select_region(0, chars as i32);
        }
    }

    /// Shows the error for the current text, if any. Returns validity.
    fn validate_rename(&self) -> bool {
        let imp = self.imp();
        let entry = imp.rename_entry.get().unwrap();
        let label = imp.rename_error.get().unwrap();
        let text = entry.text();
        let error = imp
            .rename
            .borrow()
            .as_ref()
            .and_then(|rename| (rename.validate)(&text));
        label.set_visible(error.is_some());
        label.set_label(error.as_deref().unwrap_or_default());
        if error.is_some() {
            entry.add_css_class("error");
        } else {
            entry.remove_css_class("error");
        }
        error.is_none()
    }

    fn finish_rename(&self) {
        let imp = self.imp();
        let Some(rename) = imp.rename.take() else {
            return;
        };
        let cancelled = imp.rename_cancelled.replace(false);
        let text = imp.rename_entry.get().unwrap().text().to_string();
        if !cancelled && text != rename.original && (rename.validate)(&text).is_none() {
            (rename.commit)(text);
        }
        self.grab_focus();
    }
}

/// Dims hidden and cut items.
fn style_item(widget: &gtk::Widget, item: &FileItem) {
    let cut = clipboard_service().is_cut(&item.target_file());
    let opacity = match (cut, item.is_hidden()) {
        (true, _) => 0.45,
        (false, true) => 0.6,
        (false, false) => 1.0,
    };
    widget.set_opacity(opacity);
}

/// When an item was moved to the trash, as a UNIX timestamp (0 if unknown).
fn deletion_time(item: &FileItem) -> i64 {
    item.trash_deletion_date()
        .and_then(|date| glib::DateTime::from_iso8601(&date, Some(&glib::TimeZone::local())).ok())
        .map_or(0, |date| date.to_unix())
}

fn fallback_icon() -> gio::Icon {
    gio::ThemedIcon::new("text-x-generic").upcast()
}

/// Shortcuts that act on the file view's selection.
const VIEW_SHORTCUTS: [(&str, &str); 11] = [
    ("BackSpace", "win.back"),
    ("<Control>Return", "win.open-in-new-tab"),
    ("<Control>x", "win.cut"),
    ("<Control>c", "win.copy"),
    ("<Control>v", "win.paste"),
    ("<Control>d", "win.duplicate"),
    ("F2", "win.rename"),
    ("Delete", "win.trash"),
    ("KP_Delete", "win.trash"),
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

fn item_menu(trash: bool) -> gio::Menu {
    let menu = gio::Menu::new();
    if trash {
        menu.append_section(
            None,
            &section(&[
                (gettext("_Restore"), "win.restore"),
                (gettext("_Delete Permanently"), "win.delete"),
            ]),
        );
        menu.append_section(
            None,
            &section(&[(gettext("Cu_t"), "win.cut"), (gettext("_Copy"), "win.copy")]),
        );
        menu.append_section(
            None,
            &section(&[(gettext("P_roperties"), "win.properties")]),
        );
        return menu;
    }
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
            (gettext("D_uplicate"), "win.duplicate"),
            (gettext("Create _Link"), "win.create-link"),
        ]),
    );
    menu.append_section(
        None,
        &section(&[
            (gettext("_Rename…"), "win.rename"),
            (gettext("Move to _Trash"), "win.trash"),
        ]),
    );
    menu.append_section(
        None,
        &section(&[
            (gettext("_Pin to Sidebar"), "win.pin"),
            (gettext("Copy _Path"), "win.copy-path"),
            (gettext("Open in Ter_minal"), "win.open-terminal"),
        ]),
    );
    menu.append_section(
        None,
        &section(&[(gettext("P_roperties"), "win.properties")]),
    );
    menu
}

fn background_menu(trash: bool) -> gio::Menu {
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
    if !trash {
        let new = section(&[
            (gettext("_Folder"), "win.new-folder"),
            (gettext("_Empty File"), "win.new-file"),
        ]);
        new.append_section(None, &crate::services::templates::templates_menu());
        submenus.append_submenu(Some(&gettext("_New")), &new);
    }
    menu.append_section(None, &submenus);

    if trash {
        menu.append_section(
            None,
            &section(&[
                (gettext("_Empty Trash"), "win.empty-trash"),
                (gettext("Select _All"), "win.select-all"),
            ]),
        );
    } else {
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
                (gettext("_Undo"), "app.undo"),
                (gettext("_Redo"), "app.redo"),
            ]),
        );
    }
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
            (gettext("Open in Ter_minal"), "win.open-terminal"),
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
