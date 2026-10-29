//! Host file chooser portal. OpenFile uses Lia's browser; saves are delegated
//! directly to the GTK backend (never to the frontend, which would recurse).
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};

mod picker;

pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.lia";
const INTERFACE: &str = "org.freedesktop.impl.portal.FileChooser";
const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";
const GTK_BACKEND: &str = "org.freedesktop.impl.portal.desktop.gtk";
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
    invocation: RefCell<Option<gio::DBusMethodInvocation>>,
    registration: RefCell<Option<gio::RegistrationId>>,
    watcher: RefCell<Option<Box<dyn FnOnce()>>>,
    window: RefCell<Option<adw::Window>>,
    forwarded: bool,
    handle: String,
    cancellable: gio::Cancellable,
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
    }

    fn respond(&self, response: u32, results: Options) {
        self.finish((response, results).to_variant());
    }

    fn cancel(&self) {
        if self.invocation.borrow().is_none() {
            return;
        }
        if self.forwarded {
            self.connection.call(
                Some(GTK_BACKEND),
                &self.handle,
                "org.freedesktop.impl.portal.Request",
                "Close",
                None,
                None,
                gio::DBusCallFlags::NONE,
                5000,
                gio::Cancellable::NONE,
                |_| {},
            );
            self.cancellable.cancel();
        }
        self.respond(1, Options::new());
    }
}

pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id("io.github.lia.Files.Portal")
        .flags(gio::ApplicationFlags::IS_SERVICE)
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
        let hold = RefCell::new(Some(app.hold()));
        app.connect_shutdown(move |_| {
            hold.borrow_mut().take();
        });
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
        invocation: RefCell::new(Some(invocation)),
        registration: RefCell::default(),
        watcher: RefCell::default(),
        window: RefCell::default(),
        forwarded: method != "OpenFile",
        handle: handle.to_string(),
        cancellable: gio::Cancellable::new(),
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
    if method == "OpenFile" {
        match picker::present(app, &title, &parent, &options, request.clone()) {
            Ok(window) => {
                request.window.replace(Some(window));
            }
            Err(error) => {
                tracing::warn!("invalid chooser options: {error}");
                request.respond(2, Options::new());
            }
        }
    } else {
        let pending = request.clone();
        connection.call(
            Some(GTK_BACKEND),
            OBJECT_PATH,
            INTERFACE,
            method,
            Some(&params),
            None,
            gio::DBusCallFlags::NONE,
            i32::MAX,
            Some(&request.cancellable),
            move |result| match result {
                Ok(result) => pending.finish(result),
                Err(error) => {
                    tracing::warn!("GTK save chooser failed: {error}");
                    pending.respond(2, Options::new());
                }
            },
        );
    }
}

/// Decode a portal path without losing non-UTF-8 filesystem bytes.
pub fn folder_from_options(options: &Options) -> Option<gio::File> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = options.get("current_folder")?.get::<Vec<u8>>()?;
    let bytes = bytes.strip_suffix(&[0]).unwrap_or(&bytes);
    if bytes.is_empty() || bytes.contains(&0) {
        return None;
    }
    let path = std::path::Path::new(std::ffi::OsStr::from_bytes(bytes));
    path.is_absolute().then(|| gio::File::for_path(path))
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
    connection
        .register_object(OBJECT_PATH, &info)
        .property(|_, _, _, _, _| 4u32.to_variant())
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
