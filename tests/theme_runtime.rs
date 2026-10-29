//! Display-dependent theme integration check, run in an isolated XDG profile:
//! Copy Penumbra's generated penumbra.css into the test XDG config/lia-files,
//! and set LIA_THEME_SCHEMES to its generated gtksourceview-5/styles directory.
//! LIA_THEME_LIGHT_CSS must point to a light-mode render of the Lia Files template.
//! GSETTINGS_BACKEND=memory XDG_CONFIG_HOME=/tmp/lia-theme-test LIA_THEME_TEST=1
//! cargo test --test theme_runtime -- --ignored --test-threads=1
use adw::prelude::*;
use gtk::{gdk, gio, glib};
use lia_files::{application::Application, util::settings};
use sourceview5::prelude::*;
use std::time::{Duration, Instant};

fn settle() {
    let until = Instant::now() + Duration::from_millis(250);
    while Instant::now() < until {
        while glib::MainContext::default().pending() {
            glib::MainContext::default().iteration(false);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
#[ignore = "requires a GTK display and an isolated XDG profile"]
fn watches_styles_and_preserves_the_last_valid_theme() {
    assert_eq!(std::env::var("LIA_THEME_TEST").as_deref(), Ok("1"));
    lia_files::register_resources();
    let app = Application::new();
    app.register(gio::Cancellable::NONE).unwrap();
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
    let directory = glib::user_config_dir().join("lia-files");
    std::fs::create_dir_all(&directory).unwrap();
    let css = directory.join("test.css");
    std::fs::write(
        &css,
        ":root { --accent-color: #123456; } .theme-probe { color: var(--accent-color); }",
    )
    .unwrap();
    let label = gtk::Label::builder()
        .label("Theme probe")
        .css_classes(["theme-probe"])
        .build();
    let density = gtk::CssProvider::new();
    density.load_from_string(".density-probe { min-height: var(--lia-row-height); }");
    gtk::style_context_add_provider_for_display(
        &gdk::Display::default().unwrap(),
        &density,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    label.add_css_class("density-probe");
    let window = gtk::Window::builder().child(&label).build();
    window.present();
    settle();
    assert_eq!(label.color(), gdk::RGBA::parse("#123456").unwrap());
    assert_eq!(label.measure(gtk::Orientation::Vertical, -1).0, 32);
    settings().set_string("density", "compact").unwrap();
    settle();
    assert_eq!(label.measure(gtk::Orientation::Vertical, -1).0, 28);
    settings().set_string("density", "comfortable").unwrap();
    settle();
    assert_eq!(label.measure(gtk::Orientation::Vertical, -1).0, 40);
    std::fs::write(
        directory.join("test.tmp"),
        ":root { --accent-color: #abcdef; } .theme-probe { color: var(--accent-color); }",
    )
    .unwrap();
    std::fs::rename(directory.join("test.tmp"), &css).unwrap();
    settle();
    assert_eq!(label.color(), gdk::RGBA::parse("#abcdef").unwrap());
    std::fs::write(&css, ".theme-probe { color: this-is-invalid; }").unwrap();
    settle();
    assert_eq!(label.color(), gdk::RGBA::parse("#abcdef").unwrap());
    settings().set_string("accent-override", "#ff0000").unwrap();
    settle();
    assert_eq!(label.color(), gdk::RGBA::parse("#abcdef").unwrap());
    std::fs::remove_file(&css).unwrap();
    settle();
    assert_ne!(label.color(), gdk::RGBA::parse("#abcdef").unwrap());
    window.close();

    // Capture the actual app's composite surfaces, not only a CSS probe.
    let actual = app.new_window(&gio::File::for_path(env!("CARGO_MANIFEST_DIR")));
    settings().set_string("corners", "sharp").unwrap();
    settle();
    let paintable = gtk::WidgetPaintable::new(Some(&actual));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, actual.width() as f64, actual.height() as f64);
    let node = snapshot.to_node().unwrap();
    let serialized = String::from_utf8(node.serialize().to_vec()).unwrap();
    assert!(
        serialized.contains("rgba(16,13,24,0.85)"),
        "dark glass surface missing"
    );
    assert!(
        serialized.contains("rgba(13,10,18,0.98)"),
        "file surface missing"
    );
    if let Ok(path) = std::env::var("LIA_THEME_NODE") {
        std::fs::write(path, node.serialize()).unwrap();
    }
    let manager = sourceview5::StyleSchemeManager::default();
    let schemes =
        std::env::var("LIA_THEME_SCHEMES").expect("path to Penumbra GtkSourceView schemes");
    manager.append_search_path(&schemes);
    let buffer = sourceview5::Buffer::new(None::<&gtk::TextTagTable>);
    let view = sourceview5::View::with_buffer(&buffer);
    lia_files::util::appearance::follow_text_style(&view);
    settings()
        .set_string("text-scheme-dark", "penumbra")
        .unwrap();
    settings()
        .set_string("text-scheme-light", "penumbra-light")
        .unwrap();
    assert_eq!(buffer.style_scheme().unwrap().id(), "penumbra");
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight);
    settle();
    assert_eq!(buffer.style_scheme().unwrap().id(), "penumbra-light");
    let light_css = std::env::var("LIA_THEME_LIGHT_CSS").expect("generated light Lia Files CSS");
    let theme_path = directory.join("penumbra.css");
    let original = std::fs::read_to_string(&theme_path).unwrap();
    std::fs::copy(light_css, &theme_path).unwrap();
    settle();
    assert_eq!(actual.color(), gdk::RGBA::parse("#251e2d").unwrap());
    let light_paintable = gtk::WidgetPaintable::new(Some(&actual));
    let light_snapshot = gtk::Snapshot::new();
    light_paintable.snapshot(
        &light_snapshot,
        actual.width() as f64,
        actual.height() as f64,
    );
    let light_node = light_snapshot.to_node().unwrap();
    let light_serialized = String::from_utf8(light_node.serialize().to_vec()).unwrap();
    assert!(
        light_serialized.contains("rgba(252,251,255,0.88)"),
        "light glass surface missing"
    );
    if let Ok(directory) = std::env::var("LIA_THEME_SCREENSHOTS") {
        let renderer = actual.renderer().unwrap();
        renderer
            .render_texture(&node, None)
            .save_to_png(std::path::Path::new(&directory).join("dark.png"))
            .unwrap();
        renderer
            .render_texture(&light_node, None)
            .save_to_png(std::path::Path::new(&directory).join("light.png"))
            .unwrap();
    }
    std::fs::write(theme_path, original).unwrap();
    settle();
    settings()
        .set_string("text-scheme-light", "missing-scheme")
        .unwrap();
    assert_eq!(buffer.style_scheme().unwrap().id(), "Adwaita");
    gtk::Settings::default()
        .unwrap()
        .set_gtk_enable_animations(false);
    settle();
    actual.close();
}
