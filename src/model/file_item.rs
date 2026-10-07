//! `FileItem`: one GObject per file, wrapping a `gio::File` and the
//! `gio::FileInfo` queried for it.
//!
//! Items are immutable snapshots: when a file changes on disk, the
//! `DirectoryModel` replaces its item, which also lets the sort model
//! re-position it. Only the thumbnail is filled in later.
//!
//! The read-only properties (`display-name`, `size`, `modified`, `is-dir`,
//! `type-description`) exist for GTK's key-based sorters and for bindings.

use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::OnceLock;

use gtk::{gdk, gio, glib, prelude::*, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct FileItem {
        pub(super) file: OnceCell<gio::File>,
        pub(super) info: OnceCell<gio::FileInfo>,
        /// Filled in asynchronously by the thumbnail service.
        pub(super) thumbnail: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FileItem {
        const NAME: &'static str = "LiaFileItem";
        type Type = super::FileItem;
    }

    impl ObjectImpl for FileItem {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: OnceLock<Vec<glib::ParamSpec>> = OnceLock::new();
            PROPERTIES.get_or_init(|| {
                vec![
                    glib::ParamSpecString::builder("display-name")
                        .read_only()
                        .build(),
                    glib::ParamSpecUInt64::builder("size").read_only().build(),
                    glib::ParamSpecInt64::builder("modified")
                        .read_only()
                        .build(),
                    glib::ParamSpecBoolean::builder("is-dir")
                        .read_only()
                        .build(),
                    glib::ParamSpecString::builder("type-description")
                        .read_only()
                        .build(),
                    glib::ParamSpecObject::builder::<gdk::Texture>("thumbnail").build(),
                ]
            })
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "display-name" => obj.display_name().to_value(),
                // Folders count as 0 so that sorting by size groups them.
                "size" => (if obj.is_dir() { 0 } else { obj.size() }).to_value(),
                "modified" => obj.modified().to_value(),
                "is-dir" => obj.is_dir().to_value(),
                "type-description" => obj.type_description().to_value(),
                "thumbnail" => self.thumbnail.borrow().to_value(),
                name => unreachable!("unknown property {name}"),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "thumbnail" => {
                    self.thumbnail.replace(value.get().unwrap());
                }
                name => unreachable!("property {name} is read-only"),
            }
        }
    }
}

glib::wrapper! {
    pub struct FileItem(ObjectSubclass<imp::FileItem>);
}

impl FileItem {
    pub fn new(file: gio::File, info: gio::FileInfo) -> Self {
        let item: Self = glib::Object::new();
        let imp = item.imp();
        imp.file.set(file).unwrap();
        imp.info.set(info).unwrap();
        item
    }

    pub fn file(&self) -> &gio::File {
        self.imp().file.get().unwrap()
    }

    pub fn info(&self) -> &gio::FileInfo {
        self.imp().info.get().unwrap()
    }

    /// The on-disk name (the model's key).
    pub fn name(&self) -> PathBuf {
        self.info().name()
    }

    pub fn display_name(&self) -> glib::GString {
        self.info().display_name()
    }

    pub fn is_dir(&self) -> bool {
        matches!(
            self.info().file_type(),
            gio::FileType::Directory | gio::FileType::Mountable
        )
    }

    pub fn is_hidden(&self) -> bool {
        let info = self.info();
        info.is_hidden() || info.is_backup()
    }

    pub fn size(&self) -> u64 {
        self.info().size().max(0) as u64
    }

    /// Modification time as a UNIX timestamp (0 if unknown).
    pub fn modified(&self) -> i64 {
        self.info()
            .attribute_uint64(gio::FILE_ATTRIBUTE_TIME_MODIFIED)
            .try_into()
            .unwrap_or(0)
    }

    /// Creation time as a UNIX timestamp, if the file system records it.
    pub fn created(&self) -> Option<i64> {
        let info = self.info();
        info.has_attribute(gio::FILE_ATTRIBUTE_TIME_CREATED)
            .then(|| info.attribute_uint64(gio::FILE_ATTRIBUTE_TIME_CREATED))
            .and_then(|time| time.try_into().ok())
    }

    /// Where a symbolic link points.
    pub fn symlink_target(&self) -> Option<PathBuf> {
        let info = self.info();
        info.is_symlink().then(|| info.symlink_target()).flatten()
    }

    pub fn content_type(&self) -> Option<glib::GString> {
        self.info().content_type()
    }

    /// Human readable type, e.g. "Markdown document", "Folder".
    pub fn type_description(&self) -> String {
        thread_local! {
            static CACHE: RefCell<HashMap<glib::GString, String>> = RefCell::default();
        }
        let Some(content_type) = self.content_type() else {
            return String::new();
        };
        CACHE.with(|cache| {
            cache
                .borrow_mut()
                .entry(content_type.clone())
                .or_insert_with(|| gio::content_type_get_description(&content_type).into())
                .clone()
        })
    }

    pub fn icon(&self) -> Option<gio::Icon> {
        self.info().icon()
    }

    pub fn thumbnail(&self) -> Option<gdk::Texture> {
        self.imp().thumbnail.borrow().clone()
    }

    pub fn set_thumbnail(&self, texture: Option<&gdk::Texture>) {
        self.set_property("thumbnail", texture);
    }

    /// The file that operations (copy, drag…) act on: the target of a
    /// shortcut entry (in `recent://`, `network://`), or the file itself.
    pub fn target_file(&self) -> gio::File {
        self.target_uri()
            .map_or_else(|| self.file().clone(), |uri| gio::File::for_uri(&uri))
    }

    /// For items in the trash: where they were deleted from.
    pub fn trash_orig_path(&self) -> Option<PathBuf> {
        self.info()
            .attribute_byte_string(gio::FILE_ATTRIBUTE_TRASH_ORIG_PATH)
            .map(|path| PathBuf::from(path.as_str()))
    }

    /// For items in the trash: when they were deleted (ISO 8601, local time).
    pub fn trash_deletion_date(&self) -> Option<glib::GString> {
        self.info()
            .attribute_string(gio::FILE_ATTRIBUTE_TRASH_DELETION_DATE)
    }

    /// For shortcut entries (e.g. in `network://`), the location they point to.
    pub fn target_uri(&self) -> Option<glib::GString> {
        self.info()
            .attribute_string(gio::FILE_ATTRIBUTE_STANDARD_TARGET_URI)
    }
}
