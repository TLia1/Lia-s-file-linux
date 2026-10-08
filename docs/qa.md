# Quality checks

Run before each release. The automated checks catch regressions; the manual
ones cover what tools can't judge (how it sounds in Orca, how it looks).

## Automated

| Check | Command |
|---|---|
| Format, lints | `make lint` |
| Unit & integration tests | `make test` |
| FAT/NTFS name handling on a real drive | see below, then `LIA_TEST_FAT_DIR=<mount point> cargo test --test edge_cases` |
| Accessible names (main window + 11 dialogs) | `cargo build && tools/a11y-audit.sh` — exits 1 if a control has no name |
| Translations are valid | `msgfmt --check --statistics -o /dev/null po/fr.po` |

A FAT drive without root: create an image, then let udisks map and mount it.

```sh
truncate -s 16M /tmp/fat.img && mkfs.vfat /tmp/fat.img
udisksctl loop-setup -f /tmp/fat.img     # → /dev/loopN
udisksctl mount -b /dev/loopN            # → /run/media/$USER/XXXX-XXXX
# afterwards: udisksctl unmount -b /dev/loopN && udisksctl loop-delete -b /dev/loopN
```

### Performance

`make test-tree` creates a 100 000-file folder. With `LIA_LOG=lia_files=debug`
the app logs the time to its first frame, to the first items of a folder and
to the complete listing. Measure a release build (`cargo build --release`).

Baseline (2026-10-08, release, tmpfs, headless KWin):

| Metric | Target | Details view | Grid view |
|---|---|---|---|
| Start → first frame | < 300 ms | ~190 ms | ~190 ms |
| 1 000 items: first / complete | < 50 / < 150 ms | 25–35 / 81–89 ms | 25–37 / 138–145 ms |
| 100 000 items: first / complete | < 100 ms / — | 24–33 ms / ~2.0 s | 25–37 ms / ~1.8 s |
| Memory (RSS) | < 120 MB with 5 tabs | 178 MB with 1 tab, 205 MB with 5 | |

Memory is over target, but mostly a fixed cost (one tab already takes
178 MB in this software-rendered session); it hasn't been profiled yet.
Scrolling frame rate and thumbnail latency can only be judged on real
hardware: use the GTK inspector (Ctrl+Shift+D → Recorder) or Sysprof.

## Manual

### Screen reader (Orca)

Start Orca (Super+Alt+S on GNOME), then without looking at the screen:

- [ ] Every toolbar and command-bar button is announced with its name.
- [ ] Moving through the file list announces each file; the list is called "Files".
- [ ] The sidebar is "Places"; drives announce their free space.
- [ ] Dialogs (Properties, Batch Rename, Conflict, Preferences) announce their title and each field's label.
- [ ] Toasts ("Moved 3 items to the Trash") are read out.

### Keyboard only

Unplug the mouse:

- [ ] Tab / Shift+Tab reach the sidebar, address bar, search, command bar and file list; the focus ring is always visible.
- [ ] Every shortcut in Ctrl+? works.
- [ ] Rename (F2), New Folder (Ctrl+Shift+N), Trash (Del) and Undo (Ctrl+Z) work without the mouse.
- [ ] Menus (New, Sort, View, ⋯) open with Enter/Space and close with Escape.
- [ ] Every dialog can be completed and dismissed (Escape) from the keyboard.

### Look

- [ ] Light and dark style; every accent colour.
- [ ] Scaling at 100 %, 125 %, 150 % and 200 %: no clipped text or icons.
- [ ] High contrast (Settings → Accessibility) keeps selection and focus visible.
- [ ] Large text: rows grow, nothing overlaps.

### Languages

- [ ] French: `LANGUAGE=fr` with the app installed (`make install`), or with
      `LIA_LOCALEDIR` set at build time to a folder containing
      `fr/LC_MESSAGES/lia-files.mo`. Long labels are ellipsized, not clipped.
- [ ] Right-to-left: the sidebar is on the right, the breadcrumb starts from
      the right and back/forward arrows are mirrored. Without an RTL locale
      installed, compile one: `localedef -i he_IL -f UTF-8 /tmp/loc/he_IL.utf8`,
      then run with `LOCPATH=/tmp/loc LC_ALL=he_IL.utf8`.

### File systems & crashes

- [ ] Copy to a USB stick, unplug it mid-copy: an error is shown, the app keeps working.
- [ ] Kill the app (`kill -9`) during a large copy, restart it: the hidden
      `.name.part` file is gone (see `~/.local/state/lia-files/recovery/`)
      and Ctrl+Z still undoes the last action from before the crash.

### Integration

- [ ] "Show in folder" from a browser's downloads opens the folder with the
      file selected. Or by hand:
      `gdbus call --session --dest org.freedesktop.FileManager1 --object-path /org/freedesktop/FileManager1 --method org.freedesktop.FileManager1.ShowItems "['file:///etc/hosts']" ""`
      (`ShowItemProperties` opens Properties; `ShowFolders` opens folders).
      Another running file manager may own the name: quit it first.

## Release

1. Bump the version in `Cargo.toml` and `meson.build`, and add a
   `<release>` with notes at the top of the metainfo `<releases>`.
2. `cargo build`, then `build-aux/flatpak-cargo-sources.py` (Cargo.lock
   changed) and `tools/screenshots.sh` if the UI changed.
3. Update the translations: `meson compile -C build lia-files-pot lia-files-update-po`,
   translate the new strings in `po/fr.po`.
4. Commit, tag `vX.Y.Z` and push the tag: CI builds the Flatpak bundle and
   attaches it to a GitHub release. The metainfo screenshot URLs point at
   the tag, so the tag must contain `data/screenshots/`.
5. AUR: copy `build-aux/PKGBUILD`, set `pkgver`, run `updpkgsums` and
   `makepkg --printsrcinfo > .SRCINFO`, then push to the AUR.
