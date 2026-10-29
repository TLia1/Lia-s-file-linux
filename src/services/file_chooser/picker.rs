use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gdk, gio, glib};

use super::{Choice, FileFilter, Options, Request, folder_from_options, selection_uris};
use crate::widgets::{address_bar::AddressBar, folder_pane::FolderPane, sidebar::Sidebar};

pub(super) fn present(
    app: &adw::Application,
    title: &str,
    parent: &str,
    options: &Options,
    request: Rc<Request>,
) -> Result<adw::Window, &'static str> {
    let directory = boolean(options, "directory", false)?;
    let multiple = boolean(options, "multiple", false)?;
    let modal = boolean(options, "modal", true)?;
    let filters = option::<Vec<FileFilter>>(options, "filters")?.unwrap_or_default();
    let current = option::<FileFilter>(options, "current_filter")?;
    let choices = option::<Vec<Choice>>(options, "choices")?.unwrap_or_default();
    let accept_label = option::<String>(options, "accept_label")?
        .unwrap_or_else(|| gettext(if directory { "_Select Folder" } else { "_Open" }));
    let mut filters = filters;
    if let Some(current) = &current
        && !filters.contains(current)
    {
        filters.push(current.clone());
    }
    if filters
        .iter()
        .any(|(_, patterns)| patterns.iter().any(|(kind, _)| *kind > 1))
    {
        return Err("Unknown file filter type");
    }
    let window = adw::Window::builder()
        .application(app)
        .title(title)
        .default_width(980)
        .default_height(660)
        .modal(modal)
        .build();
    let pane = FolderPane::new();
    pane.set_picker_mode(directory, multiple);
    pane.set_hexpand(true);
    pane.set_vexpand(true);
    let address: AddressBar = glib::Object::new();
    address.set_hexpand(true);
    let sidebar: Sidebar = glib::Object::new();
    sidebar.set_size_request(200, -1);
    let accept = gtk::Button::with_mnemonic(&accept_label);
    accept.add_css_class("suggested-action");
    accept.set_sensitive(false);
    let cancel = gtk::Button::with_mnemonic(&gettext("_Cancel"));
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .build();
    header.pack_start(&cancel);
    header.pack_end(&accept);
    header.set_title_widget(Some(&adw::WindowTitle::new(title, "Lia Files")));
    let navigation = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    for (icon, tooltip, action) in [
        (
            "go-previous-symbolic",
            "Back",
            FolderPane::go_back as fn(&FolderPane),
        ),
        ("go-next-symbolic", "Forward", FolderPane::go_forward),
        ("go-up-symbolic", "Up", FolderPane::go_up),
    ] {
        let button = gtk::Button::from_icon_name(icon);
        button.set_tooltip_text(Some(&gettext(tooltip)));
        button.connect_clicked(glib::clone!(
            #[weak]
            pane,
            move |_| action(&pane)
        ));
        navigation.append(&button);
    }
    navigation.append(&address);
    let hidden = gtk::ToggleButton::with_label(&gettext("Hidden Files"));
    hidden.set_active(crate::util::settings().boolean("show-hidden"));
    hidden.connect_toggled(glib::clone!(
        #[weak]
        pane,
        move |button| pane.set_show_hidden(button.is_active())
    ));
    navigation.append(&hidden);
    let search = gtk::SearchEntry::builder()
        .placeholder_text(gettext("Search this folder"))
        .hexpand(true)
        .build();
    search.connect_search_changed(glib::clone!(
        #[weak]
        pane,
        move |entry| pane.set_search_text(entry.text())
    ));
    let browser = gtk::Box::new(gtk::Orientation::Vertical, 8);
    browser.set_margin_start(12);
    browser.set_margin_end(12);
    browser.append(&navigation);
    browser.append(&search);
    browser.append(&pane);
    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.set_vexpand(true);
    body.append(&sidebar);
    body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    body.append(&browser);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.append(&header);
    content.append(&body);
    let footer = gtk::Box::new(gtk::Orientation::Vertical, 6);
    footer.set_margin_start(12);
    footer.set_margin_end(12);
    footer.set_margin_bottom(12);
    let filter_dropdown = if filters.is_empty() || directory {
        None
    } else {
        let labels: Vec<&str> = filters.iter().map(|(name, _)| name.as_str()).collect();
        let dropdown = gtk::DropDown::from_strings(&labels);
        dropdown.update_property(&[gtk::accessible::Property::Label(&gettext("File type"))]);
        let index = current
            .as_ref()
            .and_then(|f| filters.iter().position(|candidate| candidate == f))
            .unwrap_or(0);
        dropdown.set_selected(index as u32);
        pane.set_picker_filter(Some(gtk::FileFilter::from_gvariant(
            &filters[index].to_variant(),
        )));
        let serialized = filters.clone();
        dropdown.connect_selected_notify(glib::clone!(
            #[weak]
            pane,
            move |dropdown| {
                let filter = serialized
                    .get(dropdown.selected() as usize)
                    .map(|filter| gtk::FileFilter::from_gvariant(&filter.to_variant()));
                pane.set_picker_filter(filter);
            }
        ));
        footer.append(&dropdown);
        Some(dropdown)
    };
    let mut choice_values: Vec<(String, Box<dyn Fn() -> String>)> = Vec::new();
    for (id, label, values, initial) in choices {
        if values.is_empty() {
            let check = gtk::CheckButton::with_label(&label);
            check.set_active(initial == "true");
            footer.append(&check);
            choice_values.push((id, Box::new(move || check.is_active().to_string())));
        } else {
            let labels: Vec<&str> = values.iter().map(|(_, label)| label.as_str()).collect();
            let dropdown = gtk::DropDown::from_strings(&labels);
            dropdown.set_selected(
                values
                    .iter()
                    .position(|(id, _)| *id == initial)
                    .unwrap_or(0) as u32,
            );
            dropdown.update_property(&[gtk::accessible::Property::Label(&label)]);
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
            row.append(&gtk::Label::new(Some(&label)));
            row.append(&dropdown);
            footer.append(&row);
            choice_values.push((
                id,
                Box::new(move || values[dropdown.selected() as usize].0.clone()),
            ));
        }
    }
    let error = gtk::Label::builder().wrap(true).xalign(0.0).build();
    error.add_css_class("error");
    footer.append(&error);
    content.append(&footer);
    window.set_content(Some(&content));
    window.set_default_widget(Some(&accept));

    let submit: Rc<dyn Fn(Vec<gio::File>)> = Rc::new(glib::clone!(
        #[weak]
        error,
        #[strong]
        request,
        move |files: Vec<gio::File>| {
            match selection_uris(&files, directory, multiple) {
                Ok(uris) => {
                    let mut results = Options::new();
                    results.insert("uris".into(), uris.to_variant());
                    results.insert(
                        "choices".into(),
                        choice_values
                            .iter()
                            .map(|(id, value)| (id.clone(), value()))
                            .collect::<Vec<_>>()
                            .to_variant(),
                    );
                    if let Some(dropdown) = &filter_dropdown
                        && let Some(filter) = filters.get(dropdown.selected() as usize)
                    {
                        results.insert("current_filter".into(), filter.to_variant());
                    }
                    request.respond(0, results);
                }
                Err(message) => error.set_text(&gettext(message)),
            }
        }
    ));
    accept.connect_clicked(glib::clone!(
        #[weak]
        pane,
        #[strong]
        submit,
        move |_| {
            submit(selected(&pane, directory));
        }
    ));
    pane.connect_file_picked(glib::clone!(
        #[strong]
        submit,
        move |pane, file| {
            if !directory {
                submit(if multiple && pane.n_selected() > 1 {
                    pane.selected_files()
                } else {
                    vec![file.clone()]
                });
            }
        }
    ));
    cancel.connect_clicked(glib::clone!(
        #[strong]
        request,
        move |_| request.cancel()
    ));
    window.connect_close_request(glib::clone!(
        #[strong]
        request,
        move |_| {
            request.cancel();
            glib::Propagation::Stop
        }
    ));
    let update = Rc::new(glib::clone!(
        #[weak]
        accept,
        #[weak]
        error,
        move |pane: &FolderPane| {
            error.set_text("");
            let items = pane.selected_items();
            let valid = if items.is_empty() {
                directory && pane.location().is_some_and(|f| f.path().is_some())
            } else {
                (multiple || items.len() == 1)
                    && items
                        .iter()
                        .all(|i| i.is_dir() == directory && i.target_file().path().is_some())
            };
            accept.set_sensitive(valid && !pane.loading() && !pane.is_this_pc());
        }
    ));
    pane.connect_selection_changed(glib::clone!(
        #[strong]
        update,
        move |pane| update(pane)
    ));
    pane.connect_loading_notify(glib::clone!(
        #[strong]
        update,
        move |pane| update(pane)
    ));
    pane.connect_location_notify(glib::clone!(
        #[weak]
        address,
        #[weak]
        sidebar,
        #[weak]
        search,
        #[strong]
        update,
        move |pane| {
            if let Some(location) = pane.location() {
                address.set_location(&location);
                sidebar.set_location(Some(&location));
            }
            search.set_text("");
            update(pane);
        }
    ));
    address.connect_navigate(glib::clone!(
        #[weak]
        pane,
        move |_, file| pane.navigate_to(file)
    ));
    sidebar.connect_open_location(glib::clone!(
        #[weak]
        pane,
        move |_, file, _| pane.navigate_to(file)
    ));
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        address,
        #[weak]
        pane,
        #[weak]
        hidden,
        #[strong]
        request,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, modifiers| {
            if key == gdk::Key::Escape && !address.is_editing() {
                request.cancel();
            } else if modifiers.contains(gdk::ModifierType::CONTROL_MASK) && key == gdk::Key::l {
                address.start_editing();
            } else if modifiers.contains(gdk::ModifierType::CONTROL_MASK) && key == gdk::Key::h {
                hidden.set_active(!hidden.is_active());
            } else if modifiers.contains(gdk::ModifierType::ALT_MASK) && key == gdk::Key::Left {
                pane.go_back();
            } else {
                return glib::Propagation::Proceed;
            }
            glib::Propagation::Stop
        }
    ));
    window.add_controller(keys);
    let initial = folder_from_options(options)
        .filter(|folder| {
            folder.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
                == gio::FileType::Directory
        })
        .unwrap_or_else(|| gio::File::for_path(glib::home_dir()));
    pane.navigate_to(&initial);
    gtk::prelude::WidgetExt::realize(&window);
    set_parent(&window, parent);
    window.present();
    Ok(window)
}

