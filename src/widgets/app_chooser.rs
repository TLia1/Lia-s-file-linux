//! `AppChooser`: the list of applications that can open a content type,
//! default app first, then the other recommended and registered ones. "Show
//! Other Apps" adds every remaining installed app.
//!
//! A click selects an app, a double-click (or Enter) emits `app-activated`.
//! Used by the Open With dialog and the Properties dialog.

use std::cell::RefCell;
use std::sync::OnceLock;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib, glib::clone, subclass::prelude::*};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct AppChooser {
        pub(super) list_box: gtk::ListBox,
        pub(super) content_type: RefCell<String>,
        /// One entry per app row, in order.
        pub(super) apps: RefCell<Vec<gio::AppInfo>>,
        pub(super) show_others_row: RefCell<Option<adw::ButtonRow>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AppChooser {
        const NAME: &'static str = "LiaAppChooser";
        type Type = super::AppChooser;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for AppChooser {
        fn signals() -> &'static [glib::subclass::Signal] {
            static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    glib::subclass::Signal::builder("app-activated")
                        .param_types([gio::AppInfo::static_type()])
                        .build(),
                    glib::subclass::Signal::builder("selection-changed").build(),
                ]
            })
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().setup();
        }

        fn dispose(&self) {
            self.list_box.unparent();
        }
    }

    impl WidgetImpl for AppChooser {}
}

glib::wrapper! {
    pub struct AppChooser(ObjectSubclass<imp::AppChooser>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for AppChooser {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl AppChooser {
    pub fn new(content_type: &str) -> Self {
        let chooser = Self::default();
        chooser.set_content_type(content_type);
        chooser
    }

    fn setup(&self) {
        let list_box = &self.imp().list_box;
        list_box.set_parent(self);
        list_box.add_css_class("boxed-list");
        list_box.set_selection_mode(gtk::SelectionMode::Single);
        list_box.set_activate_on_single_click(false);
        list_box.update_property(&[gtk::accessible::Property::Label(&gettext("Apps"))]);
        list_box.connect_row_selected(clone!(
            #[weak(rename_to = chooser)]
            self,
            move |_, _| chooser.emit_by_name::<()>("selection-changed", &[])
        ));
        list_box.connect_row_activated(clone!(
            #[weak(rename_to = chooser)]
            self,
            move |_, row| {
                let app = chooser
                    .imp()
                    .apps
                    .borrow()
                    .get(row.index() as usize)
                    .cloned();
                if let Some(app) = app {
                    chooser.emit_by_name::<()>("app-activated", &[&app]);
                }
            }
        ));
    }

    pub fn content_type(&self) -> String {
        self.imp().content_type.borrow().clone()
    }

    /// Lists the apps for `content_type`, selecting the default one.
    pub fn set_content_type(&self, content_type: &str) {
        self.imp().content_type.replace(content_type.to_owned());
        self.reload();
    }

    /// Rebuilds the list (e.g. after the default app changed).
    pub fn reload(&self) {
        let imp = self.imp();
        let content_type = self.content_type();
        let default = self.default_app();
        let mut apps: Vec<gio::AppInfo> = Vec::new();
        let mut push = |app: gio::AppInfo| {
            if !apps.iter().any(|a| a.equal(&app)) {
                apps.push(app);
            }
        };
        if let Some(default) = &default {
            push(default.clone());
        }
        gio::AppInfo::recommended_for_type(&content_type)
            .into_iter()
            .chain(gio::AppInfo::all_for_type(&content_type))
            .filter(|app| app.should_show())
            .for_each(&mut push);

        imp.list_box.remove_all();
        imp.apps.replace(Vec::new());
        imp.show_others_row.replace(None);
        if apps.is_empty() {
            // Nothing registered for the type: offer everything.
            self.append_apps(other_apps(&[]));
        } else {
            self.append_apps(apps);
            let row = adw::ButtonRow::builder()
                .title(gettext("Show Other Apps"))
                .start_icon_name("view-more-horizontal-symbolic")
                .build();
            row.connect_activated(clone!(
                #[weak(rename_to = chooser)]
                self,
                move |_| chooser.show_other_apps()
            ));
            imp.list_box.append(&row);
            imp.show_others_row.replace(Some(row));
        }
        if let Some(first) = imp.list_box.row_at_index(0) {
            imp.list_box.select_row(Some(&first));
        }
    }

    /// The app currently set as default for the content type.
    pub fn default_app(&self) -> Option<gio::AppInfo> {
        gio::AppInfo::default_for_type(&self.content_type(), false)
    }

    pub fn selected_app(&self) -> Option<gio::AppInfo> {
        let imp = self.imp();
        let row = imp.list_box.selected_row()?;
        imp.apps.borrow().get(row.index() as usize).cloned()
    }

    pub fn connect_app_activated<F: Fn(&Self, &gio::AppInfo) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "app-activated",
            false,
            glib::closure_local!(move |chooser: &Self, app: &gio::AppInfo| f(chooser, app)),
        )
    }

    pub fn connect_selection_changed<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "selection-changed",
            false,
            glib::closure_local!(move |chooser: &Self| f(chooser)),
        )
    }

    fn show_other_apps(&self) {
        let imp = self.imp();
        if let Some(row) = imp.show_others_row.take() {
            imp.list_box.remove(&row);
        }
        let others = other_apps(&imp.apps.borrow());
        let first_new = imp.apps.borrow().len() as i32;
        self.append_apps(others);
        if let Some(row) = imp.list_box.row_at_index(first_new) {
            row.grab_focus();
        }
    }

    /// Appends rows for `apps`, before the "Show Other Apps" row.
    fn append_apps(&self, apps: Vec<gio::AppInfo>) {
        let imp = self.imp();
        let default = self.default_app();
        for app in apps {
            let row = adw::ActionRow::builder()
                .title(app.display_name())
                .use_markup(false)
                .build();
            if default.as_ref().is_some_and(|d| d.equal(&app)) {
                row.set_subtitle(&gettext("Default app"));
            }
            let icon = app
                .icon()
                .unwrap_or_else(|| gio::ThemedIcon::new("application-x-executable").upcast());
            let image = gtk::Image::builder()
                .gicon(&icon)
                .pixel_size(32)
                .css_classes(["lowres-icon"])
                .build();
            row.add_prefix(&image);
            let position = imp.apps.borrow().len() as i32;
            imp.list_box.insert(&row, position);
            imp.apps.borrow_mut().push(app);
        }
    }
}

/// Installed apps not in `listed`, by name.
fn other_apps(listed: &[gio::AppInfo]) -> Vec<gio::AppInfo> {
    let mut apps: Vec<gio::AppInfo> = gio::AppInfo::all()
        .into_iter()
        .filter(|app| app.should_show() && !listed.iter().any(|l| l.equal(app)))
        .collect();
    apps.sort_by_cached_key(|app| app.display_name().to_lowercase());
    apps
}
