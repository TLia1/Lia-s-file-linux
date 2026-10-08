//! The Open With dialog: pick an app for the selected files, optionally
//! making it the default for their type.

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib, glib::clone};

use crate::widgets::app_chooser::AppChooser;

/// Asks which app opens `files`. `content_types` are their types; "always
/// use" is offered when they're all the same.
pub fn present(parent: &impl IsA<gtk::Widget>, files: Vec<gio::File>, content_types: &[String]) {
    let Some(content_type) = content_types.first().cloned() else {
        return;
    };
    let same_type = content_types.iter().all(|t| *t == content_type);
    let description = gio::content_type_get_description(&content_type);

    let chooser = AppChooser::new(&content_type);
    let always = adw::SwitchRow::builder()
        .title(gettext("Always use for “{}”").replace("{}", &description))
        .use_markup(false)
        .visible(same_type)
        .build();
    let options = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .visible(same_type)
        .build();
    options.append(&always);

    let name = files
        .first()
        .map(crate::widgets::folder_pane::display_name)
        .unwrap_or_default();
    let heading = if files.len() == 1 {
        gettext("Choose an app to open “{}”").replace("{}", &name)
    } else {
        gettext("Choose an app to open {} files").replace("{}", &files.len().to_string())
    };
    let label = gtk::Label::builder()
        .label(heading)
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(18)
        .margin_start(12)
        .margin_end(12)
        .build();
    content.append(&label);
    content.append(&chooser);
    content.append(&options);
    let clamp = adw::Clamp::builder()
        .maximum_size(480)
        .child(&content)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&clamp)
        .build();

    let open_button = gtk::Button::builder()
        .label(gettext("_Open"))
        .use_underline(true)
        .css_classes(["suggested-action"])
        .build();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    let cancel_button = gtk::Button::with_mnemonic(&gettext("_Cancel"));
    header.pack_start(&cancel_button);
    header.pack_end(&open_button);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&scroller));

    let dialog = adw::Dialog::builder()
        .title(gettext("Open With"))
        .content_width(440)
        .content_height(560)
        .child(&toolbar)
        .default_widget(&open_button)
        .build();

    let parent = parent.as_ref().clone();
    let open = clone!(
        #[weak]
        dialog,
        #[weak]
        always,
        #[weak]
        parent,
        move |app: &gio::AppInfo| {
            if always.is_active()
                && always.is_visible()
                && let Err(err) = app.set_as_default_for_type(&content_type)
            {
                tracing::warn!("cannot set the default app for {content_type}: {err}");
            }
            launch(&parent, app, &files);
            dialog.close();
        }
    );
    chooser.connect_app_activated(clone!(
        #[strong]
        open,
        move |_, app| open(app)
    ));
    open_button.connect_clicked(clone!(
        #[weak]
        chooser,
        move |_| {
            if let Some(app) = chooser.selected_app() {
                open(&app);
            }
        }
    ));
    chooser.connect_selection_changed(clone!(
        #[weak]
        open_button,
        move |chooser| open_button.set_sensitive(chooser.selected_app().is_some())
    ));
    cancel_button.connect_clicked(clone!(
        #[weak]
        dialog,
        move |_| {
            dialog.close();
        }
    ));
    dialog.present(Some(&parent));
}

/// Opens `files` with `app`, reporting failures in a toast.
pub fn launch(parent: &gtk::Widget, app: &gio::AppInfo, files: &[gio::File]) {
    let uris: Vec<String> = files.iter().map(|f| f.uri().into()).collect();
    let context = parent.display().app_launch_context();
    let app = app.clone();
    let parent = parent.downgrade();
    glib::spawn_future_local(async move {
        let uris: Vec<&str> = uris.iter().map(String::as_str).collect();
        if let Err(err) = app.launch_uris_future(&uris, Some(&context)).await {
            tracing::warn!("cannot open with {}: {err}", app.name());
            if let Some(window) = parent
                .upgrade()
                .and_then(|w| w.root())
                .and_downcast::<crate::window::Window>()
            {
                window.show_toast(
                    &gettext("Could not start “{}”").replace("{}", &app.display_name()),
                );
            }
        }
    });
}
