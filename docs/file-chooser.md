# Use Lia Files to choose files and folders

The native installation includes a file chooser portal backend. Apps that use
`org.freedesktop.portal.FileChooser` show Lia Files for **Open/Choose File** and
**Select Folder**, with its sidebar, breadcrumbs, search and file view. Picking a
file returns it to the requesting app instead of opening another application.

Supported: single/multiple files or folders, initial folder, custom accept label,
glob and MIME filters, current filter, application choices, cancellation, and
independent simultaneous requests. Folder mode lets you select the current
folder by pressing **Select Folder** without selecting a row. Ctrl+L edits the
location, Ctrl+H toggles hidden files, and Escape cancels.

SaveFile and SaveFiles requests are forwarded directly to
`xdg-desktop-portal-gtk`, including its overwrite confirmation and save options.
Keep that backend installed. Only local paths and mounted locations exposed as
local paths can be returned. Wayland exported parent handles are supported;
X11 pickers currently open as separate windows without parent-window attachment.

## Install and enable

Requires a **native host installation**, `xdg-desktop-portal`, and
`xdg-desktop-portal-gtk`. The Flatpak build deliberately does not install a
backend: a sandboxed file manager cannot serve as the host's file chooser.

```sh
meson setup build --reconfigure
ninja -C build
sudo meson install -C build
```

Meson installs `lia.portal` in the directory used by the installed desktop portal
(detected through pkg-config). Override with
`-Dportal-dir=/usr/share/xdg-desktop-portal/portals` if necessary. Disable this
integration at build time with `-Dfile-chooser-portal=false`.

On recent portal versions that discover user data directories (including 1.22),
you can install and enable just the picker without administrator access:

```sh
cargo build --release
python tools/install-user-portal.py --enable
gdbus call --session --dest org.freedesktop.DBus \
  --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.ReloadConfig
systemctl --user restart xdg-desktop-portal.service
```

This installs the backend in your XDG data directory, preserves other portal
preferences, and prints a backup path when changing an existing configuration.
It leaves the normal file manager launcher in place. It chooses an existing
desktop-specific config ahead of the generic config, following portal precedence.

Edit `~/.config/xdg-desktop-portal/portals.conf` (or the desktop-specific file that
takes precedence). Preserve other entries; add/change only this key under
`[preferred]`:

```ini
[preferred]
org.freedesktop.impl.portal.FileChooser=lia;gtk
```

Keep your existing `default=` and other portal preferences, especially screen
sharing and secrets. Restart the portal, then restart the app whose picker you
want to change:

```sh
systemctl --user restart xdg-desktop-portal.service
```

The backend is activated on demand with `lia-files --file-chooser-portal`; the
normal file manager remains independent. Setting the `inode/directory` default
only controls folder opening and does not change chooser dialogs. Apps that
implement their own picker or bypass portals will keep that picker. The `gtk`
entry is a backend selection fallback, not a retry after a running backend fails.

To revert, set `org.freedesktop.impl.portal.FileChooser=gtk`, restart the portal,
and restart affected apps. Keep a backup of your previous configuration.

## Verification

`cargo test` covers filesystem path handling, URI encoding, selection types and
cardinality. The display-dependent test exercises real D-Bus requests, file and
folder selection, filters, choices, single-file activation, concurrent requests,
request-owner checks, caller disconnect, cancellation and save forwarding.
Run it on a disposable bus and display:

```sh
# Terminal 1: a headless GTK display (or use your normal display for manual QA)
gtk4-broadwayd --address=127.0.0.1 --port=8098 :8
# Terminal 2:
GDK_BACKEND=broadway BROADWAY_DISPLAY=:8 GSETTINGS_BACKEND=memory \
  GTK_A11Y=none ADW_DISABLE_PORTAL=1 GIO_USE_VFS=local \
  dbus-run-session -- cargo test --lib portal_round_trip -- --ignored --nocapture
```

Use temporary `XDG_CONFIG_HOME` and `XDG_STATE_HOME` directories to isolate user
settings. To also render a PNG, first open the Broadway page at
`http://127.0.0.1:8098`, then set `LIA_PICKER_SCREENSHOT=/tmp/picker.png` for the
test. A connected Broadway browser is required for rendering.

Manual checks: open an upload picker and a folder picker in a portal-using app;
select, cancel and reopen them. Test Save As to verify GTK save dialogs still
work. On Wayland, check that the picker stays associated with the requesting
window. Restart the backend after installing a new binary if it is already
running:

```sh
gapplication action io.github.lia.Files.Portal quit
```
