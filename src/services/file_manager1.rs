//! The `org.freedesktop.FileManager1` D-Bus interface, which other apps
//! (browsers' "Show in folder", IDEs, portals) call to reveal files.
//!
//! Spec: <https://www.freedesktop.org/wiki/Specifications/file-manager-interface/>

use gtk::gio::prelude::*;
use gtk::{gio, glib};

pub const BUS_NAME: &str = "org.freedesktop.FileManager1";
const OBJECT_PATH: &str = "/org/freedesktop/FileManager1";

const INTERFACE_XML: &str = r#"
<node>
  <interface name="org.freedesktop.FileManager1">
    <method name="ShowFolders">
      <arg type="as" name="URIs" direction="in"/>
      <arg type="s" name="StartupId" direction="in"/>
    </method>
    <method name="ShowItems">
      <arg type="as" name="URIs" direction="in"/>
      <arg type="s" name="StartupId" direction="in"/>
    </method>
    <method name="ShowItemProperties">
      <arg type="as" name="URIs" direction="in"/>
      <arg type="s" name="StartupId" direction="in"/>
    </method>
  </interface>
</node>
"#;

/// What a caller asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Open each folder.
    ShowFolders,
    /// Open the folders containing the items, with the items selected.
    ShowItems,
    /// Show the items' properties.
    ShowItemProperties,
}

#[derive(Debug)]
pub struct Call {
    pub request: Request,
    pub files: Vec<gio::File>,
    /// An X11 startup notification id or a Wayland activation token.
    pub startup_id: String,
}

impl DBusMethodCall for Call {
    fn parse_call(
        _obj_path: &str,
        _interface: Option<&str>,
        method: &str,
        params: glib::Variant,
    ) -> Result<Self, glib::Error> {
        let request = match method {
            "ShowFolders" => Request::ShowFolders,
            "ShowItems" => Request::ShowItems,
            "ShowItemProperties" => Request::ShowItemProperties,
            _ => {
                return Err(glib::Error::new(
                    gio::DBusError::UnknownMethod,
                    &format!("No such method: {method}"),
                ));
            }
        };
        let (uris, startup_id) = params.get::<(Vec<String>, String)>().ok_or_else(|| {
            glib::Error::new(gio::DBusError::InvalidArgs, "Expected (as, s) arguments")
        })?;
        Ok(Self {
            request,
            files: parse_uris(&uris),
            startup_id,
        })
    }
}

/// Callers pass URIs, but some pass plain paths; both are accepted.
/// Duplicates are dropped, keeping the first occurrence.
pub fn parse_uris(uris: &[String]) -> Vec<gio::File> {
    let mut files: Vec<gio::File> = Vec::new();
    for uri in uris.iter().map(|u| u.trim()).filter(|u| !u.is_empty()) {
        let file = gio::File::for_commandline_arg(uri);
        if !files.iter().any(|f| f.equal(&file)) {
            files.push(file);
        }
    }
    files
}

/// Groups `files` by parent folder, in order of first appearance. Files
/// without a parent (a root) are shown as folders of their own.
pub fn group_by_parent(files: &[gio::File]) -> Vec<(gio::File, Vec<gio::File>)> {
    let mut groups: Vec<(gio::File, Vec<gio::File>)> = Vec::new();
    for file in files {
        let Some(parent) = file.parent() else {
            groups.push((file.clone(), Vec::new()));
            continue;
        };
        match groups.iter_mut().find(|(p, _)| p.equal(&parent)) {
            Some((_, children)) => children.push(file.clone()),
            None => groups.push((parent, vec![file.clone()])),
        }
    }
    groups
}

/// Exports the interface on `connection` and requests the well-known name.
/// Another file manager that already owns the name keeps it, unless it
/// allows replacement. `handler` runs on the main thread for every call.
pub fn export(connection: &gio::DBusConnection, handler: impl Fn(Call) + 'static) {
    let info = match gio::DBusNodeInfo::for_xml(INTERFACE_XML)
        .map(|node| node.lookup_interface(BUS_NAME))
    {
        Ok(Some(info)) => info,
        Ok(None) => unreachable!("the interface is defined above"),
        Err(err) => {
            tracing::error!("invalid FileManager1 introspection data: {err}");
            return;
        }
    };
    let registered = connection
        .register_object(OBJECT_PATH, &info)
        .typed_method_call::<Call>()
        .invoke_and_return(move |_, sender, call| {
            tracing::debug!("{BUS_NAME} call from {sender:?}: {call:?}");
            handler(call);
            Ok(None)
        })
        .build();
    if let Err(err) = registered {
        tracing::warn!("cannot export {BUS_NAME}: {err}");
        return;
    }
    gio::bus_own_name_on_connection(
        connection,
        BUS_NAME,
        gio::BusNameOwnerFlags::ALLOW_REPLACEMENT,
        |_, name| tracing::debug!("acquired {name}"),
        |_, name| tracing::info!("{name} is owned by another file manager"),
    );
}
