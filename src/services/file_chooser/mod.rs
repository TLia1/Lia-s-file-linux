//! Host file chooser portal: opening, choosing folders and saving all use
//! Lia's browser.
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gettextrs::gettext;
use gtk::{gio, glib};

use crate::services::file_ops::naming;

mod picker;

pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.lia";
const INTERFACE: &str = "org.freedesktop.impl.portal.FileChooser";
const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
pub type Options = HashMap<String, glib::Variant>;
pub type FileFilter = (String, Vec<(u32, String)>);
pub type Choice = (String, String, Vec<(String, String)>, String);

const REQUEST_XML: &str = r#"<node><interface name="org.freedesktop.impl.portal.Request">
<method name="Close"/></interface></node>"#;

fn interface_xml() -> String {
    let mut xml = format!(
        "<node><interface name='{INTERFACE}'><property name='version' type='u' access='read'/>"
    );
    for method in ["OpenFile", "SaveFile", "SaveFiles"] {
        xml.push_str(&format!(
            "<method name='{method}'>
        <arg type='o' direction='in'/><arg type='s' direction='in'/>
        <arg type='s' direction='in'/><arg type='s' direction='in'/>
        <arg type='a{{sv}}' direction='in'/><arg type='u' direction='out'/>
        <arg type='a{{sv}}' direction='out'/></method>"
        ));
    }
    xml.push_str("</interface></node>");
    xml
}

/// A pending interaction owns its invocation until a single terminal response.
struct Request {
    connection: gio::DBusConnection,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    invocation: RefCell<Option<gio::DBusMethodInvocation>>,
    registration: RefCell<Option<gio::RegistrationId>>,
    watcher: RefCell<Option<Box<dyn FnOnce()>>>,
    window: RefCell<Option<adw::Window>>,
}

impl Request {
    fn finish(&self, result: glib::Variant) {
        // Also guards callbacks fired by closing/destroying the window.
        let Some(invocation) = self.invocation.take() else {
            return;
        };
        if let Some(id) = self.registration.take() {
            let _ = self.connection.unregister_object(id);
        }
        if let Some(id) = self.watcher.take() {
            id();
        }
        if let Some(window) = self.window.take() {
            window.destroy();
        }
        invocation.return_value(Some(&result));
        let _ = self.connection.flush_sync(gio::Cancellable::NONE);
        // Release the application hold once the reply is sent so the portal
        // exits after its inactivity timeout and frees GTK/GPU memory.
        self.hold.take();
    }

    fn respond(&self, response: u32, results: Options) {
        self.finish((response, results).to_variant());
    }

    fn cancel(&self) {
        self.respond(1, Options::new());
    }
}

pub fn run() -> glib::ExitCode {
    // Exit 2s after the last picker closes so GTK/GSK/GPU caches (~150 MB) are
    // not kept in RAM while idle; D-Bus re-activates the service on demand.
    let app = adw::Application::builder()
        .application_id("io.github.lia.Files.Portal")
        .flags(gio::ApplicationFlags::IS_SERVICE)
        .inactivity_timeout(2000)
        .resource_base_path(crate::config::RESOURCE_BASE)
        .build();
    app.connect_startup(|app| {
        crate::application::fix_font_rendering();
        crate::application::follow_color_scheme();
        let appearance = crate::util::appearance::Appearance::new();
        app.connect_shutdown(move |_| {
            let _ = &appearance;
        });
        let Some(connection) = app.dbus_connection() else {
            app.quit();
            return;
        };
        let registration = export(app, &connection);
        if let Err(error) = registration {
            tracing::error!("cannot export file chooser: {error}");
            app.quit();
            return;
        }
        // Request the name directly: gio's bus-own-name closure cannot handle
        // the NULL connection supplied when a private/session bus disappears.
        let owned = connection
            .call_sync(
                Some("org.freedesktop.DBus"),
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
                "RequestName",
                Some(&(BUS_NAME, 4u32).to_variant()), // DO_NOT_QUEUE
                None,
                gio::DBusCallFlags::NONE,
                5000,
                gio::Cancellable::NONE,
            )
            .ok()
            .and_then(|value| value.get::<(u32,)>());
        if !matches!(owned, Some((1 | 4,))) {
            tracing::error!("cannot acquire {BUS_NAME}");
            app.quit();
            return;
        }
        app.add_action_entries([gio::ActionEntry::builder("quit")
            .activate(|app: &adw::Application, _, _| app.quit())
            .build()]);
    });
    app.run_with_args(&["lia-files-portal"])
}

