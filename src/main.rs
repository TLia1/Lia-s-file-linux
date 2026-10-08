use gtk::glib;
use gtk::prelude::*;
use lia_files::application::Application;

fn main() -> glib::ExitCode {
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

    Application::new().run()
}
