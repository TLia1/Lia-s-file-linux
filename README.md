# Lia Files

A clean, modern file manager for Linux: a tidy command bar, a breadcrumb
address bar, tabs, a quiet sidebar and a details pane. Built with Rust,
GTK 4 and libadwaita.

![Details view with the details pane](data/screenshots/main.png)

## Install

- **Flatpak**: download `lia-files.flatpak` from the
  [latest release](https://github.com/TLia1/Lia-s-file-linux/releases/latest),
  then `flatpak install --user lia-files.flatpak`.
- **Arch Linux / CachyOS**: build `build-aux/PKGBUILD` with `makepkg -si`.

To make it the default file manager:
`xdg-mime default io.github.lia.Files.desktop inode/directory`.

To use Lia Files for other apps' **Choose File**, **Select Folder** and **Save As** dialogs,
install the native file chooser portal and select `lia` in your portal settings.
See [File chooser integration](docs/file-chooser.md) for setup, rollback and tests.

## Build from source

Needs Rust ≥ 1.88, GTK ≥ 4.20, libadwaita ≥ 1.8, GtkSourceView 5,
libarchive, Meson and blueprint-compiler.

```sh
cargo run                 # run uninstalled
make install              # meson setup build && ninja -C build install
```

Flatpak: `flatpak-builder --user --install --force-clean _flatpak build-aux/io.github.lia.Files.json`.

`make lint` and `make test` run the checks CI runs; [docs/qa.md](docs/qa.md)
has the manual checklist and the release steps. [PLAN.md](PLAN.md) describes
the design and architecture.

## Disk usage

Choose **Analyze Disk Usage** from the main menu or a folder’s context menu.
The selected folder (or current location) is scanned in the background. Results
show allocated space, largest first, with percentage bars. Click a folder to
explore it, use the up arrow to return, or **Open Folder** to browse it normally.
Click a file to reveal it in the file manager.
Large lists load 100 entries at a time. You can cancel or close a running scan.

Hidden files are included. Symbolic links are not followed, hard links are
counted once, and other filesystems and virtual system folders are skipped.
Unreadable entries are reported, so incomplete totals are visible. Shared or
compressed filesystem extents can make these totals differ from reclaimable
space; this tool does not delete files.

## Themes

Appearance preferences and watched user stylesheets let Lia Files follow desktop
themes, including Penumbra. See [Themes and desktop integration](docs/themes.md)
for tokens, text preview schemes, glass surfaces and custom context-menu actions.

## License

GPL-3.0-or-later.