fn dispatch(
    app: &adw::Application,
    connection: gio::DBusConnection,
    sender: Option<&str>,
    method: &str,
    params: glib::Variant,
    invocation: gio::DBusMethodInvocation,
) {
    let Some((handle, _app_id, parent, title, options)) =
        params.get::<(glib::variant::ObjectPath, String, String, String, Options)>()
    else {
        invocation.return_dbus_error(
            "org.freedesktop.DBus.Error.InvalidArgs",
            "Expected (osssa{sv})",
        );
        return;
    };
    let request = Rc::new(Request {
        connection: connection.clone(),
        hold: RefCell::new(Some(app.hold())),
        invocation: RefCell::new(Some(invocation)),
        registration: RefCell::default(),
        watcher: RefCell::default(),
        window: RefCell::default(),
    });
    let info = gio::DBusNodeInfo::for_xml(REQUEST_XML)
        .unwrap()
        .lookup_interface("org.freedesktop.impl.portal.Request")
        .unwrap();
    let pending = request.clone();
    let owner = sender.unwrap_or_default().to_string();
    let registration = connection
        .register_object(&handle, &info)
        .method_call(move |_, sender, _, _, _, _, invocation| {
            if sender != Some(owner.as_str()) {
                invocation.return_dbus_error(
                    "org.freedesktop.DBus.Error.AccessDenied",
                    "Not the request owner",
                );
                return;
            }
            invocation.return_value(None);
            pending.cancel();
        })
        .build();
    match registration {
        Ok(id) => {
            request.registration.replace(Some(id));
        }
        Err(error) => {
            tracing::warn!("cannot register chooser request: {error}");
            request.respond(2, Options::new());
            return;
        }
    }
    if let Some(sender) = sender {
        let pending = request.clone();
        let watcher = gio::bus_watch_name_on_connection(
            &connection,
            sender,
            gio::BusNameWatcherFlags::NONE,
            |_, _, _| {},
            move |_, _| pending.cancel(),
        );
        request
            .watcher
            .replace(Some(Box::new(move || gio::bus_unwatch_name(watcher))));
    }
    let mode = match method {
        "SaveFile" => picker::Mode::Save,
        "SaveFiles" => picker::Mode::SaveFiles,
        _ => picker::Mode::Open,
    };
    match picker::present(app, mode, &title, &parent, &options, request.clone()) {
        Ok(window) => {
            request.window.replace(Some(window));
        }
        Err(error) => {
            tracing::warn!("invalid chooser options: {error}");
            request.respond(2, Options::new());
        }
    }
}

/// Decode a portal path without losing non-UTF-8 filesystem bytes.
pub fn folder_from_options(options: &Options) -> Option<gio::File> {
    path_option(options, "current_folder")
}

/// The file a Save request suggests overwriting (SaveFile's `current_file`).
pub fn current_file_from_options(options: &Options) -> Option<gio::File> {
    path_option(options, "current_file")
}

fn path_option(options: &Options, key: &str) -> Option<gio::File> {
    let bytes = options.get(key)?.get::<Vec<u8>>()?;
    path_from_bytes(&bytes).map(gio::File::for_path)
}

