//! `Application`: app-level actions, accelerators and window creation.

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib, subclass::prelude::*};

use crate::config;
use crate::dialogs;
use crate::model::view_settings;
use crate::services::file_manager1::{self, Request};
use crate::services::file_ops::queue;
use crate::services::recovery;
use crate::services::undo::undo_manager;
use crate::util::settings;
use crate::window::Window;

mod imp {
    use super::*;
    use adw::subclass::prelude::*;

    #[derive(Default)]
    pub struct Application {
        /// The session is restored only for the first window.
        pub(super) started: std::cell::Cell<bool>,
        pub(super) appearance:
            std::cell::OnceCell<std::rc::Rc<crate::util::appearance::Appearance>>,
    }

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
            fix_font_rendering();
            follow_color_scheme();
            self.appearance
                .set(crate::util::appearance::Appearance::new())
                .ok();
            recover_interrupted_operations();
            app.setup_actions();
            app.setup_accels();
            app.export_file_manager1();
        }

        fn activate(&self) {
            self.parent_activate();
            let app = self.obj();
            let first = !self.started.replace(true);
            if first && app.restore_session() {
                return;
            }
            let location = if first && settings().string("start-location") == "recent" {
                gio::File::for_uri("recent:///")
            } else {
                gio::File::for_path(glib::home_dir())
            };
            app.new_window(&location);
        }

        fn shutdown(&self) {
            view_settings::flush();
            undo_manager().flush();
            self.parent_shutdown();
        }

        fn open(&self, files: &[gio::File], _hint: &str) {
            self.started.set(true);
            // All folders open as tabs of one window.
            let mut files = files.iter();
            let Some(first) = files.next() else {
                return;
            };
            let window = self.obj().new_window(first);
            for file in files {
                window.open_tab(file, false);
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
        let window = Window::new(self);
        window.open_tab(location, true);
        self.present_window(&window);
        window
    }

    fn present_window(&self, window: &Window) {
        window.present();
        if self.windows().len() == 1 {
            window.add_tick_callback(|_, _| {
                tracing::debug!("first frame {:?} after start", crate::since_startup());
                glib::ControlFlow::Break
            });
        }
        if let Ok(path) = std::env::var("LIA_SCREENSHOT") {
            self.screenshot_and_quit(window, path);
        }
    }

    /// Answers "Show in folder" and similar requests from other apps. Only
    /// the primary instance runs `startup`, so only it owns the name.
    fn export_file_manager1(&self) {
        let Some(connection) = self.dbus_connection() else {
            return;
        };
        file_manager1::export(
            &connection,
            glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |call| app.show_requested(call)
            ),
        );
    }

    /// Opens a window for a `org.freedesktop.FileManager1` call.
    fn show_requested(&self, call: file_manager1::Call) {
        if call.files.is_empty() {
            return;
        }
        let window = Window::new(self);
        match call.request {
            Request::ShowFolders => {
                for (i, folder) in call.files.iter().enumerate() {
                    window.open_tab(folder, i == 0);
                }
            }
            Request::ShowItems | Request::ShowItemProperties => {
                for (i, (folder, items)) in file_manager1::group_by_parent(&call.files)
                    .into_iter()
                    .enumerate()
                {
                    window.open_tab(&folder, i == 0).select_files(items, false);
                }
            }
        }
        if !call.startup_id.is_empty() {
            window.set_startup_id(&call.startup_id);
        }
        self.present_window(&window);
        if call.request == Request::ShowItemProperties {
            dialogs::properties::PropertiesDialog::present_for(&window, call.files);
        }
    }

    /// Reopens the tabs saved by the last closed window. Returns false if
    /// disabled or there is nothing to restore.
    fn restore_session(&self) -> bool {
        let settings = settings();
        if !settings.boolean("restore-session") {
            return false;
        }
        let uris = settings.strv("session-tabs");
        if uris.is_empty() {
            return false;
        }
        let window = Window::new(self);
        let active = settings.int("session-active-tab").max(0) as usize;
        let mut selected = None;
        for (i, uri) in uris.iter().enumerate() {
            let pane = window.open_tab(&gio::File::for_uri(uri), i == 0);
            if i == active {
                selected = Some(pane);
            }
        }
        if let Some(pane) = selected {
            window.select_pane(&pane);
        }
        self.present_window(&window);
        true
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
            .and_then(|w| w.active_pane())
            .and_then(|pane| pane.location())
            .unwrap_or_else(|| gio::File::for_path(glib::home_dir()))
    }

    fn setup_actions(&self) {
        self.add_action_entries([
            gio::ActionEntry::builder("quit")
                .activate(|app: &Self, _, _| {
                    // The active window's tabs become the session; the other
                    // windows must not overwrite it while closing.
                    if let Some(active) = app.active_window().and_downcast::<Window>() {
                        active.save_session();
                    }
                    for window in app.windows() {
                        if let Some(window) = window.downcast_ref::<Window>() {
                            window.skip_session_save();
                        }
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
            gio::ActionEntry::builder("preferences")
                .activate(|app: &Self, _, _| {
                    dialogs::preferences::present(app.active_window().as_ref())
                })
                .build(),
            gio::ActionEntry::builder("about")
                .activate(|app: &Self, _, _| app.show_about())
                .build(),
            // Undo/redo act on the file system, so they're app-wide. Their
            // shortcuts are set per window (see `Window`), after text
            // fields had a chance to handle them.
            gio::ActionEntry::builder("undo")
                .activate(|app: &Self, _, _| {
                    queue().undo(app.active_window().as_ref());
                })
                .build(),
            gio::ActionEntry::builder("redo")
                .activate(|app: &Self, _, _| {
                    queue().redo(app.active_window().as_ref());
                })
                .build(),
        ]);
        if cfg!(debug_assertions) || std::env::var_os("GTK_DEBUG").is_some() {
            let action = gio::SimpleAction::new("reload-style", None);
            action.connect_activate(glib::clone!(
                #[weak(rename_to = app)]
                self,
                move |_, _| {
                    if let Some(appearance) = app.imp().appearance.get() {
                        appearance.reload();
                    }
                }
            ));
            self.add_action(&action);
            self.set_accels_for_action("app.reload-style", &["<Control><Shift>r"]);
        }
        self.add_action(&settings().create_action("restore-session"));
        let undo = undo_manager();
        for (name, property) in [("undo", "can-undo"), ("redo", "can-redo")] {
            if let Some(action) = self.lookup_action(name) {
                undo.bind_property(property, &action, "enabled")
                    .sync_create()
                    .build();
            }
        }
    }

    fn setup_accels(&self) {
        let accels: &[(&str, &[&str])] = &[
            ("app.quit", &["<Control>q"]),
            ("app.new-window", &["<Control>n"]),
            ("app.shortcuts", &["<Control>question"]),
            ("app.preferences", &["<Control>comma"]),
            ("window.close", &["<Control><Shift>w"]),
            ("win.new-tab", &["<Control>t"]),
            ("win.close-tab", &["<Control>w"]),
            ("win.toggle-sidebar", &["F9"]),
            ("win.toggle-details-pane", &["<Alt><Shift>p", "<Alt>p"]),
            ("win.invert-selection", &["<Control><Shift>i"]),
            ("win.new-folder", &["<Control><Shift>n"]),
            ("win.back", &["<Alt>Left", "Back"]),
            ("win.forward", &["<Alt>Right", "Forward"]),
            ("win.up", &["<Alt>Up"]),
            ("win.home", &["<Alt>Home"]),
            ("win.reload", &["F5", "<Control>r"]),
            ("win.focus-location", &["<Control>l", "<Alt>d"]),
            ("win.focus-search", &["<Control>f", "<Control>e"]),
            ("win.show-hidden", &["<Control>h"]),
            ("win.view-mode::details", &["<Control>1"]),
            ("win.view-mode::grid", &["<Control>2"]),
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

/// Cleans up what operations interrupted by a crash left behind.
fn recover_interrupted_operations() {
    let spawned = std::thread::Builder::new()
        .name("lia-recovery".into())
        .spawn(|| recovery::recover(&recovery::recovery_dir()));
    if let Err(err) = spawned {
        tracing::warn!("cannot start the recovery thread: {err}");
    }
}

/// Applies the `color-scheme` setting, now and when it changes.
pub(crate) fn follow_color_scheme() {
    let apply = |settings: &gio::Settings| {
        let scheme = match settings.string("color-scheme").as_str() {
            "light" => adw::ColorScheme::ForceLight,
            "dark" => adw::ColorScheme::ForceDark,
            _ => adw::ColorScheme::Default,
        };
        adw::StyleManager::default().set_color_scheme(scheme);
    };
    let settings = settings();
    apply(&settings);
    settings.connect_changed(Some("color-scheme"), move |settings, _| apply(settings));
}

/// Works around a GTK bug at fractional scales (e.g. 115 %): in the default
/// "automatic" font rendering, glyph outlines are hinted but metrics are not,
/// so the top pixel row of many glyphs is cut off. Manual rendering with
/// hinted metrics keeps the two consistent while still following the user's
/// hinting and antialiasing preferences.
pub(crate) fn fix_font_rendering() {
    let Some(settings) = gtk::Settings::default() else {
        return;
    };
    settings.set_gtk_font_rendering(gtk::FontRendering::Manual);
    settings.set_gtk_hint_font_metrics(true);
}
