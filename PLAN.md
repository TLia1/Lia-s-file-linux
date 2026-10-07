# Lia's File Manager — Product & Implementation Plan

A clean, modern desktop file manager for Linux, inspired by the clarity of the Windows 11 File Explorer: a tidy command bar, breadcrumb address bar, a quiet sidebar, tabs, and a details pane — without the clutter.

> Working name: **Lia Files** · App ID: `io.github.lia.Files` (change before first release)

---

## Table of contents

1. [Design goals](#1-design-goals)
2. [Feature list](#2-feature-list)
3. [Technology choice](#3-technology-choice)
4. [Architecture](#4-architecture)
5. [Visual design system](#5-visual-design-system)
6. [Project structure](#6-project-structure)
7. [Implementation steps](#7-implementation-steps)
8. [Testing strategy](#8-testing-strategy)
9. [Packaging & distribution](#9-packaging--distribution)
10. [Performance targets](#10-performance-targets)
11. [Future ideas](#11-future-ideas)

---

## 1. Design goals

| Goal | What it means in practice |
|---|---|
| **Clean** | One toolbar row, no menu bar, no visible chrome you don't need. Actions appear when relevant (e.g. "Extract" only when an archive is selected). |
| **Modern** | Rounded corners, soft shadows, generous spacing, smooth 150–200 ms animations, light & dark themes that follow the system. |
| **Familiar** | Keyboard shortcuts and layout match Windows Explorer where it doesn't fight Linux conventions (Ctrl+C/V, F2, Alt+←, Ctrl+L, Ctrl+Shift+N…). |
| **Fast** | Opens in < 300 ms, lists 100 000 files without freezing, never blocks the UI on disk or network I/O. |
| **Native** | Uses the Linux desktop stack properly: trash spec, XDG dirs, MIME/default apps, GVfs for network & phones, freedesktop thumbnails. |
| **Safe** | Delete goes to Trash by default, every file operation can be undone, conflicts are always asked about, nothing is lost on a crash mid-copy. |

---

## 2. Feature list

Priority legend: **P0** = MVP (must ship in v1.0) · **P1** = expected by most users (v1.x) · **P2** = nice to have (later).

### 2.1 Window layout

| Feature | Priority |
|---|---|
| Header bar with back / forward / up / refresh buttons | P0 |
| Breadcrumb address bar — click a segment to jump, click the empty area (or Ctrl+L) to type a path, path autocompletion | P0 |
| Search field in the header bar (filters current folder instantly, Enter = recursive search) | P0 |
| Command bar under the header: New ▾, Cut, Copy, Paste, Rename, Share, Delete, Sort ▾, View ▾, ⋯ more | P0 |
| Collapsible sidebar (navigation pane) | P0 |
| Status bar: item count, selection count & total size, free space of current drive | P0 |
| Tabs (Ctrl+T, Ctrl+W, drag to reorder, drag out to a new window, middle-click to close) | P0 |
| Details / preview pane on the right (toggle Alt+P / Alt+Shift+P) | P1 |
| Split view (two folders side by side, F3) | P2 |
| Remember window size, sidebar width, pane state | P0 |

### 2.2 Sidebar (navigation pane)

| Feature | Priority |
|---|---|
| **Home** and **Recent** entries | P0 |
| XDG user folders: Desktop, Documents, Downloads, Music, Pictures, Videos | P0 |
| Pinned / favourite folders (drag a folder in, right-click → Pin, reorder by drag) — compatible with GTK bookmarks file `~/.config/gtk-3.0/bookmarks` | P0 |
| Drives & partitions with usage bar, eject / unmount button | P0 |
| Removable devices (USB, SD, phones via MTP) appear automatically | P0 |
| Network locations (SMB, SFTP, WebDAV, FTP) and "Connect to server…" | P1 |
| Trash with item count, "Empty trash" on right-click | P0 |
| Collapsible groups (Pinned / This PC / Devices / Network) | P1 |
| Folder tree mode (expandable tree like Explorer) | P2 |

### 2.3 Views

| Feature | Priority |
|---|---|
| **Details view** — columns Name, Date modified, Type, Size; sortable by clicking headers; resizable & reorderable columns; choose extra columns (Date created, Owner, Permissions, Dimensions, Duration) | P0 |
| **Grid / icon view** with 3 sizes (small, medium, large) and Ctrl + scroll zoom | P0 |
| **List / compact view** | P1 |
| Thumbnails for images, videos, PDFs, fonts (freedesktop thumbnail spec, shared cache `~/.cache/thumbnails`) | P0 |
| Per-folder view memory (sort + view mode remembered per directory) | P1 |
| Group by (Type, Date modified "Today / Yesterday / Last week…", Size, Name) | P1 |
| Show/hide hidden files (Ctrl+H) | P0 |
| Show/hide file extensions | P1 |
| Natural sort ("file2" before "file10"), folders first option | P0 |
| Empty-folder and "no results" placeholder states with an illustration | P0 |
| Smooth, virtualized scrolling for huge folders | P0 |

### 2.4 Selection & interaction

| Feature | Priority |
|---|---|
| Click, Ctrl+click, Shift+click, Ctrl+A, rubber-band (lasso) selection | P0 |
| Type-ahead: start typing to jump to a matching file | P0 |
| Double-click / Enter opens with default app | P0 |
| Single-click open option | P2 |
| Checkboxes on hover (touch-friendly, like Explorer) | P1 |
| Drag & drop inside the app, between windows and with other apps (copy / move / link with modifier keys, spring-loaded folders on hover) | P0 |
| Drop on a tab or sidebar entry | P1 |

### 2.5 File operations

| Feature | Priority |
|---|---|
| New folder (Ctrl+Shift+N), new empty file, new from templates (`~/Templates`) | P0 |
| Copy, cut, paste (Ctrl+C/X/V) — interoperable clipboard with other file managers (`x-special/gnome-copied-files` + `text/uri-list`) | P0 |
| Rename in place (F2), smart selection that excludes the extension | P0 |
| Batch rename (pattern, find/replace, numbering) | P1 |
| Move to Trash (Del), permanent delete (Shift+Del, with confirmation) | P0 |
| Restore from Trash, empty Trash | P0 |
| **Undo / redo** (Ctrl+Z / Ctrl+Y) for create, rename, move, copy, trash | P0 |
| Background operation queue with progress popover (speed, ETA, pause, cancel) | P0 |
| Conflict dialog: Replace / Skip / Keep both / Apply to all, with side-by-side comparison of size & date | P0 |
| Duplicate (Ctrl+D) | P1 |
| Create symbolic link | P1 |
| Compress to .zip / .tar.gz / .7z, extract here / extract to… | P1 |
| Open archives as folders (browse without extracting) | P2 |
| Copy path (Ctrl+Shift+C) | P0 |
| Open terminal here | P0 |
| Open as administrator (via `admin://` GVfs backend / polkit) | P2 |

### 2.6 Opening files

| Feature | Priority |
|---|---|
| Open with default application (MIME database) | P0 |
| "Open with…" submenu + "Choose another app" dialog, set as default | P0 |
| Run executables / scripts with a confirmation | P1 |
| Open folder in new tab (middle-click / Ctrl+Enter) or new window | P0 |
| Desktop portal support (works inside Flatpak) | P0 |

### 2.7 Search

| Feature | Priority |
|---|---|
| Instant filter of current folder as you type | P0 |
| Recursive search in background, results stream in live | P0 |
| Filters chips: Type (documents, images, video, audio, folders), Date modified, Size | P1 |
| Search file contents (via ripgrep-style engine) | P2 |
| Use Tracker/LocalSearch or Baloo index when available | P2 |
| Recent searches | P2 |

### 2.8 Context menu (right-click)

Clean, grouped, short — like Windows 11:

- **Top icon row**: Cut · Copy · Rename · Share · Delete
- Open · Open with ▸ · Open in new tab
- Compress / Extract (when relevant)
- Pin to sidebar
- Copy path · Open in terminal
- Properties (Alt+Enter)
- **Background menu** (empty area): View ▸ · Sort by ▸ · Group by ▸ · New ▸ · Paste · Open in terminal · Properties
- Extensible custom actions (`~/.local/share/lia-files/actions/*.desktop`) — P2

### 2.9 Details pane & preview

| Feature | Priority |
|---|---|
| Large icon/thumbnail, name, type, size, dates, location | P1 |
| Image preview with dimensions, EXIF summary | P1 |
| Text / code preview (syntax highlighted, first 200 KB) | P1 |
| PDF first page, audio/video player with play button | P2 |
| Quick Look: Space opens a floating preview overlay | P2 |

### 2.10 Properties dialog

| Feature | Priority |
|---|---|
| General: name, type, location, size (computed async for folders), contents count, dates | P0 |
| Permissions: owner, group, read/write/execute toggles, "Allow executing as program" | P0 |
| Open with: default app for this type | P1 |
| Checksums (MD5 / SHA-256) on demand | P2 |
| Multiple selection → combined size | P0 |

### 2.11 Devices & storage

| Feature | Priority |
|---|---|
| Auto-detect mounts / unmounts (GIO VolumeMonitor → UDisks2) | P0 |
| Mount, unmount, safely remove, format (via GNOME Disks hand-off) | P0 / P2 |
| Drive usage bar in sidebar and "This PC" overview page (Explorer-like drive cards) | P1 |
| Encrypted (LUKS) volume unlock prompt | P1 |
| Phones (MTP), cameras (PTP), Android over GVfs | P1 |

### 2.12 Settings

| Feature | Priority |
|---|---|
| Theme: system / light / dark; accent colour follows system | P0 |
| Default view, icon size, show hidden files, folders first | P0 |
| Open behaviour (single/double click), confirm before trash | P1 |
| Thumbnail policy (always / local only / never, max file size) | P1 |
| Start location (Home, Recent, last session) | P1 |
| Restore tabs on startup | P1 |
| Keyboard shortcuts window (Ctrl+?) | P0 |

### 2.13 Accessibility & i18n

| Feature | Priority |
|---|---|
| Full keyboard navigation, visible focus rings | P0 |
| Screen reader labels (AT-SPI via GTK accessibility API) | P0 |
| High contrast & large text support | P1 |
| Translations with gettext; RTL layout works | P1 |

### 2.14 Keyboard shortcuts (defaults)

| Action | Shortcut |
|---|---|
| Back / Forward / Up | Alt+← / Alt+→ / Alt+↑ |
| Focus address bar | Ctrl+L / Alt+D |
| Search | Ctrl+F / Ctrl+E |
| New tab / close tab / next tab | Ctrl+T / Ctrl+W / Ctrl+Tab |
| New window | Ctrl+N |
| New folder | Ctrl+Shift+N |
| Rename | F2 |
| Trash / delete permanently | Del / Shift+Del |
| Undo / redo | Ctrl+Z / Ctrl+Y (also Ctrl+Shift+Z) |
| Select all / invert | Ctrl+A / Ctrl+Shift+I |
| Properties | Alt+Enter |
| Hidden files | Ctrl+H |
| Refresh | F5 |
| Zoom in / out / reset | Ctrl++ / Ctrl+− / Ctrl+0 |
| Views (details / grid) | Ctrl+1 / Ctrl+2 |
| Details pane | Alt+Shift+P |
| Quick Look | Space |
| Shortcuts help | Ctrl+? |

---

## 3. Technology choice

### Decision: **Rust + GTK 4 + libadwaita**, UI written in **Blueprint**, built with **Meson + Cargo**, shipped as **Flatpak** (and an AUR package)

#### Why

| Need | How this stack covers it |
|---|---|
| Clean, modern look out of the box | libadwaita gives rounded, adaptive, animated widgets, light/dark & accent colours following the system, `AdwTabView`, toasts, dialogs, split views. We restyle it slightly toward Explorer with our own CSS. |
| Huge folders, smooth scrolling | GTK 4 `GtkColumnView` / `GtkGridView` are virtualized list widgets backed by `GListModel` — only visible rows are created, GPU-rendered. |
| Real Linux integration | **GIO/GVfs** for free: async file I/O, file monitoring, the Trash spec (`trash://`), recent files (`recent://`), network (`smb://`, `sftp://`, `dav://`, `ftp://`), phones (`mtp://`), volume monitor, MIME & default apps, file chooser and open-URI portals. This alone saves months of work. |
| Reliability | Rust's ownership model removes whole classes of crashes (use-after-free, data races) — important for software that moves people's files. |
| Ecosystem | `gtk4-rs` and `libadwaita-rs` are mature and used by many GNOME Circle apps (Fractal, Loupe, Snapshot…). |
| You already have it | `gtk4 4.24` and `libadwaita 1.10` are installed on this machine. |

#### Alternatives considered

| Option | Why not (for this project) |
|---|---|
| **Qt 6 / QML (C++)** | Excellent and very stylable, KIO integration is great on KDE. But C++ is less safe for file operations, and GIO-level features (GVfs backends, trash, portals) need extra glue on non-KDE desktops. Strong second choice. |
| **Tauri / Electron (web UI)** | Easiest to make look exactly like Windows, but slower with 100k-file folders, heavier memory, weak native drag & drop / clipboard / thumbnails integration. |
| **Slint / Iced (pure Rust UI)** | Promising, but missing mature virtualized grids, accessibility, DnD with other apps and system integration today. |
| **Python + GTK** | Fast to prototype, but slower for large directories and harder to keep the UI thread free. |

#### Main dependencies

| Crate / library | Purpose |
|---|---|
| `gtk4` (feature `v4_14`+), `libadwaita` (feature `v1_6`+) | UI |
| `gio`, `glib` | File system, async main loop, settings |
| `blueprint-compiler` (build tool) | Declarative `.blp` UI files → `.ui` |
| `tokio` *or* GLib async + `std::thread` pools | CPU-bound work (search, hashing, folder size) off the UI thread |
| `async-channel` | Worker → UI messaging |
| `serde`, `serde_json` / `toml` | Per-folder view metadata, undo journal |
| `ignore` + `grep-searcher` (ripgrep crates) | Fast recursive & content search |
| `natord` / `icu_collator` | Natural, locale-aware sorting |
| `compress-tools` (libarchive) | Archive browse / extract / compress |
| `gettext-rs` | Translations |
| `tracing`, `tracing-subscriber` | Logging |
| `sourceview5` | Text preview with syntax highlighting (P1) |
| `glycin` | Sandboxed image loading for previews/thumbnails |

---

## 4. Architecture

```
┌──────────────────────────────── UI layer (GTK main thread) ────────────────────────────────┐
│  Application ─► Window ─► AdwTabView ─► Tab (FolderPane)                                   │
│                   │                        ├── AddressBar (breadcrumb + entry)             │
│                   ├── Sidebar              ├── CommandBar                                  │
│                   ├── OperationsPopover    ├── FileView (ColumnView | GridView)            │
│                   └── DetailsPane          └── StatusBar                                   │
└────────────────────────────────────────────┬───────────────────────────────────────────────┘
                                             │ GObject properties, signals, actions
┌────────────────────────────── Model layer ─┴───────────────────────────────────────────────┐
│  DirectoryModel (GListModel of FileItem)  ◄── gio::FileMonitor (live updates)              │
│  SortListModel / FilterListModel / SelectionModel  (GTK built-ins, chained)                │
│  NavigationHistory · ViewSettings (per folder) · Bookmarks · Places (VolumeMonitor)        │
└────────────────────────────────────────────┬───────────────────────────────────────────────┘
                                             │ async calls / channels
┌──────────────────────────── Service layer ─┴───────────────────────────────────────────────┐
│  FileOps engine (job queue, copy/move/trash/delete, conflicts, progress, cancel)           │
│  UndoManager (journal of reversible operations)                                            │
│  ThumbnailService (freedesktop cache, background workers, LRU in-memory)                    │
│  SearchService (streaming recursive search)   ·   ClipboardService                         │
│  AppLauncher (MIME, default apps, portals)    ·   ArchiveService                           │
└────────────────────────────────────────────┬───────────────────────────────────────────────┘
                                             │
                         GIO / GVfs · UDisks2 · D-Bus · XDG portals · libarchive
```

### Key principles

1. **The UI thread never does blocking I/O.** Every disk/network call uses GIO's `*_future` async APIs or runs in a worker; results come back through the GLib main context.
2. **One `FileItem` GObject per file**, holding a cached `gio::FileInfo` (name, display name, size, mtime, MIME, icon, thumbnail state). Views bind to its properties, so when a thumbnail arrives or a file changes, only that row redraws.
3. **Models are composed, not rewritten**: `DirectoryModel → FilterListModel (hidden/search) → SortListModel → MultiSelection`. GTK does incremental sorting/filtering.
4. **Directory loading is streamed**: `enumerate_children_future` in batches of ~500, items appended as they arrive, so the first files show in milliseconds even for huge folders.
5. **Everything is a `gio::File` / URI**, so `smb://`, `trash:///`, `recent:///`, `mtp://` work through the same code path as local folders.
6. **Actions everywhere**: every command is a `gio::SimpleAction` (`win.new-folder`, `view.rename`, …). Toolbar buttons, menus and shortcuts all point at the same action, so enabling/disabling is centralized.
7. **File operations are jobs** with a state machine (`Queued → Running → (Paused) → Done | Failed | Cancelled`), observable from the operations popover, and each successful job registers an undo record.

---

## 5. Visual design system

The look: **libadwaita base + a light "Fluent" layer** (spacing, hover states, selection shape and command bar inspired by Windows 11).

### 5.1 Layout (default window 1200 × 760)

```
┌─────────────────────────────────────────────────────────────────────────────────────┐
│ [Documents ×] [Downloads ×] [+]                                        ─  □  ×      │  ← tab strip in header
│ ←  →  ↑  ⟳ │ 🏠 › Documents › Projects ›                        │ 🔍 Search Projects │  ← nav + breadcrumb + search
├─────────────────────────────────────────────────────────────────────────────────────┤
│ ⊕ New ▾ │ ✂  ⧉  📋  ✎  ⇪  🗑 │  ⇅ Sort ▾   ▦ View ▾   ⋯                  ◧ Details │  ← command bar
├──────────────┬──────────────────────────────────────────────────┬───────────────────┤
│ 🏠 Home       │ Name               Date modified   Type     Size │   [ thumbnail ]   │
│ 🕘 Recent     │ 📁 assets          Today 14:02     Folder        │                   │
│              │ 📁 src             Yesterday       Folder        │   report.pdf      │
│ PINNED        │ 📄 README.md       03/10/2026      Markdown 4 KB │   PDF document    │
│ 📁 Projects   │ 📄 report.pdf      01/10/2026      PDF    1.2 MB │   1.2 MB          │
│ 📁 Desktop    │ 🖼 cover.png        28/09/2026      PNG    640 KB │   Modified …      │
│              │                                                  │                   │
│ THIS PC       │                                                  │                   │
│ 💽 System ▓▓░ │                                                  │                   │
│ 🔌 USB Key ⏏  │                                                  │                   │
│ 🗑 Trash      │                                                  │                   │
├──────────────┴──────────────────────────────────────────────────┴───────────────────┤
│ 5 items · 1 selected (1.2 MB)                                      214 GB free      │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

### 5.2 Design tokens

| Token | Value |
|---|---|
| Base spacing unit | 4 px (use 4 / 8 / 12 / 16 / 24) |
| Corner radius | 6 px rows & buttons, 8 px cards/thumbnails, 12 px dialogs & popovers |
| Row height (details view) | 32 px (comfortable), 26 px (compact setting) |
| Sidebar width | 220 px default, 180–360 px resizable |
| Font | System UI font (Cantarell / Inter / Adwaita Sans) — 10.5 pt body, 9 pt secondary text, numeric columns use tabular figures |
| Icons | Adwaita symbolic icons for UI (16 px), full-colour MIME icons for files |
| Hover | Accent colour at 6 % opacity, row radius 6 px, 2 px inset from edges |
| Selection | Accent colour at 15 % opacity + 3 px accent pill on the left edge (Explorer-style) |
| Focus | 2 px accent outline, offset 1 px |
| Animations | 150 ms ease-out (hover, pane toggle), 200 ms for popovers & tab open, respects "reduce motion" |
| Secondary text | `@window_fg_color` at 60 % opacity (dates, sizes, types) |

### 5.3 Styling rules

- Use libadwaita named colours (`@accent_bg_color`, `@window_bg_color`, `@view_bg_color`, `@headerbar_bg_color`) so light/dark/accent changes are automatic — **never hard-code colours**.
- Sidebar uses the `.navigation-sidebar` style with a slightly tinted background (`AdwOverlaySplitView` sidebar style).
- The file area is a flat `@view_bg_color` surface; no grid lines, no zebra striping.
- Column headers are subtle: secondary text colour, no borders, sort arrow only on the active column.
- Command bar buttons are flat with icon + label for main actions, icon-only for the clipboard group (tooltips on all).
- Empty states use `AdwStatusPage` with a symbolic icon and a single helpful sentence.
- Toasts (`AdwToastOverlay`) for "3 items moved to Trash — Undo".
- The whole UI must look right at 100 %, 125 %, 150 %, 200 % scaling.

---

## 6. Project structure

```
lia-files/
├── Cargo.toml
├── meson.build                     # builds resources, schemas, translations, then cargo
├── meson_options.txt
├── build-aux/
│   ├── io.github.lia.Files.json    # Flatpak manifest
│   └── PKGBUILD                    # AUR package
├── data/
│   ├── io.github.lia.Files.desktop.in
│   ├── io.github.lia.Files.metainfo.xml.in
│   ├── io.github.lia.Files.gschema.xml      # GSettings (preferences)
│   ├── icons/hicolor/scalable/apps/io.github.lia.Files.svg
│   ├── resources/
│   │   ├── resources.gresource.xml
│   │   ├── style.css                        # Fluent layer on top of libadwaita
│   │   ├── style-dark.css
│   │   └── ui/                              # Blueprint files
│   │       ├── window.blp
│   │       ├── folder-pane.blp
│   │       ├── sidebar.blp
│   │       ├── address-bar.blp
│   │       ├── command-bar.blp
│   │       ├── details-pane.blp
│   │       ├── properties-dialog.blp
│   │       ├── conflict-dialog.blp
│   │       ├── preferences.blp
│   │       └── shortcuts.blp
├── po/                                      # translations
├── src/
│   ├── main.rs
│   ├── application.rs                       # AdwApplication, actions, CLI args
│   ├── config.rs                            # generated: APP_ID, VERSION, paths
│   ├── window/                              # main window + tab management
│   ├── widgets/
│   │   ├── folder_pane/                     # one tab: address bar + view + status
│   │   ├── file_view/                       # details (ColumnView) & grid (GridView)
│   │   ├── address_bar/
│   │   ├── sidebar/
│   │   ├── command_bar/
│   │   ├── details_pane/
│   │   └── operations_popover/
│   ├── dialogs/                             # properties, conflict, open-with, rename, connect-server
│   ├── model/
│   │   ├── file_item.rs                     # GObject wrapping gio::FileInfo
│   │   ├── directory_model.rs               # streaming loader + FileMonitor
│   │   ├── sorting.rs                       # natural/locale comparators, folders-first
│   │   ├── history.rs
│   │   ├── places.rs                        # XDG dirs, bookmarks, volumes
│   │   └── view_settings.rs                 # per-folder view memory
│   ├── services/
│   │   ├── file_ops/                        # jobs: copy, move, trash, delete, mkdir, rename, link
│   │   ├── undo.rs
│   │   ├── thumbnails.rs
│   │   ├── search.rs
│   │   ├── clipboard.rs
│   │   ├── launcher.rs
│   │   └── archives.rs
│   └── util/                                # formatting (sizes, dates), errors, async helpers
└── tests/
    ├── file_ops.rs
    ├── sorting.rs
    └── undo.rs
```

---

## 7. Implementation steps

Each phase ends with something you can run and use. Estimated effort assumes one developer working part-time; adjust freely.

### Phase 0 — Environment & skeleton (≈ 2 days)

1. Install the toolchain (CachyOS / Arch):
   ```fish
   sudo pacman -S rustup meson ninja blueprint-compiler gtk4 libadwaita gtksourceview5 libarchive flatpak-builder
   rustup default stable
   ```
2. Create the project with the GNOME Rust template layout (section 6): `Cargo.toml`, `meson.build`, `src/main.rs`, `config.rs.in`.
3. Add dependencies: `gtk4 = { version = "0.10", package = "gtk4", features = ["v4_16"] }`, `adw = { version = "0.8", package = "libadwaita", features = ["v1_6"] }`, `gettext-rs`, `tracing`.
4. Set up GResources: compile `.blp` → `.ui` with Blueprint, bundle `style.css` and icons; load them in `main.rs`.
5. Create the GSettings schema (`window-width`, `window-height`, `is-maximized`, `default-view`, `show-hidden`, `sort-folders-first`, `icon-size`, `sidebar-width`).
6. `AdwApplication` + empty `AdwApplicationWindow` that saves/restores its size.
7. Add `rustfmt`, `clippy`, a `justfile` (or `Makefile`) with `run`, `lint`, `test`.
8. `git init`, `.gitignore`, first commit.

**Done when:** `meson setup build && ninja -C build && ./build/src/lia-files` opens an empty, correctly themed window.

### Phase 1 — Read-only browsing (≈ 1–2 weeks) — *the heart of the app*

1. **`FileItem` GObject** — wraps `gio::File` + `gio::FileInfo`; properties: `display-name`, `size`, `modified`, `content-type`, `icon`, `is-dir`, `is-hidden`, `thumbnail`.
2. **`DirectoryModel`** implementing `GListModel`:
   - `load(file)` cancels the previous load (`gio::Cancellable`), calls `enumerate_children_future` with the attribute list `standard::*,time::modified,time::created,unix::mode,owner::user,thumbnail::*`, and appends batches of 500.
   - Attach a `gio::FileMonitor` and handle `Created`, `Deleted`, `Changed`, `Renamed`, `MovedIn`, `MovedOut` incrementally (with a 100 ms coalescing timer).
   - Expose `loading` and `error` properties for the UI.
3. **Sorting & filtering** — `gtk::CustomSorter` with natural, case-insensitive, locale-aware comparison; "folders first" via `gtk::MultiSorter`; `gtk::CustomFilter` for hidden files.
4. **Details view** — `gtk::ColumnView` with `SignalListItemFactory` per column (icon+name, date, type, size). Sizes/dates formatted in `util/format.rs` ("Today 14:02", "1.2 MB"). Click headers to sort.
5. **Grid view** — `gtk::GridView` with icon + two-line ellipsized name, sizes 48 / 96 / 160 px; Ctrl+scroll zoom.
6. **Navigation** — `NavigationHistory` (back/forward stacks), Up button, Enter/double-click on folders, Backspace/Alt+← to go back.
7. **Address bar** — `gtk::Stack` switching between a breadcrumb (`gtk::Box` of flat buttons with `›` separators, overflowing segments collapsed into a `…` menu) and a `gtk::Entry` with path completion (async `enumerate_children` of the parent dir).
8. **Opening files** — `gtk::FileLauncher` (portal-aware) for default app; `gio::AppInfo::all_for_type` for the "Open with" list.
9. **Status bar** — item count, selection count & size, free space (`query_filesystem_info_future`).
10. **Empty / error states** — `AdwStatusPage` for empty folder, permission denied, not found.

**Done when:** you can browse your whole home folder, network folders mounted via GVfs, and a 100k-file test folder scrolls smoothly.

### Phase 2 — Window shell: sidebar, tabs, command bar, styling (≈ 1 week)

1. **Main layout** — `AdwToolbarView` (header + command bar as top bars, status bar as bottom bar) inside `AdwOverlaySplitView` (sidebar) inside `AdwToastOverlay`.
2. **Tabs** — `AdwTabView` + `AdwTabBar` placed in the header bar; each page holds a `FolderPane`. Implement new/close/reorder, middle-click close, "Open in new tab", drag a tab out to create a window (`create-window` signal).
3. **Sidebar** — `gtk::ListBox` with `.navigation-sidebar` style, sections: Home/Recent, Pinned (XDG dirs + GTK bookmarks file, watched with a FileMonitor), This PC (from `gio::VolumeMonitor`: mounts with `gtk::LevelBar` usage and eject button), Network, Trash (`trash:///` with item count). Highlight the entry matching the current folder.
4. **Command bar** — `gtk::Box` of flat buttons and `gtk::MenuButton`s bound to actions; buttons disable automatically from action state (e.g. Paste disabled when clipboard has no files).
5. **Actions & shortcuts** — define all actions in `application.rs` / `window` / `folder_pane` action groups and register accelerators from section 2.14. Add the shortcuts window (`AdwShortcutsDialog` or `gtk::ShortcutsWindow` from `shortcuts.blp`).
6. **Fluent CSS layer** — implement section 5 tokens in `style.css`: row hover/selection pill, radii, spacing, subtle column headers, command bar, sidebar tint. Test in light, dark and all accent colours.
7. **Session state** — save open tabs and current paths on close; restore on start (optional setting).

**Done when:** the app looks like the mockup in 5.1 and feels finished for browsing.

### Phase 3 — File operations engine (≈ 2 weeks) — *the most safety-critical part*

1. **Job model** — `FileOpJob { id, kind, sources, destination, state, progress, bytes_total, bytes_done, current_file, error }` as a GObject so the popover can bind to it.
2. **Job queue** — run jobs concurrently (one per destination device, configurable), with pause/resume/cancel through `gio::Cancellable`.
3. **Operations** (all async, all via GIO so they work on network & MTP too):
   - `copy` — scan sources first to compute total size; recursive copy with `gio::File::copy_future` + progress callback; preserve permissions & mtime (`FileCopyFlags::ALL_METADATA`).
   - `move` — try `move_future` (instant rename on same filesystem), fall back to copy + delete across devices.
   - `trash` — `trash_future`; if unsupported (some network mounts), ask to delete permanently.
   - `delete` — recursive `delete_future` after confirmation.
   - `mkdir`, `create file`, `rename` (`set_display_name_future`), `symlink`, `duplicate` ("name (copy).ext").
4. **Conflict handling** — when the target exists, pause the job and show `conflict-dialog` (thumbnails, sizes, dates of both files): Replace · Skip · Keep both (auto-rename "name (2)") · "Do this for all conflicts".
5. **Error handling** — per-file errors (permission denied, disk full, file name too long, invalid characters on FAT/NTFS) → dialog with Retry / Skip / Skip all / Cancel. Never leave half-written files: copy to `.name.part` then rename on completion.
6. **Operations popover** — header button that appears while jobs run, showing a progress ring; popover lists jobs with progress bar, speed, ETA, pause/cancel; finished jobs fade out; a toast when done.
7. **Undo manager** — every completed job pushes an inverse record (`rename back`, `move back`, `restore from trash`, `trash created copies`). Ctrl+Z / Ctrl+Y; toast "Moved 3 items to Trash — Undo". Permanent delete is not undoable (hence the confirmation).
8. **Clipboard** — write `x-special/gnome-copied-files` + `text/uri-list` + plain text for compatibility with Nautilus, Dolphin and terminals; read both formats on paste. Cut items show dimmed.
9. **Drag & drop** — `gtk::DragSource` on views providing `gdk::FileList`; `gtk::DropTarget` on views, folders, sidebar entries and tabs; Shift = move, Ctrl = copy, Ctrl+Shift = link, default = move on same device / copy across devices (Explorer behaviour); hover a folder for 800 ms to open it.
10. **Inline rename** — `gtk::EditableLabel`/popover over the item, pre-selecting the name without extension; validate characters and show an inline error.
11. **New ▾ menu** — New folder, New empty file, and entries from `~/Templates`.
12. **Trash view** — `trash:///` with extra columns "Original location" and "Date deleted", "Restore" and "Empty trash" buttons in the command bar.

**Done when:** you can manage files day-to-day and every test in `tests/file_ops.rs` passes (see section 8).

### Phase 4 — Thumbnails & details pane (≈ 1 week)

1. **ThumbnailService** — follows the freedesktop thumbnail spec: look up `~/.cache/thumbnails/{normal,large,x-large}/<md5(uri)>.png`; if missing and file is local & under size limit, generate with `glycin` (images) or by spawning the system thumbnailers listed in `/usr/share/thumbnailers/*.thumbnailer` (videos, PDFs, fonts); write the PNG with `Thumb::URI` / `Thumb::MTime` keys; record failures in `fail/`.
2. Worker pool (2–4 threads), priority to items currently visible (queue is reordered on scroll), in-memory LRU of `gdk::Texture`s.
3. Bind thumbnails to the `FileItem::thumbnail` property; fade in when ready.
4. **Details pane** — `AdwOverlaySplitView`/`gtk::Paned` on the right: big preview, name, metadata list (`AdwActionRow`s); image dimensions; text preview with `sourceview5` (read-only, first 200 KB); multi-selection summary.

### Phase 5 — Search (≈ 1 week)

1. Instant filter: typing in the search entry updates a `gtk::StringFilter` on the current model (debounced 120 ms).
2. Enter (or a "Search in subfolders" button) starts `SearchService`: walk with the `ignore` crate on a thread (respecting hidden setting), stream matches through a channel into a dedicated `SearchResultsModel`, shown in the details view with an extra "Folder" column.
3. Filter chips (`gtk::FlowBox` of toggle buttons) for Kind / Date / Size.
4. Cancel automatically when the query changes or the tab navigates away.
5. (P2) Content search using `grep-searcher`; (P2) query Tracker/LocalSearch over D-Bus when available.

### Phase 6 — Properties, Open with, devices, network (≈ 1–2 weeks)

1. **Properties dialog** (`AdwDialog` with `AdwViewStack` tabs General / Permissions / Open with): async recursive size counter that updates live; permissions editing via `set_attribute_uint32("unix::mode")`; owner/group display.
2. **Open with dialog** — list `AppInfo`s for the MIME type, "Set as default" via `set_as_default_for_type`.
3. **Volumes** — mount/unmount/eject through `gio::Volume` / `gio::Mount` with `gtk::MountOperation` (handles password and LUKS prompts); show a toast "Safe to remove".
4. **"This PC" page** — card grid of drives with usage bars, opened from the sidebar header.
5. **Connect to server** dialog — URI entry with examples (`smb://`, `sftp://`, `dav://`), recent servers list, mounts via `mount_enclosing_volume_future`.
6. **Open terminal here** — detect terminal via `xdg-terminal-exec`, fallback list (kgx, gnome-terminal, konsole, alacritty, kitty, wezterm).

### Phase 7 — Archives, batch rename, polish features (≈ 1–2 weeks)

1. `ArchiveService` with libarchive (`compress-tools`): Extract here / Extract to…, Compress… dialog (format, name), as file-op jobs with progress.
2. Batch rename dialog with live preview table (pattern `{name}_{n:03}`, find/replace, case change).
3. Group-by in details view using `gtk::SortListModel` section sorter + `ColumnView` header factory (GTK ≥ 4.12 sections).
4. Per-folder view settings stored in `~/.local/share/lia-files/views.json` (or GIO metadata `metadata::lia-view` on local files).
5. Quick Look overlay (Space).
6. Preferences dialog (`AdwPreferencesDialog`) for everything in 2.12.

### Phase 8 — Quality pass (≈ 1 week, then continuous)

1. **Accessibility** — accessible labels on all icon-only buttons, test with Orca, full keyboard-only walkthrough.
2. **Performance** — profile with `sysprof` and the GTK inspector (Ctrl+Shift+D); verify targets in section 10.
3. **i18n** — extract strings with `xgettext`, add `po/fr.po` (and others), test RTL with `GTK_DEBUG=...`/`LANGUAGE=ar`.
4. **Edge cases** — unreadable folders, broken symlinks, names with newlines/invalid UTF-8, read-only filesystems, full disks, disconnected network mid-copy, USB unplugged mid-copy, 255-byte names, FAT/NTFS forbidden characters.
5. **Crash safety** — undo journal persisted to disk; `.part` files cleaned on next start.

### Phase 9 — Packaging & release (≈ 3 days)

1. Desktop file, AppStream metainfo with screenshots, app icon (follow GNOME HIG icon guidelines).
2. Register as handler for `inode/directory` so "Show in folder" from other apps opens Lia Files (and implement the `org.freedesktop.FileManager1` D-Bus interface: `ShowFolders`, `ShowItems`, `ShowItemProperties`).
3. Flatpak manifest (GNOME runtime, `--filesystem=host`, `--talk-name=org.gtk.vfs.*`, `--talk-name=org.freedesktop.UDisks2`) and an AUR `PKGBUILD` for native install on CachyOS.
4. CI (GitHub Actions / Forgejo): fmt, clippy, tests, Flatpak build.
5. Tag v1.0.

### Milestone summary

| Milestone | Phases | Result |
|---|---|---|
| **M1 — "It browses"** | 0–1 | Fast read-only browser |
| **M2 — "It looks right"** | 2 | Clean modern Explorer-like shell |
| **M3 — "Daily driver"** | 3–4 | Safe file management + thumbnails → usable as your main file manager |
| **M4 — "Complete"** | 5–7 | Search, properties, devices, archives |
| **v1.0** | 8–9 | Polished, accessible, packaged |

---

## 8. Testing strategy

| Level | What | How |
|---|---|---|
| Unit | Sorting, size/date formatting, path parsing, rename validation, undo inverse generation | `cargo test`, plain Rust functions kept free of GTK where possible |
| Integration | File operations on a real temp tree: copy/move/trash/delete/conflicts/cancel/undo, cross-device moves (tmpfs ↔ disk) | `tests/file_ops.rs` using `tempfile`, run inside a GLib main context |
| Model | `DirectoryModel` reacts correctly to external create/delete/rename (FileMonitor) | Integration test that touches files and asserts model contents |
| UI smoke | App starts, opens a folder, switches views, opens tabs | Headless run with `GDK_BACKEND=broadway` or under `xvfb-run` / `weston --backend=headless` |
| Manual checklist | Look & feel in light/dark, all accent colours, 100–200 % scaling, keyboard-only, Orca | Checklist in `docs/qa.md` before each release |
| Stress | 100 000 files folder, 50 GB copy, 10 parallel jobs | Script in `tools/gen-test-tree.sh` |

---

## 9. Packaging & distribution

- **Flatpak** (Flathub) — primary channel, works on every distro.
- **AUR** (`lia-files` and `lia-files-git`) — native package for Arch/CachyOS users; integrates better with system-wide thumbnailers and admin access.
- Version with SemVer, changelog in metainfo `<releases>`.

---

## 10. Performance targets

| Metric | Target |
|---|---|
| Cold start to first window | < 300 ms |
| Open folder with 1 000 items (local SSD) | first items < 50 ms, complete < 150 ms |
| Open folder with 100 000 items | first items < 100 ms, UI stays at 60 fps while loading |
| Scrolling | 60 fps (120 fps on high-refresh displays) in details and grid view |
| Memory | < 120 MB with 5 tabs and thumbnails |
| Thumbnail of visible items | < 500 ms after scrolling stops (cache miss) |
| Search | first results < 200 ms on home folder |

---

## 11. Future ideas

- Dual-pane "commander" mode
- Folder tree in the sidebar
- Cloud drives (Google Drive via GOA/GVfs, Nextcloud via WebDAV)
- Tags & colour labels (stored in `xattr` `user.xdg.tags`)
- Plugin / custom actions API (scripts on the context menu)
- Git status badges on files in repositories
- Integrated file previewers for 3D models, Markdown rendering, archives
- Mobile/adaptive layout (libadwaita breakpoints) for Linux phones and tablets