fn selected(pane: &FolderPane, directory: bool) -> Vec<gio::File> {
    let files = pane.selected_files();
    if directory && files.is_empty() {
        pane.location().into_iter().collect()
    } else {
        files
    }
}

fn option<T: glib::variant::FromVariant>(
    options: &Options,
    key: &str,
) -> Result<Option<T>, &'static str> {
    options
        .get(key)
        .map(|v| v.get::<T>().ok_or("Incorrect option type"))
        .transpose()
}

fn boolean(options: &Options, key: &str, default: bool) -> Result<bool, &'static str> {
    Ok(option(options, key)?.unwrap_or(default))
}

fn set_parent(window: &adw::Window, parent: &str) {
    // GDK's Wayland-specific function is exported by GTK, but is not part of
    // the backend-independent gdk4 Rust crate. Check the surface type first.
    unsafe extern "C" {
        fn gdk_wayland_toplevel_set_transient_for_exported(
            toplevel: *mut gdk::ffi::GdkToplevel,
            handle: *const std::ffi::c_char,
        ) -> glib::ffi::gboolean;
    }
    if let Some(handle) = parent.strip_prefix("wayland:")
        && let Ok(handle) = std::ffi::CString::new(handle)
        && let Some(surface) = window.surface()
        && surface.type_().name() == "GdkWaylandToplevel"
    {
        // SAFETY: the live surface is a Wayland toplevel and handle is NUL terminated.
        unsafe {
            gdk_wayland_toplevel_set_transient_for_exported(
                surface.as_ptr() as *mut gdk::ffi::GdkToplevel,
                handle.as_ptr(),
            );
        }
    }
}
