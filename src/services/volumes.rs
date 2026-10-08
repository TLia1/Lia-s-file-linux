//! Drives, volumes and mounts, shared by the sidebar and the "This PC"
//! page: listing them from the `gio::VolumeMonitor`, mounting, unmounting
//! and ejecting.
//!
//! Mounting and unmounting take a `gtk::MountOperation`, which asks for
//! passwords (network shares, LUKS-encrypted drives) and shows which apps
//! keep a drive busy.

use gettextrs::gettext;
use gtk::{gio, glib, prelude::*};

/// URI schemes of network locations.
pub const NETWORK_SCHEMES: [&str; 10] = [
    "smb",
    "sftp",
    "ssh",
    "ftp",
    "ftps",
    "dav",
    "davs",
    "afp",
    "nfs",
    "google-drive",
];

pub fn is_network(file: &gio::File) -> bool {
    file.uri_scheme()
        .is_some_and(|scheme| NETWORK_SCHEMES.contains(&scheme.as_str()))
}

#[derive(Clone)]
pub enum Device {
    Mount(gio::Mount),
    /// A volume that isn't mounted yet; opening it mounts it.
    Volume(gio::Volume),
}

impl Device {
    pub fn name(&self) -> String {
        match self {
            Self::Mount(mount) => mount.name().into(),
            Self::Volume(volume) => volume.name().into(),
        }
    }

    pub fn symbolic_icon(&self) -> gio::Icon {
        match self {
            Self::Mount(mount) => mount.symbolic_icon(),
            Self::Volume(volume) => volume.symbolic_icon(),
        }
    }

    /// The full-colour icon.
    pub fn icon(&self) -> gio::Icon {
        match self {
            Self::Mount(mount) => mount.icon(),
            Self::Volume(volume) => volume.icon(),
        }
    }

    /// Where its files are, once mounted.
    pub fn location(&self) -> Option<gio::File> {
        match self {
            Self::Mount(mount) => Some(mount.root()),
            Self::Volume(volume) => volume.get_mount().map(|m| m.root()),
        }
    }

    pub fn is_network(&self) -> bool {
        match self {
            Self::Mount(mount) => is_network(&mount.root()),
            Self::Volume(volume) => {
                volume
                    .identifier(gio::VOLUME_IDENTIFIER_KIND_CLASS)
                    .as_deref()
                    == Some("network")
            }
        }
    }

    /// The mount that can be unmounted or ejected.
    pub fn removable_mount(&self) -> Option<gio::Mount> {
        match self {
            Self::Mount(mount) if mount.can_eject() || mount.can_unmount() => Some(mount.clone()),
            _ => None,
        }
    }

    pub fn volume(&self) -> Option<gio::Volume> {
        match self {
            Self::Volume(volume) => Some(volume.clone()),
            Self::Mount(_) => None,
        }
    }
}

/// Mounted drives and network locations (except the root file system),
/// then volumes that can be mounted.
pub fn devices(monitor: &gio::VolumeMonitor) -> Vec<Device> {
    let mut devices = Vec::new();
    for mount in monitor.mounts() {
        let root = mount.root();
        if mount.is_shadowed() || root.path().is_some_and(|p| p.parent().is_none()) {
            continue;
        }
        devices.push(Device::Mount(mount));
    }
    for volume in monitor.volumes() {
        if volume.get_mount().is_none() && volume.can_mount() {
            devices.push(Device::Volume(volume));
        }
    }
    devices
}

fn mount_operation(parent: Option<&gtk::Widget>) -> gtk::MountOperation {
    let window = parent
        .and_then(|w| w.root())
        .and_then(|root| root.downcast::<gtk::Window>().ok());
    gtk::MountOperation::new(window.as_ref())
}

/// Whether an error was already shown to the user (e.g. a password
/// dialog was cancelled).
pub fn is_handled(err: &glib::Error) -> bool {
    err.matches(gio::IOErrorEnum::FailedHandled)
}

