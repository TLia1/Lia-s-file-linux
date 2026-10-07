//! `ConflictDialog`: asked when a copied/moved item already exists at the
//! destination — Replace (or Merge, for folders) · Skip · Keep both, with
//! a side-by-side comparison and "Do this for all conflicts".

use gettextrs::gettext;
use gtk::{gdk, gio, glib, prelude::*, subclass::prelude::*};

use crate::services::file_ops::{Conflict, FileSummary};
use crate::util::format;

mod imp {
    use super::*;
    use adw::subclass::prelude::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/lia/Files/ui/conflict-dialog.ui")]
    pub struct ConflictDialog {
        #[template_child]
        pub(super) target_image: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) target_size: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) target_date: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) source_heading: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) source_image: TemplateChild<gtk::Image>,
        #[template_child]
        pub(super) source_size: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) source_date: TemplateChild<gtk::Label>,
        #[template_child]
        pub(super) apply_all: TemplateChild<gtk::CheckButton>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ConflictDialog {
        const NAME: &'static str = "LiaConflictDialog";
        type Type = super::ConflictDialog;
        type ParentType = adw::AlertDialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ConflictDialog {}
    impl WidgetImpl for ConflictDialog {}
    impl AdwDialogImpl for ConflictDialog {}
    impl AdwAlertDialogImpl for ConflictDialog {}
}

glib::wrapper! {
    pub struct ConflictDialog(ObjectSubclass<imp::ConflictDialog>)
        @extends adw::AlertDialog, adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl ConflictDialog {
    /// `many`: the job may have more conflicts, so offer "apply to all".
    pub fn new(conflict: &Conflict, many: bool) -> Self {
        let dialog: Self = glib::Object::new();
        dialog.fill(conflict, many);
        dialog
    }

    pub fn apply_to_all(&self) -> bool {
        let check = &self.imp().apply_all;
        check.is_visible() && check.is_active()
    }

    fn fill(&self, conflict: &Conflict, many: bool) {
        use adw::prelude::*;
        let imp = self.imp();
        let name = &conflict.target.name;
        let folder = &conflict.folder_name;

        let (heading, body) = if conflict.is_merge {
            (
                gettext("Merge Folder “{}”?").replace("{}", name),
                gettext(
                    "A folder with this name already exists in “{}”. Merging adds the new \
                     items to it, and asks before replacing any file.",
                )
                .replace("{}", folder),
            )
        } else if conflict.can_replace {
            (
                gettext("Replace “{}”?").replace("{}", name),
                gettext(
                    "A file with this name already exists in “{}”. Replacing it overwrites \
                     its contents.",
                )
                .replace("{}", folder),
            )
        } else {
            (
                gettext("“{}” Already Exists").replace("{}", name),
                gettext("An item with this name already exists in “{}”.").replace("{}", folder),
            )
        };
        self.set_heading(Some(&heading));
        self.set_body(&body);

        if conflict.is_merge {
            self.set_response_label("replace", &gettext("_Merge"));
            self.set_response_appearance("replace", adw::ResponseAppearance::Suggested);
            imp.source_heading.set_label(&gettext("Merge With"));
        } else if !conflict.can_replace {
            self.remove_response("replace");
            self.set_default_response(Some("keep"));
            imp.source_heading.set_label(&gettext("New Item"));
        }
        imp.apply_all.set_visible(many);

        let newer = conflict.source.modified > conflict.target.modified;
        fill_side(
            &conflict.target,
            &imp.target_image,
            &imp.target_size,
            &imp.target_date,
            !newer && conflict.source.modified != conflict.target.modified,
        );
        fill_side(
            &conflict.source,
            &imp.source_image,
            &imp.source_size,
            &imp.source_date,
            newer,
        );
    }
}

fn fill_side(
    summary: &FileSummary,
    image: &gtk::Image,
    size: &gtk::Label,
    date: &gtk::Label,
    newer: bool,
) {
    // A cached thumbnail when there is one, otherwise the type's icon.
    let texture = summary
        .thumbnail
        .as_ref()
        .and_then(|path| gdk::Texture::from_filename(path).ok());
    match texture {
        Some(texture) => image.set_paintable(Some(&texture)),
        None => {
            let icon = if summary.is_dir {
                gio::ThemedIcon::new("folder").upcast()
            } else {
                summary
                    .content_type
                    .as_deref()
                    .map(gio::content_type_get_icon)
                    .unwrap_or_else(|| gio::ThemedIcon::new("text-x-generic").upcast())
            };
            image.set_from_gicon(&icon);
        }
    }
    image.set_tooltip_text(Some(&summary.name));

    size.set_label(&if summary.is_dir {
        gettext("Folder")
    } else {
        format::size(summary.size)
    });
    let mut text = format::date(summary.modified);
    if newer {
        // Translators: appended to the more recent of two dates
        text = gettext("{} · Newer").replace("{}", &text);
    }
    date.set_label(&text);
}
