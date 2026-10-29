//! Theme-neutral appearance settings and a watched, ordered user stylesheet.
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use sourceview5::prelude::*;

use super::settings;

thread_local! {
    static TEXT_VIEWS: RefCell<Vec<glib::WeakRef<sourceview5::View>>> = const { RefCell::new(Vec::new()) };
}

pub struct Appearance {
    directory: PathBuf,
    display: gdk::Display,
    preferences: gtk::CssProvider,
    user: RefCell<gtk::CssProvider>,
    motion: gtk::CssProvider,
    monitors: RefCell<Vec<gio::FileMonitor>>,
    pending: RefCell<Option<glib::SourceId>>,
}

impl Appearance {
    pub fn new() -> Rc<Self> {
        let this = Rc::new(Self {
            directory: glib::user_config_dir().join("lia-files"),
            display: gdk::Display::default().expect("GTK display"),
            preferences: gtk::CssProvider::new(),
            user: RefCell::new(gtk::CssProvider::new()),
            motion: gtk::CssProvider::new(),
            monitors: RefCell::default(),
            pending: RefCell::default(),
        });
        gtk::style_context_add_provider_for_display(
            &this.display,
            &this.preferences,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        gtk::style_context_add_provider_for_display(
            &this.display,
            &this.motion,
            gtk::STYLE_PROVIDER_PRIORITY_USER + 1,
        );
        this.apply_preferences();
        let weak = Rc::downgrade(&this);
        settings().connect_changed(None, move |_, key| {
            if [
                "corners",
                "density",
                "selection-style",
                "window-opacity",
                "accent-override",
            ]
            .contains(&key)
                && let Some(this) = weak.upgrade()
            {
                this.apply_preferences();
            }
        });
        if let Some(gtk_settings) = gtk::Settings::default() {
            let weak = Rc::downgrade(&this);
            gtk_settings.connect_gtk_enable_animations_notify(move |_| {
                if let Some(this) = weak.upgrade() {
                    this.apply_motion();
                }
            });
        }
        let weak = Rc::downgrade(&this);
        adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(this) = weak.upgrade() {
                this.apply_preferences();
            }
        });
        this.apply_motion();
        if let Err(error) = std::fs::create_dir_all(&this.directory) {
            tracing::warn!("cannot create theme directory: {error}");
        }
        // Also watch the parent so atomic replacement/removal of the directory
        // re-establishes the directory monitor when it returns.
        this.watch();
        this.reload();
        this
    }

    fn watch(self: &Rc<Self>) {
        self.monitors.borrow_mut().clear();
        for path in [Some(self.directory.as_path()), self.directory.parent()]
            .into_iter()
            .flatten()
        {
            let Ok(monitor) = gio::File::for_path(path)
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            else {
                continue;
            };
            let weak = Rc::downgrade(self);
            let directory = self.directory.clone();
            monitor.connect_changed(move |_, file, other, _| {
                let relevant = |file: &gio::File| {
                    file.path()
                        .is_some_and(|p| p == directory || p.parent() == Some(directory.as_path()))
                };
                if !relevant(file) && !other.is_some_and(relevant) {
                    return;
                }
                if let Some(this) = weak.upgrade() {
                    this.schedule_reload();
                }
            });
            self.monitors.borrow_mut().push(monitor);
        }
    }

    fn schedule_reload(self: &Rc<Self>) {
        if let Some(source) = self.pending.take() {
            source.remove();
        }
        let weak = Rc::downgrade(self);
        self.pending.replace(Some(glib::timeout_add_local_once(
            Duration::from_millis(100),
            move || {
                if let Some(this) = weak.upgrade() {
                    this.pending.take();
                    this.reload();
                    this.watch();
                }
            },
        )));
    }

    pub fn reload(&self) {
        let css = match read_stylesheets(&self.directory) {
            Ok(css) => css,
            Err(error) => {
                tracing::warn!("cannot read user style: {error}");
                return;
            }
        };
        // Parse before replacing: an incomplete or invalid editor write leaves
        // the last usable theme in place. Deleting all CSS restores the defaults.
        let candidate = gtk::CssProvider::new();
        let invalid = Rc::new(Cell::new(false));
        let failed = invalid.clone();
        let handler = candidate.connect_parsing_error(move |_, _, error| {
            if error.kind::<gtk::CssParserError>().is_some() {
                failed.set(true);
            }
            tracing::warn!("user stylesheet: {error}");
        });
        candidate.set_prefers_color_scheme(color_scheme());
        candidate.load_from_string(&css);
        candidate.disconnect(handler);
        if invalid.get() {
            return;
        }
        gtk::style_context_remove_provider_for_display(&self.display, &*self.user.borrow());
        gtk::style_context_add_provider_for_display(
            &self.display,
            &candidate,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        self.user.replace(candidate);
        sourceview5::StyleSchemeManager::default().force_rescan();
        TEXT_VIEWS.with(|views| {
            views.borrow_mut().retain(|view| {
                if let Some(view) = view.upgrade() {
                    update_text_scheme(&view);
                    true
                } else {
                    false
                }
            })
        });
    }

    fn apply_preferences(&self) {
        self.preferences.set_prefers_color_scheme(color_scheme());
        self.user.borrow().set_prefers_color_scheme(color_scheme());
        let s = settings();
        let mut css = String::from(":root {\n");
        if s.string("corners") == "sharp" {
            for name in ["small", "large", "popover", "chip", "bar", "drive-bar"] {
                css.push_str(&format!("--lia-radius-{name}: 0px;\n"));
            }
            css.push_str("--lia-radius: 0px;\n");
        }
        let (height, padding) = match s.string("density").as_str() {
            "compact" => (28, 8),
            "comfortable" => (40, 16),
            _ => (32, 12),
        };
        css.push_str(&format!("--lia-row-height: {height}px; --lia-control-height: {height}px; --lia-cell-padding: {padding}px;\n"));
        match s.string("selection-style").as_str() {
            "fill" => css.push_str("--lia-selection-marker: 0px;\n"),
            "outline" => css.push_str("--lia-selection-marker: 0px; --lia-selection-outline: 2px; --lia-selected-bg: transparent; --lia-selected-hover-bg: var(--lia-hover-bg);\n"),
            _ => (),
        }
        css.push_str(&format!(
            "--lia-window-opacity: {};\n",
            s.double("window-opacity")
        ));
        let accent = s.string("accent-override");
        if valid_accent(&accent) {
            // libadwaita computes an accessible foreground for the current mode.
            let rgba = gdk::RGBA::parse(&accent).expect("validated hex colour");
            let ink = adw::rgba_to_standalone(&rgba, adw::StyleManager::default().is_dark());
            let foreground =
                if 0.2126 * rgba.red() + 0.7152 * rgba.green() + 0.0722 * rgba.blue() > 0.55 {
                    "black"
                } else {
                    "white"
                };
            css.push_str(&format!("--accent-bg-color: {accent}; --accent-color: {ink}; --accent-fg-color: {foreground};\n"));
        }
        css.push('}');
        if s.string("corners") == "sharp" {
            css.push_str("window.lia-window.csd { border-radius: 0; }");
        }
        self.preferences.load_from_string(&css);
    }

    fn apply_motion(&self) {
        let enabled = gtk::Settings::default().is_none_or(|s| s.is_gtk_enable_animations());
        self.motion.load_from_string(if enabled { "" } else {
            ":root { --lia-fast: 0ms; --lia-normal: 0ms; --lia-slow: 0ms; } * { animation: none; transition: none; }"
        });
    }
}