fn path_from_bytes(bytes: &[u8]) -> Option<std::path::PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
    if bytes.is_empty() || bytes.contains(&0) {
        return None;
    }
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(bytes));
    path.is_absolute().then(|| path.to_path_buf())
}

/// Where SaveFile writes `name` in `folder`. The file need not exist yet.
pub fn save_uri(folder: &gio::File, name: &str, portable: bool) -> Result<String, String> {
    naming::validate_name(name, portable).map_err(|error| error.message())?;
    let folder = folder
        .path()
        .ok_or_else(|| gettext("This location is not available as a local file."))?;
    if !folder.is_dir() {
        return Err(gettext("The folder is no longer available."));
    }
    Ok(gio::File::for_path(folder.join(name)).uri().to_string())
}

/// SaveFiles puts every suggested name in `folder`, renaming the ones that
/// would replace an existing file (or each other), as the portal allows.
pub fn save_files_uris(folder: &gio::File, files: &[Vec<u8>]) -> Result<Vec<String>, String> {
    use std::os::unix::ffi::OsStrExt;
    let folder = folder
        .path()
        .ok_or_else(|| gettext("This location is not available as a local file."))?;
    if !folder.is_dir() {
        return Err(gettext("The folder is no longer available."));
    }
    let mut taken: Vec<std::ffi::OsString> = Vec::new();
    let mut uris = Vec::with_capacity(files.len());
    for bytes in files {
        let bytes = bytes.strip_suffix(&[0]).unwrap_or(bytes);
        let suggested = std::path::Path::new(std::ffi::OsStr::from_bytes(bytes))
            .file_name()
            .filter(|name| {
                !bytes.contains(&0) && naming::validate_name(&name.to_string_lossy(), false).is_ok()
            })
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| gettext("Untitled"));
        let name = naming::unique_name(&suggested, false, naming::UniqueStyle::Numbered, |n| {
            taken.iter().any(|t| t.as_os_str() == n) || folder.join(n).exists()
        });
        taken.push(name.clone().into());
        uris.push(gio::File::for_path(folder.join(name)).uri().to_string());
    }
    Ok(uris)
}

/// Only local/FUSE-backed paths may be returned by a portal backend.
pub fn selection_uris(
    files: &[gio::File],
    directory: bool,
    multiple: bool,
) -> Result<Vec<String>, &'static str> {
    if files.is_empty() {
        return Err("Select a file or folder.");
    }
    if !multiple && files.len() != 1 {
        return Err("Select only one item.");
    }
    files
        .iter()
        .map(|file| {
            let path = file
                .path()
                .ok_or("This location is not available as a local file.")?;
            let metadata = std::fs::metadata(&path)
                .map_err(|_| "The selected item is no longer available.")?;
            if metadata.is_dir() != directory {
                return Err(if directory {
                    "Select a folder."
                } else {
                    "Select a file."
                });
            }
            Ok(gio::File::for_path(path).uri().to_string())
        })
        .collect()
}

fn export(
    app: &adw::Application,
    connection: &gio::DBusConnection,
) -> Result<gio::RegistrationId, glib::Error> {
    let info = gio::DBusNodeInfo::for_xml(&interface_xml())
        .unwrap()
        .lookup_interface(INTERFACE)
        .unwrap();
    let weak = app.downgrade();
    let prop_weak = weak.clone();
    connection
        .register_object(OBJECT_PATH, &info)
        .property(move |_, _, _, _, _| {
            if let Some(app) = prop_weak.upgrade() {
                drop(app.hold());
            }
            4u32.to_variant()
        })
        .method_call(
            move |connection, sender, _, _, method, params, invocation| {
                if let Some(app) = weak.upgrade() {
                    dispatch(&app, connection, sender, method, params, invocation);
                } else {
                    invocation.return_dbus_error(
                        "org.freedesktop.DBus.Error.Failed",
                        "Portal is shutting down",
                    );
                }
            },
        )
        .build()
}

#[cfg(test)]
mod tests;