/// Mounts `volume` and returns its root folder.
pub async fn mount(
    volume: &gio::Volume,
    parent: Option<&gtk::Widget>,
) -> Result<gio::File, glib::Error> {
    let operation = mount_operation(parent);
    volume
        .mount_future(gio::MountMountFlags::NONE, Some(&operation))
        .await?;
    volume.get_mount().map(|m| m.root()).ok_or_else(|| {
        glib::Error::new(
            gio::IOErrorEnum::NotMounted,
            &gettext("The volume was mounted but can't be found"),
        )
    })
}

/// Mounts the network share or drive `location` is on (no-op if it
/// already is).
pub async fn mount_location(
    location: &gio::File,
    parent: Option<&gtk::Widget>,
) -> Result<(), glib::Error> {
    let operation = mount_operation(parent);
    match location
        .mount_enclosing_volume_future(gio::MountMountFlags::NONE, Some(&operation))
        .await
    {
        Err(err) if err.matches(gio::IOErrorEnum::AlreadyMounted) => Ok(()),
        result => result,
    }
}

/// Ejects `mount` if it can be, otherwise unmounts it. Returns a message
/// for the user on success ("… can be safely removed"), if any.
pub async fn eject(
    mount: &gio::Mount,
    parent: Option<&gtk::Widget>,
) -> Result<Option<String>, glib::Error> {
    let operation = mount_operation(parent);
    let name = mount.name();
    if mount.can_eject() {
        mount
            .eject_with_operation_future(gio::MountUnmountFlags::NONE, Some(&operation))
            .await?;
        return Ok(Some(
            gettext("“{}” can be safely removed").replace("{}", &name),
        ));
    }
    mount
        .unmount_with_operation_future(gio::MountUnmountFlags::NONE, Some(&operation))
        .await?;
    // A drive that can be powered off, e.g. a USB disk with one partition.
    let unused = |d: &gio::Drive| d.volumes().iter().all(|v| v.get_mount().is_none());
    if let Some(drive) = mount.drive().filter(|d| d.can_stop() && unused(d)) {
        drive
            .stop_future(gio::MountUnmountFlags::NONE, Some(&operation))
            .await?;
        return Ok(Some(
            gettext("“{}” can be safely removed").replace("{}", &name),
        ));
    }
    Ok(None)
}

/// Unmounts `mount`, keeping the drive available to mount again.
pub async fn unmount(mount: &gio::Mount, parent: Option<&gtk::Widget>) -> Result<(), glib::Error> {
    let operation = mount_operation(parent);
    mount
        .unmount_with_operation_future(gio::MountUnmountFlags::NONE, Some(&operation))
        .await
}

/// The size and free space of the file system `location` is on.
pub async fn usage(location: &gio::File) -> Option<(u64, u64)> {
    let attributes = format!(
        "{},{}",
        gio::FILE_ATTRIBUTE_FILESYSTEM_SIZE,
        gio::FILE_ATTRIBUTE_FILESYSTEM_FREE
    );
    let info = location
        .query_filesystem_info_future(&attributes, glib::Priority::LOW)
        .await
        .ok()?;
    let size = info.attribute_uint64(gio::FILE_ATTRIBUTE_FILESYSTEM_SIZE);
    let free = info
        .attribute_uint64(gio::FILE_ATTRIBUTE_FILESYSTEM_FREE)
        .min(size);
    (size > 0).then_some((size, free))
}

/// Makes a thin drive usage bar: accent colour, red above 90 %.
pub fn usage_bar() -> gtk::LevelBar {
    let bar = gtk::LevelBar::builder()
        .min_value(0.0)
        .max_value(1.0)
        .visible(false)
        .css_classes(["usage-bar"])
        .build();
    bar.update_property(&[gtk::accessible::Property::Label(&gettext("Used space"))]);
    for name in [
        gtk::LEVEL_BAR_OFFSET_LOW,
        gtk::LEVEL_BAR_OFFSET_HIGH,
        gtk::LEVEL_BAR_OFFSET_FULL,
    ] {
        bar.remove_offset_value(Some(name));
    }
    bar.add_offset_value("usage-normal", 0.9);
    bar.add_offset_value("usage-critical", 1.0);
    bar
}

/// "120 GB free of 512 GB".
pub fn usage_text(size: u64, free: u64) -> String {
    use crate::util::format;
    gettext("{free} free of {size}")
        .replace("{free}", &format::size(free))
        .replace("{size}", &format::size(size))
}
