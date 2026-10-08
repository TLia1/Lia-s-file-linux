//! `DetailsPane`: the pane on the right describing the selection of the
//! active tab — a large thumbnail or a text preview, the name and type,
//! and a few properties (size, dimensions, dates, location…).
//!
//! With nothing selected it describes the folder itself; with several
//! items, their count and total size. It only does work while visible,
//! and slow lookups (folder contents, image dimensions, text) run async
//! and are cancelled when the selection changes.

use std::cell::{OnceCell, RefCell};
use std::time::Duration;

use adw::prelude::*;
use gettextrs::{gettext, ngettext};
use gtk::{gdk_pixbuf, gio, glib, glib::clone, subclass::prelude::*};
use sourceview5::prelude::*;

use crate::model::{FileItem, places};
use crate::util::format;
use crate::widgets::folder_pane::{self, FolderPane};
use crate::widgets::thumbnail_image::ThumbnailImage;

/// How much of a text file is previewed.
pub const TEXT_PREVIEW_BYTES: usize = 200 * 1024;
/// Selection changes are coalesced (rubber-band selection, Ctrl+A…).
const REFRESH_DELAY: Duration = Duration::from_millis(60);

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/lia/Files/ui/details-pane.ui")]
    pub struct DetailsPane {
        #[template_child]
        pub(super) preview_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub(super) preview_image: TemplateChild<ThumbnailImage>,
        #[template_child]
        pub(super) text_scroller: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub(super) name_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) type_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) properties_list: TemplateChild<gtk::ListBox>,

        pub(super) text_view: OnceCell<sourceview5::View>,
        pub(super) pane: glib::WeakRef<FolderPane>,
        pub(super) pane_handlers: RefCell<Vec<glib::SignalHandlerId>>,
        pub(super) style_handler: RefCell<Option<glib::SignalHandlerId>>,
        /// What's shown: the folder and the selected items.
        pub(super) shown: RefCell<Option<(gio::File, Vec<FileItem>)>>,
        /// The folder's item count, kept up to date while it loads.
        pub(super) contents_row: RefCell<Option<adw::ActionRow>>,
        pub(super) tasks: RefCell<Vec<glib::JoinHandle<()>>>,
        pub(super) refresh_timeout: RefCell<Option<glib::SourceId>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DetailsPane {
        const NAME: &'static str = "LiaDetailsPane";
        type Type = super::DetailsPane;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            sourceview5::init();
            ThumbnailImage::ensure_type();
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("lia-details-pane");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for DetailsPane {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup_text_view();
        }

        fn dispose(&self) {
            let obj = self.obj();
            obj.set_folder_pane(None);
            obj.cancel_tasks();
            if let Some(handler) = self.style_handler.take() {
                adw::StyleManager::default().disconnect(handler);
            }
            self.dispose_template();
        }
    }

    impl WidgetImpl for DetailsPane {
        fn map(&self) {
            self.parent_map();
            // Changes while hidden were ignored.
            self.obj().refresh();
        }
    }
}

