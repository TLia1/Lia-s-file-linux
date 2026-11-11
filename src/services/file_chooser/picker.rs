use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gdk, gio, glib};

use super::{
    Choice, FileFilter, Options, Request, current_file_from_options, folder_from_options,
    save_files_uris, save_uri, selection_uris,
};
use crate::services::file_ops::naming;
use crate::widgets::{address_bar::AddressBar, folder_pane::FolderPane, sidebar::Sidebar};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// OpenFile: existing files or folders.
    Open,
    /// SaveFile: a folder and a typed name.
    Save,
    /// SaveFiles: a folder that receives the suggested names.
    SaveFiles,
}

pub(super) fn present(
    app: &adw::Application,
    mode: Mode,
    title: &str,
    parent: &str,
    options: &Options,
    request: Rc<Request>,
) -> Result<adw::Window, &'static str> {
    // SaveFiles chooses where the files go: the browser picks a folder.
    let directory = match mode {
        Mode::Open => boolean(options, "directory", false)?,
        Mode::Save => false,
        Mode::SaveFiles => true,
    };
    let multiple = mode == Mode::Open && boolean(options, "multiple", false)?;
    let modal = boolean(options, "modal", true)?;
    let filters = match mode {
        Mode::SaveFiles => Vec::new(),
        _ => option::<Vec<FileFilter>>(options, "filters")?.unwrap_or_default(),
    };
    let current = match mode {
        Mode::SaveFiles => None,
        _ => option::<FileFilter>(options, "current_filter")?,
    };
    let choices = option::<Vec<Choice>>(options, "choices")?.unwrap_or_default();
    let current_file = match mode {
        Mode::Save => current_file_from_options(options),
        _ => None,
    };
    let current_name = match mode {
        Mode::Save => option::<String>(options, "current_name")?,
        _ => None,
    };
    let save_files = match mode {
        Mode::SaveFiles => option::<Vec<Vec<u8>>>(options, "files")?.unwrap_or_default(),
        _ => Vec::new(),
    };
    let accept_label = option::<String>(options, "accept_label")?.unwrap_or_else(|| {
        gettext(match mode {
            Mode::Open if directory => "_Select Folder",
            Mode::Open => "_Open",
            _ => "_Save",
        })
    });
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
        .css_classes(["lia-window"])
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
    let name = gtk::Entry::builder()
        .hexpand(true)
        .activates_default(true)
        .build();
    if mode == Mode::Save {
        let initial = current_file
            .as_ref()
            .and_then(|file| file.basename())
            .map(|name| name.to_string_lossy().into_owned())
            .or(current_name)
            .unwrap_or_default();
        name.set_text(&initial);
        name.update_property(&[gtk::accessible::Property::Label(&gettext("Name"))]);
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        let label = gtk::Label::with_mnemonic(&gettext("_Name"));
        label.set_mnemonic_widget(Some(&name));
        row.append(&label);
        row.append(&name);
        browser.append(&row);
    }
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

    let finish: Rc<dyn Fn(Vec<String>)> = Rc::new(glib::clone!(
        #[strong]
        request,
        move |uris: Vec<String>| {
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
    ));
    let submit: Rc<dyn Fn(Vec<gio::File>)> = Rc::new(glib::clone!(
        #[weak]
        error,
        #[weak]
        pane,
        #[weak_allow_none]
        name,
        #[weak]
        window,
        #[strong]
        finish,
        move |files: Vec<gio::File>| {
            let result = match mode {
                Mode::Open => selection_uris(&files, directory, multiple).map_err(gettext),
                Mode::SaveFiles => files
                    .first()
                    .ok_or_else(|| gettext("Select a folder."))
                    .and_then(|folder| save_files_uris(folder, &save_files)),
                Mode::Save => {
                    let (Some(folder), Some(name)) = (pane.location(), &name) else {
                        return;
                    };
                    let typed = name.text();
                    let target = folder.child(typed.as_str());
                    if !typed.is_empty()
                        && !typed.contains('/')
                        && target
                            .query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
                            == gio::FileType::Directory
                    {
                        // Like GTK: typing a folder's name opens it.
                        name.set_text("");
                        pane.navigate_to(&target);
                        return;
                    }
                    if !pane.writable() {
                        error.set_text(&gettext("You can't save files in this folder."));
                        return;
                    }
                    match save_uri(&folder, &typed, pane.portable_names()) {
                        Ok(uri) if target.query_exists(gio::Cancellable::NONE) => {
                            confirm_replace(&window, &typed, finish.clone(), uri);
                            return;
                        }
                        result => result.map(|uri| vec![uri]),
                    }
                }
            };
            match result {
                Ok(uris) => finish(uris),
                Err(message) => error.set_text(&message),
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
        #[weak_allow_none]
        name,
        #[strong]
        submit,
        move |pane, file| {
            if mode == Mode::Save
                && let Some(name) = &name
            {
                // Activating a file saves over it, after confirmation.
                if let Some(basename) = file.basename() {
                    name.set_text(&basename.to_string_lossy());
                }
                submit(Vec::new());
            } else if !directory {
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
        #[weak_allow_none]
        name,
        move |pane: &FolderPane| {
            error.set_text("");
            let items = pane.selected_items();
            let valid = if mode == Mode::Save {
                name.is_some_and(|name| !name.text().is_empty())
                    && pane.location().is_some_and(|f| f.path().is_some())
            } else if items.is_empty() {
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
    if mode == Mode::Save {
        // Choosing a file suggests its name, to save over it.
        pane.connect_selection_changed(glib::clone!(
            #[weak]
            name,
            move |pane| {
                if let [item] = pane.selected_items().as_slice()
                    && !item.is_dir()
                    && let Some(basename) = item.target_file().basename()
                {
                    name.set_text(&basename.to_string_lossy());
                }
            }
        ));
        name.connect_changed(glib::clone!(
            #[weak]
            pane,
            #[strong]
            update,
            move |_| update(&pane)
        ));
    }
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
    let initial = current_file
        .as_ref()
        .and_then(|file| file.parent())
        .or_else(|| folder_from_options(options))
        .filter(|folder| {
            folder.query_file_type(gio::FileQueryInfoFlags::NONE, gio::Cancellable::NONE)
                == gio::FileType::Directory
        })
        .unwrap_or_else(|| gio::File::for_path(glib::home_dir()));
    pane.navigate_to(&initial);
    gtk::prelude::WidgetExt::realize(&window);
    set_parent(&window, parent);
    window.present();
    if mode == Mode::Save {
        // Ready to type over the name, keeping its extension.
        name.grab_focus();
        let text = name.text();
        let stem = naming::stem_char_count(&text, false);
        name.select_region(0, stem as i32);
    }
    Ok(window)
}

fn confirm_replace(window: &adw::Window, name: &str, finish: Rc<dyn Fn(Vec<String>)>, uri: String) {
    let dialog = adw::AlertDialog::builder()
        .heading(gettext("Replace “{}”?").replace("{}", name))
        .body(gettext(
            "A file with this name already exists. Saving will replace its contents.",
        ))
        .default_response("cancel")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", &gettext("_Cancel")),
        ("replace", &gettext("_Replace")),
    ]);
    dialog.set_response_appearance("replace", adw::ResponseAppearance::Destructive);
    dialog.connect_response(None, move |_, response| {
        if response == "replace" {
            finish(vec![uri.clone()]);
        }
    });
    dialog.present(Some(window));
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
