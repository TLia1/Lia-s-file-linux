use gtk::glib;
use gtk::prelude::*;
use lia_files::application::Application;

fn main() -> glib::ExitCode {
    let portal = std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "--file-chooser-portal");
    if portal {
        // Do not query the frontend while it waits for this backend to start.
        gtk::disable_portals();
        // SAFETY: before GTK, logging or any application thread is started.
        unsafe { std::env::set_var("ADW_DISABLE_PORTAL", "1") };
    }
    lia_files::mark_startup();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("LIA_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn,lia_files=info")),
        )
        .init();

    lia_files::init_i18n();
    lia_files::register_resources();
    glib::set_application_name("Lia Files");

    if portal {
        lia_files::services::file_chooser::run()
    } else {
        Application::new().run()
    }
}
