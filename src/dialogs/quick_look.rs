//! Quick Look: Space shows a large preview of the selected item in a
//! floating dialog — the image itself, the beginning of a text file, or a
//! large thumbnail (or icon) for other files. The arrow keys move to the
//! previous / next item of the folder, Space or Escape closes, Enter opens.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gdk, gio, glib, glib::clone};
use sourceview5::prelude::*;

use crate::model::FileItem;
use crate::util::format;
use crate::widgets::details_pane;
use crate::widgets::folder_pane::FolderPane;
use crate::widgets::thumbnail_image::ThumbnailImage;

/// Images larger than this are shown as thumbnails (decoding would take
/// too long for a quick look).
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

struct QuickLook {
    dialog: adw::Dialog,
    title: adw::WindowTitle,
    stack: gtk::Stack,
    picture: gtk::Picture,
    thumbnail: ThumbnailImage,
    text: sourceview5::View,
    text_scroller: gtk::ScrolledWindow,
    item: RefCell<Option<FileItem>>,
    task: RefCell<Option<glib::JoinHandle<()>>>,
}

impl QuickLook {
    fn show(&self, item: &FileItem) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.item.replace(Some(item.clone()));
        self.title
            .set_title(&format::one_line(&item.display_name()));
        let kind = if item.is_dir() {
            gettext("Folder")
        } else {
            item.type_description()
        };
        self.title.set_subtitle(&if item.is_dir() {
            kind
        } else {
            format!("{kind} · {}", format::size(item.size()))
        });
        self.thumbnail.set_item(Some(item));
        self.stack.set_visible_child_name("thumbnail");

        let content_type = item.content_type().map(String::from).unwrap_or_default();
        let file = item.target_file();
        if item.is_dir() || item.size() == 0 {
            return;
        }
        if content_type.starts_with("image/")
            && item.size() <= MAX_IMAGE_BYTES
            && let Some(path) = file.path()
        {
            let picture = self.picture.clone();
            let stack = self.stack.clone();
            self.task.replace(Some(glib::spawn_future_local(async move {
                let texture = gio::spawn_blocking(move || gdk::Texture::from_filename(path)).await;
                if let Ok(Ok(texture)) = texture {
                    picture.set_paintable(Some(&texture));
                    stack.set_visible_child_name("image");
                }
            })));
        } else if gio::content_type_is_a(&content_type, "text/plain") {
            let name = item.display_name().to_string();
            let text = self.text.clone();
            let scroller = self.text_scroller.clone();
            let stack = self.stack.clone();
            self.task.replace(Some(glib::spawn_future_local(async move {
                let Some(contents) = details_pane::read_text(&file).await else {
                    return;
                };
                let Some(buffer) = text.buffer().downcast::<sourceview5::Buffer>().ok() else {
                    return;
                };
                let language = sourceview5::LanguageManager::default()
                    .guess_language(Some(&name), Some(&content_type));
                buffer.set_language(language.as_ref());
                buffer.set_text(&contents);
                buffer.place_cursor(&buffer.start_iter());
                scroller.vadjustment().set_value(0.0);
                stack.set_visible_child_name("text");
            })));
        }
    }
}

/// Shows the first selected item of `pane`.
pub fn present(pane: &FolderPane) {
    let Some(item) = pane.selected_items().into_iter().next() else {
        return;
    };

    let picture = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::ScaleDown)
        .can_shrink(true)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let thumbnail = ThumbnailImage::new(256);
    thumbnail.set_halign(gtk::Align::Center);
    thumbnail.set_valign(gtk::Align::Center);

    let buffer = sourceview5::Buffer::new(None::<&gtk::TextTagTable>);
    buffer.set_highlight_matching_brackets(false);
    let text = sourceview5::View::builder()
        .buffer(&buffer)
        .editable(false)
        .cursor_visible(false)
        .monospace(true)
        .show_line_numbers(true)
        .top_margin(8)
        .bottom_margin(8)
        .left_margin(8)
        .right_margin(8)
        .build();
    crate::util::appearance::follow_text_style(&text);
    let text_scroller = gtk::ScrolledWindow::builder().child(&text).build();

    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(150)
        .vexpand(true)
        .build();
    stack.add_named(&thumbnail, Some("thumbnail"));
    stack.add_named(&picture, Some("image"));
    stack.add_named(&text_scroller, Some("text"));

    let title = adw::WindowTitle::new("", "");
    let open_button = gtk::Button::builder()
        .label(gettext("_Open"))
        .use_underline(true)
        .tooltip_text(gettext("Open (Enter)"))
        .build();
    let previous = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .tooltip_text(gettext("Previous Item (Left)"))
        .build();
    let next = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .tooltip_text(gettext("Next Item (Right)"))
        .build();
    let arrows = gtk::Box::builder().css_classes(["linked"]).build();
    arrows.append(&previous);
    arrows.append(&next);
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    header.pack_start(&open_button);
    header.pack_start(&arrows);
    let toolbar = adw::ToolbarView::builder()
        .content(&stack)
        .css_classes(["quick-look"])
        .build();
    toolbar.add_top_bar(&header);

    let (width, height) = pane
        .root()
        .map_or((900, 640), |root| (root.width(), root.height()));
    let dialog = adw::Dialog::builder()
        .child(&toolbar)
        .content_width((width * 3 / 4).clamp(360, 1200))
        .content_height((height * 4 / 5).clamp(300, 900))
        .build();

    let look = Rc::new(QuickLook {
        dialog: dialog.clone(),
        title,
        stack,
        picture,
        thumbnail,
        text,
        text_scroller,
        item: RefCell::default(),
        task: RefCell::default(),
    });

    let step = Rc::new(clone!(
        #[weak]
        pane,
        #[weak]
        look,
        move |delta: i32| {
            if let Some(item) = pane.select_relative(delta) {
                look.show(&item);
            }
        }
    ));
    let open = Rc::new(clone!(
        #[weak]
        pane,
        #[weak]
        look,
        move || {
            let item = look.item.borrow().clone();
            if let Some(item) = item {
                look.dialog.close();
                pane.open_item(&item, false);
            }
        }
    ));
    previous.connect_clicked(clone!(
        #[strong]
        step,
        move |_| step(-1)
    ));
    next.connect_clicked(clone!(
        #[strong]
        step,
        move |_| step(1)
    ));
    open_button.connect_clicked(clone!(
        #[strong]
        open,
        move |_| open()
    ));

    // Captured before the text view or buttons see them.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed(clone!(
        #[weak]
        dialog,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, modifiers| {
            if !(modifiers & gtk::accelerator_get_default_mod_mask()).is_empty() {
                return glib::Propagation::Proceed;
            }
            match key {
                gdk::Key::space => {
                    dialog.close();
                }
                gdk::Key::Left | gdk::Key::Up => step(-1),
                gdk::Key::Right | gdk::Key::Down => step(1),
                gdk::Key::Return | gdk::Key::KP_Enter => open(),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        }
    ));
    dialog.add_controller(keys);
    // The dialog's state lives until it closes.
    let keep = RefCell::new(Some(look.clone()));
    dialog.connect_closed(clone!(
        #[weak]
        pane,
        move |_| {
            if let Some(look) = keep.take()
                && let Some(task) = look.task.take()
            {
                task.abort();
            }
            pane.grab_focus();
        }
    ));

    look.show(&item);
    dialog.present(Some(pane));
}
