//! The main window. For now it hosts a single `FolderPane`; tabs arrive in
//! phase 2. Window actions always act on the active pane.

use std::cell::OnceCell;

use adw::prelude::*;
use gtk::{gio, glib, glib::clone, subclass::prelude::*};

use crate::application::Application;
use crate::util::settings;
use crate::widgets::folder_pane::{self, FolderPane};

mod imp {
    use super::*;
    use adw::subclass::prelude::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/lia/Files/ui/window.ui")]
    pub struct Window {
        #[template_child]
        pub(super) toast_overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub(super) window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub(super) pane_bin: TemplateChild<adw::Bin>,
        pub(super) pane: OnceCell<FolderPane>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "LiaWindow";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
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
            obj.setup_pane();
            obj.setup_actions();
            obj.restore_state();
        }
    }

    impl WidgetImpl for Window {}

    impl WindowImpl for Window {
        fn close_request(&self) -> glib::Propagation {
            self.obj().save_state();
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
    pub fn new(app: &Application, location: &gio::File) -> Self {
        let window: Self = glib::Object::builder().property("application", app).build();
        window.pane().navigate_to(location);
        window.pane().grab_focus();
        window
    }

    /// The pane that window actions act on.
    pub fn pane(&self) -> &FolderPane {
        self.imp().pane.get().unwrap()
    }

    pub fn show_toast(&self, message: &str) {
        let toast = adw::Toast::builder().title(message).timeout(3).build();
        self.imp().toast_overlay.add_toast(toast);
    }

    fn setup_pane(&self) {
        let imp = self.imp();
        let pane = FolderPane::new();
        imp.pane_bin.set_child(Some(&pane));
        pane.connect_location_notify(clone!(
            #[weak(rename_to = window)]
            self,
            move |pane| {
                if let Some(location) = pane.location() {
                    let name = folder_pane::display_name(&location);
                    window.set_title(Some(&name));
                    window.imp().window_title.set_title(&name);
                }
                window.update_navigation_actions();
            }
        ));
        imp.pane.set(pane).unwrap();
    }

    fn setup_actions(&self) {
        let pane_action = |name: &str, f: fn(&FolderPane)| {
            gio::ActionEntry::builder(name)
                .activate(move |window: &Self, _, _| f(window.pane()))
                .build()
        };
        self.add_action_entries([
            pane_action("back", FolderPane::go_back),
            pane_action("forward", FolderPane::go_forward),
            pane_action("up", FolderPane::go_up),
            pane_action("reload", FolderPane::reload),
            pane_action("focus-location", FolderPane::focus_location),
            pane_action("focus-search", FolderPane::focus_search),
            pane_action("copy-path", FolderPane::copy_path),
            pane_action("zoom-in", |pane| pane.zoom(1)),
            pane_action("zoom-out", |pane| pane.zoom(-1)),
            pane_action("zoom-reset", FolderPane::zoom_reset),
            pane_action("open", |pane| pane.open_selection(false)),
            pane_action("open-with", |pane| pane.open_selection(true)),
            pane_action("home", |pane| {
                pane.navigate_to(&gio::File::for_path(glib::home_dir()))
            }),
        ]);

        // Stateful actions backed directly by GSettings.
        let settings = settings();
        for key in ["show-hidden", "sort-folders-first", "default-view"] {
            self.add_action(&settings.create_action(key));
        }
    }

    fn update_navigation_actions(&self) {
        let pane = self.pane();
        let set = |name: &str, enabled: bool| {
            if let Some(action) = self.lookup_action(name).and_downcast::<gio::SimpleAction>() {
                action.set_enabled(enabled);
            }
        };
        set("back", pane.can_go_back());
        set("forward", pane.can_go_forward());
        set("up", pane.can_go_up());
    }

    fn restore_state(&self) {
        let settings = settings();
        self.set_default_size(settings.int("window-width"), settings.int("window-height"));
        if settings.boolean("is-maximized") {
            self.maximize();
        }
    }

    fn save_state(&self) {
        let settings = settings();
        let (width, height) = self.default_size();
        let _ = settings.set_int("window-width", width);
        let _ = settings.set_int("window-height", height);
        let _ = settings.set_boolean("is-maximized", self.is_maximized());
    }
}
