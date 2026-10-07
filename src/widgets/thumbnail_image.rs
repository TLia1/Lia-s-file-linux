//! `ThumbnailImage`: a file's icon, replaced by its thumbnail (faded in)
//! once the thumbnail service has one.

use std::cell::{Cell, OnceCell, RefCell};

use gtk::{gio, glib, glib::clone, prelude::*, subclass::prelude::*};

use crate::model::FileItem;
use crate::services::thumbnails::{ThumbSize, ThumbnailRequest, thumbnail_service};

mod imp {
    use super::*;

    #[derive(Default, glib::Properties)]
    #[properties(wrapper_type = super::ThumbnailImage)]
    pub struct ThumbnailImage {
        pub(super) image: OnceCell<gtk::Image>,
        /// The size of the icon / thumbnail, in logical pixels.
        #[property(get, set = Self::set_pixel_size, default = 48)]
        pub(super) pixel_size: Cell<i32>,
        pub(super) item: RefCell<Option<FileItem>>,
        pub(super) handler: RefCell<Option<glib::SignalHandlerId>>,
        pub(super) request: RefCell<Option<ThumbnailRequest>>,
        /// Set while requesting: a thumbnail that arrives synchronously
        /// was in memory and appears without fading in.
        pub(super) requesting: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ThumbnailImage {
        const NAME: &'static str = "LiaThumbnailImage";
        type Type = super::ThumbnailImage;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_css_name("thumbnail-image");
            klass.set_accessible_role(gtk::AccessibleRole::Img);
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for ThumbnailImage {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            self.pixel_size.set(48);
            let image = gtk::Image::builder().pixel_size(48).build();
            image.set_parent(&*obj);
            self.image.set(image).unwrap();
            // Moving to a HiDPI monitor needs a larger thumbnail.
            obj.connect_scale_factor_notify(|image| image.request());
        }

        fn dispose(&self) {
            self.obj().set_item(None);
            if let Some(image) = self.image.get() {
                image.unparent();
            }
        }
    }

    impl WidgetImpl for ThumbnailImage {}

    impl ThumbnailImage {
        fn set_pixel_size(&self, size: i32) {
            if self.pixel_size.replace(size) == size {
                return;
            }
            self.image.get().unwrap().set_pixel_size(size);
            self.obj().request();
        }
    }
}

glib::wrapper! {
    pub struct ThumbnailImage(ObjectSubclass<imp::ThumbnailImage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ThumbnailImage {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ThumbnailImage {
    pub fn new(pixel_size: i32) -> Self {
        glib::Object::builder()
            .property("pixel-size", pixel_size)
            .build()
    }

    /// Shows `item` (or nothing). Its thumbnail is requested, and cancelled
    /// if another item is shown before it arrives.
    pub fn set_item(&self, item: Option<&FileItem>) {
        let imp = self.imp();
        if imp.item.borrow().as_ref() == item {
            return;
        }
        imp.request.take();
        if let (Some(old), Some(handler)) = (imp.item.take(), imp.handler.take()) {
            old.disconnect(handler);
        }
        let image = imp.image.get().unwrap();
        image.remove_css_class("fade-in");

        let Some(item) = item else {
            image.clear();
            return;
        };
        imp.item.replace(Some(item.clone()));
        let handler = item.connect_notify_local(
            Some("thumbnail"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| this.update(!this.imp().requesting.get())
            ),
        );
        imp.handler.replace(Some(handler));
        self.update(false);
        self.request();
    }

    /// Shows a plain icon (e.g. for a folder or a selection).
    pub fn set_icon(&self, icon: &gio::Icon) {
        self.set_item(None);
        let image = self.imp().image.get().unwrap();
        image.remove_css_class("thumbnail");
        image.set_from_gicon(icon);
    }

    /// Whether a thumbnail (rather than an icon) is shown.
    pub fn has_thumbnail(&self) -> bool {
        self.imp()
            .item
            .borrow()
            .as_ref()
            .is_some_and(|item| item.thumbnail().is_some())
    }

    fn request(&self) {
        let imp = self.imp();
        let Some(item) = imp.item.borrow().clone() else {
            return;
        };
        let pixels = imp.pixel_size.get() * self.scale_factor().max(1);
        imp.requesting.set(true);
        let request = thumbnail_service().request(&item, ThumbSize::for_pixels(pixels));
        imp.requesting.set(false);
        imp.request.replace(request);
    }

    /// Shows the item's thumbnail if it has one, its icon otherwise.
    fn update(&self, animate: bool) {
        let imp = self.imp();
        let image = imp.image.get().unwrap();
        let Some(item) = imp.item.borrow().clone() else {
            return;
        };
        match item.thumbnail() {
            Some(texture) => {
                image.set_paintable(Some(&texture));
                image.add_css_class("thumbnail");
                if animate {
                    image.add_css_class("fade-in");
                }
            }
            None => {
                image.remove_css_class("thumbnail");
                image.remove_css_class("fade-in");
                image.set_from_gicon(
                    &item
                        .icon()
                        .unwrap_or_else(|| gio::ThemedIcon::new("text-x-generic").upcast()),
                );
            }
        }
    }
}