impl Drop for Appearance {
    fn drop(&mut self) {
        if let Some(source) = self.pending.take() {
            source.remove();
        }
        for provider in [&self.preferences, self.user.get_mut(), &self.motion] {
            gtk::style_context_remove_provider_for_display(&self.display, provider);
        }
    }
}

fn color_scheme() -> gtk::InterfaceColorScheme {
    if adw::StyleManager::default().is_dark() {
        gtk::InterfaceColorScheme::Dark
    } else {
        gtk::InterfaceColorScheme::Light
    }
}

pub fn valid_accent(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

pub fn read_stylesheets(directory: &Path) -> std::io::Result<String> {
    let entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(error),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "css") && path.is_file() {
            paths.push(path);
        }
    }
    paths.sort();
    let mut css = String::new();
    for path in paths {
        css.push_str(&std::fs::read_to_string(path)?);
        css.push('\n');
    }
    Ok(css)
}

/// Keep both preview implementations on the same settings and fallbacks.
pub fn follow_text_style(view: &sourceview5::View) {
    TEXT_VIEWS.with(|views| views.borrow_mut().push(view.downgrade()));
    view.add_css_class("lia-text-preview");
    update_text_scheme(view);
    let handler = adw::StyleManager::default().connect_dark_notify(glib::clone!(
        #[weak]
        view,
        move |_| update_text_scheme(&view)
    ));
    let handler = RefCell::new(Some(handler));
    view.connect_destroy(move |_| {
        if let Some(handler) = handler.take() {
            adw::StyleManager::default().disconnect(handler);
        }
    });
    let s = settings();
    let handler = s.connect_changed(
        None,
        glib::clone!(
            #[weak]
            view,
            move |_, key| {
                if key.starts_with("text-scheme-") {
                    update_text_scheme(&view);
                }
            }
        ),
    );
    let handler = RefCell::new(Some(handler));
    view.connect_destroy(move |_| {
        if let Some(handler) = handler.take() {
            settings().disconnect(handler);
        }
    });

    // Read the desktop's monospace font without assuming the schema exists.
    if let Some(schema) = gio::SettingsSchemaSource::default()
        .and_then(|s| s.lookup("org.gnome.desktop.interface", true))
        .filter(|s| s.has_key("monospace-font-name"))
    {
        let desktop = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
        let provider = gtk::CssProvider::new();
        #[allow(deprecated)] // GTK has no replacement for a widget-local font provider.
        view.style_context()
            .add_provider(&provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
        let apply = move |s: &gio::Settings| {
            let font = gtk::pango::FontDescription::from_string(&s.string("monospace-font-name"));
            let family = font.family().unwrap_or_else(|| "monospace".into());
            let family = family
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace(['\n', '\r'], " ");
            let size = font.size() as f64 / gtk::pango::SCALE as f64;
            let unit = if font.is_size_absolute() { "px" } else { "pt" };
            provider.load_from_string(&format!(
                "textview.lia-text-preview {{ font-family: \"{family}\"; font-size: {}{unit}; }}",
                if size > 0.0 { size } else { 10.0 }
            ));
        };
        apply(&desktop);
        let handler = desktop.connect_changed(Some("monospace-font-name"), move |s, _| apply(s));
        let handler = RefCell::new(Some(handler));
        view.connect_destroy(move |_| {
            if let Some(handler) = handler.take() {
                desktop.disconnect(handler);
            }
        });
    }
}

fn update_text_scheme(view: &sourceview5::View) {
    let Some(buffer) = view.buffer().downcast::<sourceview5::Buffer>().ok() else {
        return;
    };
    let dark = adw::StyleManager::default().is_dark();
    let name = settings().string(if dark {
        "text-scheme-dark"
    } else {
        "text-scheme-light"
    });
    let manager = sourceview5::StyleSchemeManager::default();
    let scheme = manager
        .scheme(&name)
        .or_else(|| manager.scheme(if dark { "Adwaita-dark" } else { "Adwaita" }));
    buffer.set_style_scheme(scheme.as_ref());
}