glib::wrapper! {
    pub struct DetailsPane(ObjectSubclass<imp::DetailsPane>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for DetailsPane {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl DetailsPane {
    /// Follows the selection of `pane` (the active tab).
    pub fn set_folder_pane(&self, pane: Option<&FolderPane>) {
        let imp = self.imp();
        if let Some(old) = imp.pane.upgrade() {
            for handler in imp.pane_handlers.take() {
                old.disconnect(handler);
            }
        }
        imp.pane.set(pane);
        if let Some(pane) = pane {
            let refresh = clone!(
                #[weak(rename_to = details)]
                self,
                move |_: &FolderPane| details.schedule_refresh()
            );
            let handlers = vec![
                pane.connect_selection_changed(refresh.clone()),
                pane.connect_location_notify(refresh.clone()),
                pane.connect_item_count_text_notify(refresh),
            ];
            imp.pane_handlers.replace(handlers);
        }
        self.schedule_refresh();
    }

    fn schedule_refresh(&self) {
        let imp = self.imp();
        if !self.is_mapped() || imp.refresh_timeout.borrow().is_some() {
            return;
        }
        let id = glib::timeout_add_local_once(
            REFRESH_DELAY,
            clone!(
                #[weak(rename_to = details)]
                self,
                move || {
                    details.imp().refresh_timeout.take();
                    details.refresh();
                }
            ),
        );
        imp.refresh_timeout.replace(Some(id));
    }

    fn refresh(&self) {
        let imp = self.imp();
        if let Some(id) = imp.refresh_timeout.take() {
            id.remove();
        }
        let pane = imp.pane.upgrade();
        let Some((pane, location)) = pane.and_then(|p| p.location().map(|l| (p, l))) else {
            self.cancel_tasks();
            imp.shown.replace(None);
            return;
        };
        let items = pane.selected_items();
        let unchanged = imp
            .shown
            .borrow()
            .as_ref()
            .is_some_and(|(shown_location, shown_items)| {
                shown_location.equal(&location) && *shown_items == items
            });
        if unchanged {
            if let Some(row) = imp.contents_row.borrow().as_ref() {
                row.set_subtitle(&pane.item_count_text());
            }
            return;
        }
        imp.shown.replace(Some((location.clone(), items.clone())));

        self.cancel_tasks();
        imp.properties_list.remove_all();
        imp.contents_row.replace(None);
        imp.preview_stack.set_visible_child_name("image");
        match items.as_slice() {
            [] => self.show_folder(&pane, &location),
            [item] => self.show_item(item),
            items => self.show_selection(items),
        }
        imp.properties_list
            .set_visible(imp.properties_list.first_child().is_some());
    }

    // ── The three cases ────────────────────────────────────────────────────

    fn show_folder(&self, pane: &FolderPane, location: &gio::File) {
        let imp = self.imp();
        imp.preview_image.set_icon(&folder_icon(location));
        imp.name_label
            .set_label(&folder_pane::display_name(location));
        imp.type_label.set_label(&gettext("Folder"));

        let contents = self.add_row(&gettext("Contents"), &pane.item_count_text());
        imp.contents_row.replace(Some(contents));
        self.add_row(&gettext("Location"), &location.parse_name());
        let modified = self.add_pending_row(&gettext("Modified"));
        let location = location.clone();
        self.spawn(async move {
            let info = location
                .query_info_future(
                    gio::FILE_ATTRIBUTE_TIME_MODIFIED,
                    gio::FileQueryInfoFlags::NONE,
                    glib::Priority::LOW,
                )
                .await;
            if let Ok(info) = info
                && info.has_attribute(gio::FILE_ATTRIBUTE_TIME_MODIFIED)
            {
                let time = info.attribute_uint64(gio::FILE_ATTRIBUTE_TIME_MODIFIED);
                fill_row(&modified, &format::full_date(time as i64));
            }
        });
    }

    fn show_item(&self, item: &FileItem) {
        let imp = self.imp();
        imp.preview_image.set_item(Some(item));
        imp.name_label
            .set_label(&format::one_line(&item.display_name()));
        let type_description = if item.is_dir() {
            gettext("Folder")
        } else {
            item.type_description()
        };
        imp.type_label.set_label(&type_description);
        let file = item.target_file();
        let content_type = item.content_type().map(String::from).unwrap_or_default();

        if item.is_dir() {
            let contents = self.add_pending_row(&gettext("Contents"));
            let folder = file.clone();
            self.spawn(async move {
                if let Ok(count) = count_children(&folder).await {
                    fill_row(&contents, &format::item_count(count));
                }
            });
        } else {
            self.add_row(
                &gettext("Size"),
                &glib::format_size_full(item.size(), glib::FormatSizeFlags::LONG_FORMAT),
            );
        }

        if content_type.starts_with("image/")
            && let Some(path) = file.path()
        {
            let dimensions = self.add_pending_row(&gettext("Dimensions"));
            self.spawn(async move {
                let size = gio::spawn_blocking(move || {
                    gdk_pixbuf::Pixbuf::file_info(&path).map(|(_, w, h)| (w, h))
                })
                .await;
                if let Ok(Some((width, height))) = size {
                    // Translators: image dimensions, e.g. "1920 × 1080 pixels"
                    let text = gettext("{w} × {h} pixels")
                        .replace("{w}", &width.to_string())
                        .replace("{h}", &height.to_string());
                    fill_row(&dimensions, &text);
                }
            });
        }

        if item.modified() > 0 {
            self.add_row(&gettext("Modified"), &format::full_date(item.modified()));
        }
        if let Some(created) = item.created() {
            self.add_row(&gettext("Created"), &format::full_date(created));
        }

        if let Some(original) = item.trash_orig_path() {
            let folder = original.parent().unwrap_or(&original);
            self.add_row(
                &gettext("Original location"),
                &gio::File::for_path(folder).parse_name(),
            );
            if let Some(deleted) = item.trash_deletion_date() {
                let deleted =
                    glib::DateTime::from_iso8601(&deleted, Some(&glib::TimeZone::local()))
                        .map(|date| format::full_date(date.to_unix()))
                        .unwrap_or_else(|_| deleted.to_string());
                self.add_row(&gettext("Date deleted"), &deleted);
            }
        } else if let Some(parent) = file.parent() {
            self.add_row(&gettext("Location"), &parent.parse_name());
        }
        if let Some(target) = item.symlink_target() {
            self.add_row(&gettext("Link target"), &target.to_string_lossy());
        }

        if !item.is_dir() && item.size() > 0 && gio::content_type_is_a(&content_type, "text/plain")
        {
            let name = item.display_name().to_string();
            self.spawn(clone!(
                #[weak(rename_to = details)]
                self,
                async move {
                    if let Some(text) = read_text(&file).await {
                        details.show_text(&text, &name, &content_type);
                    }
                }
            ));
        }
    }

    fn show_selection(&self, items: &[FileItem]) {
        let imp = self.imp();
        let count = items.len() as u32;
        let folders = items.iter().filter(|i| i.is_dir()).count() as u32;
        let files = count - folders;

        imp.preview_image
            .set_icon(&gio::ThemedIcon::from_names(&["folder-documents", "folder"]).upcast());
        imp.name_label.set_label(
            &ngettext("{} item selected", "{} items selected", count)
                .replace("{}", &count.to_string()),
        );
        let folders_text =
            ngettext("{} folder", "{} folders", folders).replace("{}", &folders.to_string());
        let files_text = ngettext("{} file", "{} files", files).replace("{}", &files.to_string());
        let summary = match (folders, files) {
            (0, _) => files_text,
            (_, 0) => folders_text,
            // Translators: "2 folders, 5 files"
            _ => gettext("{folders}, {files}")
                .replace("{folders}", &folders_text)
                .replace("{files}", &files_text),
        };
        imp.type_label.set_label(&summary);

        if files > 0 {
            let total: u64 = items.iter().filter(|i| !i.is_dir()).map(|i| i.size()).sum();
            let title = if folders > 0 {
                gettext("Size of files")
            } else {
                gettext("Size")
            };
            self.add_row(&title, &format::size(total));
        }
        let parent = items[0].target_file().parent();
        if let Some(parent) = parent
            && items[1..]
                .iter()
                .all(|i| i.target_file().parent().is_some_and(|p| p.equal(&parent)))
        {
            self.add_row(&gettext("Location"), &parent.parse_name());
        }
    }

    // ── Text preview ───────────────────────────────────────────────────────

    fn setup_text_view(&self) {
        let imp = self.imp();
        let buffer = sourceview5::Buffer::new(None::<&gtk::TextTagTable>);
        buffer.set_highlight_matching_brackets(false);
        let view = sourceview5::View::builder()
            .buffer(&buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(10)
            .right_margin(10)
            .build();
        imp.text_scroller.set_child(Some(&view));
        imp.text_view.set(view).unwrap();

        // Highlighting follows the light/dark style.
        let style = adw::StyleManager::default();
        let handler = style.connect_dark_notify(clone!(
            #[weak(rename_to = details)]
            self,
            move |_| details.update_text_scheme()
        ));
        imp.style_handler.replace(Some(handler));
        self.update_text_scheme();
    }

    fn update_text_scheme(&self) {
        let Some(buffer) = self.text_buffer() else {
            return;
        };
        let name = if adw::StyleManager::default().is_dark() {
            "Adwaita-dark"
        } else {
            "Adwaita"
        };
        let scheme = sourceview5::StyleSchemeManager::default().scheme(name);
        buffer.set_style_scheme(scheme.as_ref());
    }

    fn text_buffer(&self) -> Option<sourceview5::Buffer> {
        self.imp()
            .text_view
            .get()
            .and_then(|view| view.buffer().downcast().ok())
    }

    fn show_text(&self, text: &str, name: &str, content_type: &str) {
        let imp = self.imp();
        let Some(buffer) = self.text_buffer() else {
            return;
        };
        let language =
            sourceview5::LanguageManager::default().guess_language(Some(name), Some(content_type));
        buffer.set_language(language.as_ref());
        buffer.set_text(text);
        buffer.place_cursor(&buffer.start_iter());
        imp.text_scroller.vadjustment().set_value(0.0);
        imp.text_scroller.hadjustment().set_value(0.0);
        imp.preview_stack.set_visible_child_name("text");
    }

    // ── Helpers ────────────────────────────────────────────────────────────

    fn add_row(&self, title: &str, value: &str) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(value)
            .use_markup(false)
            .subtitle_selectable(true)
            .css_classes(["property"])
            .build();
        self.imp().properties_list.append(&row);
        row
    }

    /// A row filled in later (see [`fill_row`]); hidden until then.
    fn add_pending_row(&self, title: &str) -> adw::ActionRow {
        let row = self.add_row(title, "");
        row.set_visible(false);
        row
    }

    fn spawn(&self, future: impl std::future::Future<Output = ()> + 'static) {
        self.imp()
            .tasks
            .borrow_mut()
            .push(glib::spawn_future_local(future));
    }

    fn cancel_tasks(&self) {
        for task in self.imp().tasks.take() {
            task.abort();
        }
    }
}

fn fill_row(row: &adw::ActionRow, value: &str) {
    row.set_subtitle(value);
    row.set_visible(true);
    if let Some(list) = row.parent() {
        list.set_visible(true);
    }
}

/// The full-colour version of a location's icon.
fn folder_icon(location: &gio::File) -> gio::Icon {
    let symbolic = places::icon_name_for(location);
    let name = symbolic.trim_end_matches("-symbolic");
    gio::ThemedIcon::from_names(&[name, "folder"]).upcast()
}

async fn count_children(folder: &gio::File) -> Result<u32, glib::Error> {
    let enumerator = folder
        .enumerate_children_future(
            gio::FILE_ATTRIBUTE_STANDARD_NAME,
            gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS,
            glib::Priority::LOW,
        )
        .await?;
    let mut count = 0;
    loop {
        let infos = enumerator
            .next_files_future(1000, glib::Priority::LOW)
            .await?;
        if infos.is_empty() {
            return Ok(count);
        }
        count += infos.len() as u32;
    }
}

/// The beginning of a text file.
pub async fn read_text(file: &gio::File) -> Option<String> {
    let stream = file.read_future(glib::Priority::LOW).await.ok()?;
    let (buffer, read, _) = stream
        .read_all_future(vec![0u8; TEXT_PREVIEW_BYTES], glib::Priority::LOW)
        .await
        .ok()?;
    decode_preview(&buffer[..read])
}

/// Decodes the start of a text file for previewing: `None` for binary
/// data; a character cut at the end is dropped; other invalid UTF-8 (e.g.
/// Latin-1) is replaced.
pub fn decode_preview(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.contains(&0) {
        return None;
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Some(text.to_owned()),
        Err(err) if err.error_len().is_none() => {
            Some(String::from_utf8_lossy(&bytes[..err.valid_up_to()]).into_owned())
        }
        Err(_) => Some(String::from_utf8_lossy(bytes).into_owned()),
    }
}
