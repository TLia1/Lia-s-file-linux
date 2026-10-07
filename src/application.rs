//! `Application`: app-level actions, accelerators and window creation.

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib, subclass::prelude::*};

use crate::config;
use crate::window::Window;

mod imp {
    use super::*;
    use adw::subclass::prelude::*;

    #[derive(Default)]
    pub struct Application;

    #[glib::object_subclass]
    impl ObjectSubclass for Application {
        const NAME: &'static str = "LiaApplication";
        type Type = super::Application;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for Application {}

    impl ApplicationImpl for Application {
        fn startup(&self) {
            self.parent_startup();
            let app = self.obj();
            app.setup_actions();
            app.setup_accels();
        }

        fn activate(&self) {
            self.parent_activate();
            self.obj()
                .new_window(&gio::File::for_path(glib::home_dir()));
        }

        fn open(&self, files: &[gio::File], _hint: &str) {
            for file in files {
                self.obj().new_window(file);
            }
        }
    }

    impl GtkApplicationImpl for Application {}
    impl AdwApplicationImpl for Application {}
}

glib::wrapper! {
    pub struct Application(ObjectSubclass<imp::Application>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Default for Application {
    fn default() -> Self {
        Self::new()
    }
}

impl Application {
    pub fn new() -> Self {
        glib::Object::builder()
            .property("application-id", config::APP_ID)
            .property("flags", gio::ApplicationFlags::HANDLES_OPEN)
            .property("resource-base-path", config::RESOURCE_BASE)
            .build()
    }

    pub fn new_window(&self, location: &gio::File) -> Window {
        let window = Window::new(self, location);
        window.present();
        if let Ok(path) = std::env::var("LIA_SCREENSHOT") {
            self.screenshot_and_quit(&window, path);
        }
        window
    }

    /// Developer aid: `LIA_SCREENSHOT=out.png [LIA_SCREENSHOT_DELAY=ms]`
    /// renders the first window to a PNG and quits. Used for visual checks
    /// without capturing the rest of the desktop.
    fn screenshot_and_quit(&self, window: &Window, path: String) {
        let delay = std::env::var("LIA_SCREENSHOT_DELAY")
            .ok()
            .and_then(|d| d.parse().ok())
            .unwrap_or(1500);
        glib::timeout_add_local_once(
            std::time::Duration::from_millis(delay),
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                #[weak]
                window,
                move || {
                    let paintable = gtk::WidgetPaintable::new(Some(&window));
                    let snapshot = gtk::Snapshot::new();
                    paintable.snapshot(&snapshot, window.width() as f64, window.height() as f64);
                    let renderer = window.renderer();
                    match (snapshot.to_node(), renderer) {
                        (Some(node), Some(renderer)) => {
                            let texture = renderer.render_texture(&node, None);
                            if let Err(err) = texture.save_to_png(&path) {
                                tracing::error!("cannot save screenshot: {err}");
                            }
                        }
                        _ => tracing::error!("nothing to screenshot"),
                    }
                    app.quit();
                }
            ),
        );
    }

    fn active_location(&self) -> gio::File {
        self.active_window()
            .and_downcast::<Window>()
            .and_then(|w| w.pane().location())
            .unwrap_or_else(|| gio::File::for_path(glib::home_dir()))
    }

    fn setup_actions(&self) {
        self.add_action_entries([
            gio::ActionEntry::builder("quit")
                .activate(|app: &Self, _, _| {
                    for window in app.windows() {
                        window.close();
                    }
                })
                .build(),
            gio::ActionEntry::builder("new-window")
                .activate(|app: &Self, _, _| {
                    app.new_window(&app.active_location());
                })
                .build(),
            gio::ActionEntry::builder("shortcuts")
                .activate(|app: &Self, _, _| app.show_shortcuts())
                .build(),
            gio::ActionEntry::builder("about")
                .activate(|app: &Self, _, _| app.show_about())
                .build(),
        ]);
    }

    fn setup_accels(&self) {
        let accels: &[(&str, &[&str])] = &[
            ("app.quit", &["<Control>q"]),
            ("app.new-window", &["<Control>n"]),
            ("app.shortcuts", &["<Control>question"]),
            ("window.close", &["<Control>w"]),
            ("win.back", &["<Alt>Left", "Back"]),
            ("win.forward", &["<Alt>Right", "Forward"]),
            ("win.up", &["<Alt>Up"]),
            ("win.home", &["<Alt>Home"]),
            ("win.reload", &["F5", "<Control>r"]),
            ("win.focus-location", &["<Control>l", "<Alt>d"]),
            ("win.focus-search", &["<Control>f", "<Control>e"]),
            ("win.show-hidden", &["<Control>h"]),
            ("win.default-view::details", &["<Control>1"]),
            ("win.default-view::grid", &["<Control>2"]),
            (
                "win.zoom-in",
                &["<Control>plus", "<Control>equal", "<Control>KP_Add"],
            ),
            ("win.zoom-out", &["<Control>minus", "<Control>KP_Subtract"]),
            ("win.zoom-reset", &["<Control>0", "<Control>KP_0"]),
            ("win.copy-path", &["<Control><Shift>c"]),
        ];
        for (action, keys) in accels {
            self.set_accels_for_action(action, keys);
        }
    }

    fn show_shortcuts(&self) {
        let builder =
            gtk::Builder::from_resource(&format!("{}/ui/shortcuts.ui", config::RESOURCE_BASE));
        let dialog: adw::ShortcutsDialog = builder.object("shortcuts_dialog").unwrap();
        dialog.present(self.active_window().as_ref());
    }

    fn show_about(&self) {
        let dialog = adw::AboutDialog::builder()
            .application_name(config::APP_NAME)
            .application_icon(config::APP_ID)
            .version(config::VERSION)
            .developer_name("Lia")
            .license_type(gtk::License::Gpl30)
            .comments(gettext("A clean, modern file manager"))
            .build();
        dialog.present(self.active_window().as_ref());
    }
}
