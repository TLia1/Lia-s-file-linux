# Theming Lia Files for Penumbra

What could make Lia Files customisable enough to sit naturally inside the Penumbra desktop, and what each side (this app, the theme) would have to do. Written against Lia Files at `ec2fa54` and Penumbra at `d40b2f8`, libadwaita 1.10 and GTK 4.24.

## Where things stand

Lia Files already gets a lot for free. `data/resources/style.css` uses only libadwaita's named colours, Penumbra writes those colours into `~/.config/gtk-4.0/gtk.css`, and the colour-scheme setting defaults to `system`, which follows the `color-scheme` that `penumbra-theme` sets through GSettings. Fonts (Inter) and icons (Penumbra/Papirus with violet folders) come from GTK settings too.

The gaps:

| # | Gap | Why it matters |
|---|---|---|
| 1 | **`--accent-color` and `--error-color` are not Penumbra's.** libadwaita derives them from the background colours in OKLab, ignoring the `@define-color accent_color` Penumbra sets. Checked on this machine: `--accent-bg-color` is Penumbra's `#916dcf`, but `--accent-color` comes out `#dbb7ff` (Penumbra wants `#b69af7`) and `--error-color` `#ff938c` (Penumbra's rose is `#ef8fa4`). | Group headers, focus rings, the "This PC" heading hover and the batch-rename errors use these. The same affects every libadwaita app, not only this one. |
| 2 | **Colours probably go stale after a theme change.** `penumbra-theme` rewrites `gtk.css`, but GTK reads the user stylesheet once at startup. Light/dark flips at once through libadwaita while the hex colours from `gtk.css` stay the old mode's. *Not tested yet:* open a window, run `penumbra-theme set mode=light`, look. | This app is the default file manager and stays open for a long time; a half-switched window looks broken. |
| 3 | **Radii are hard-coded** (4, 6, 8, 12, 14 px across ~25 rules). | Penumbra has a Rounded/Sharp switch (`penumbra-corners`) that the shell, Hyprland, rofi, hyprlock and Satty follow. Lia Files would stay rounded inside square windows. |
| 4 | **No translucency.** The window is opaque. | The shell is built around glass (surfaces at 72–88 % over Hyprland's blur). Hyprland already blurs translucent windows (`blur.enabled = true`, `popups = true`). |
| 5 | **Text previews use the Adwaita GtkSourceView schemes and Adwaita Mono.** `quick_look.rs:119` and `details_pane/mod.rs:400` hard-code `Adwaita` / `Adwaita-dark`; Quick Look also picks it once and doesn't follow a light/dark change. | Penumbra has its own colours for bat, Neovim, VS Code; previews would be the one place with GNOME blue highlighting. |
| 6 | **Different motion.** 150 / 200 / 600 ms here; Penumbra uses 160 / 260 / 420 ms. | Small, but a fade that runs at a different speed from everything around it reads as "another app". |
| 7 | **Penumbra still assumes Dolphin.** Super+E falls back to `org.kde.dolphin`, `install.sh` defaults to it, the Arch package list installs it, and "Set as Wallpaper" exists only as a Dolphin service menu. | Lia Files is the default here, but the theme doesn't know it exists. |

## The idea in one line

**Lia Files exposes design tokens and loads a user stylesheet that it watches; Penumbra renders that stylesheet from its palette and rewrites it when the look changes.** Lia Files stays theme-agnostic (nothing in it knows about Penumbra), and Penumbra treats it like any other app it colours, through a template under `palette/templates/apps/`.

```
palette.toml ──build-palette.py / penumbra-theme──▶ ~/.config/lia-files/penumbra.css ─┐
penumbra-corners ──────────────────────────────────▶ ~/.config/lia-files/corners.css  ─┤ FileMonitor
                                                                                       ▼
                                                     Lia Files reloads its user CSS provider
```

The alternative, Lia Files reading `~/.local/state/penumbra/palette.json` and `corners` directly, would be less code today but ties the app to one theme. The stylesheet route also gives anyone else (another theme, a user tweak) the same hooks.

## In Lia Files

### 1. Design tokens in `style.css`

Turn the repeated numbers into custom properties on `:root`, with today's values as defaults, so a user stylesheet can change one line instead of overriding 25 selectors. GTK ≥ 4.16 supports `var()` and `color-mix()`.

```css
:root {
  /* Shape */
  --lia-radius-small: 4px;   /* status bar buttons, usage bars */
  --lia-radius: 6px;         /* buttons, rows, crumbs, address bar */
  --lia-radius-large: 8px;   /* grid tiles, drive cards, previews */
  --lia-radius-popover: 12px;
  --lia-radius-chip: 14px;

  /* Density */
  --lia-row-height: 32px;
  --lia-control-height: 32px;

  /* Selection and hover */
  --lia-hover-bg: color-mix(in srgb, var(--accent-bg-color) 6%, transparent);
  --lia-selected-bg: color-mix(in srgb, var(--accent-bg-color) 15%, transparent);
  --lia-selected-hover-bg: color-mix(in srgb, var(--accent-bg-color) 20%, transparent);
  --lia-selection-marker: 3px;   /* the pill on the left; 0 to turn it off */

  /* Surfaces */
  --lia-window-opacity: 1;       /* < 1 for glass, see §4 */

  /* Motion */
  --lia-fast: 150ms;
  --lia-normal: 200ms;
  --lia-slow: 600ms;
}

columnview.details-view > listview > row {
  min-height: var(--lia-row-height);
  border-radius: var(--lia-radius);
  transition: background-color var(--lia-fast) ease-out;
}

columnview.details-view > listview > row:selected {
  background-color: var(--lia-selected-bg);
  box-shadow: inset var(--lia-selection-marker) 0 0 0 var(--accent-bg-color);
}
```

Worth checking while doing it: whether GTK accepts `var()` inside `alpha()`. If it does, `alpha(var(--accent-bg-color), var(--lia-hover-alpha))` keeps the current style; `color-mix()` works either way.

### 2. A user stylesheet, reloaded when it changes

Load every `*.css` in `$XDG_CONFIG_HOME/lia-files/` (sorted by name, so `corners.css` comes after `colors.css`) into one `gtk::CssProvider` at `STYLE_PROVIDER_PRIORITY_USER`, after the app's own stylesheet. Watch the folder with `gio::File::monitor_directory` and reload the provider on change, debounced by ~100 ms since a theme switch writes several files.

This fixes gap 2 for anything the stylesheet sets, and it's the hook for everything else here. Inside Flatpak the folder is the sandbox's own config dir, so Penumbra would also need `--filesystem=xdg-config/lia-files:ro` or to write into `~/.var/app/io.github.lia.Files/config/lia-files/`; worth one line in the manifest.

A "Reload Style" action (Ctrl+Shift+R, only when `GTK_DEBUG` or a debug setting is on) is handy while writing themes.

### 3. Appearance settings

New keys in `io.github.lia.Files.gschema.xml`, shown in Preferences → Appearance next to the colour scheme. Each one just sets a token, through a small generated provider at a priority between the app's CSS and the user's, so a theme stylesheet can still override it.

| Key | Values | What it sets |
|---|---|---|
| `corners` | `rounded`, `sharp` | the `--lia-radius-*` tokens to their defaults or `0` |
| `density` | `compact`, `normal`, `comfortable` | `--lia-row-height` 28 / 32 / 40, padding |
| `window-opacity` | 0.6–1.0, default 1 | `--lia-window-opacity` (glass) |
| `selection-style` | `marker`, `fill`, `outline` | marker width, fill alpha, outline |
| `accent-override` | empty or `#rrggbb` | `--accent-bg-color` and a derived `--accent-color` |

With a Penumbra stylesheet present these mostly don't need touching, but they make the app fit other desktops too. Penumbra's `corners.css` would win over the `corners` setting, which is the right order: the desktop-wide switch should be the one that matters.

### 4. Glass

Translucent surfaces so Hyprland's blur shows through, as in the shell:

```css
window.lia-window {   /* add a "lia-window" style class to the template in window.blp */
  background-color: color-mix(in srgb, var(--window-bg-color) calc(var(--lia-window-opacity) * 100%), transparent);
}
lia-sidebar, .nav-row, .command-bar { background: none; }   /* let the window's glass show */
.file-view, this-pc { background-color: color-mix(in srgb, var(--view-bg-color) …); }
```

Things to decide:
- **Which layers.** Glass on the sidebar and toolbars with a near-opaque file area (≈ 0.92) reads best; a fully translucent file list with busy wallpapers hurts legibility, especially thumbnails and secondary text.
- **Light mode.** Penumbra's shell uses denser glass in light mode (0.78 vs 0.72); the tokens can follow it if Penumbra's stylesheet sets them per mode.
- **Popovers and dialogs** stay opaque; Hyprland's `popups = true` blurs them only if they have alpha.
- If `calc()` inside `color-mix()` isn't accepted, write the percentage as its own token (`--lia-window-glass: 85%`).

### 5. Text previews

- Prefer a GtkSourceView scheme named after the theme when one is installed: look up `penumbra` / `penumbra-light` (or, more generally, a `text-scheme-dark` / `text-scheme-light` setting defaulting to `Adwaita-dark` / `Adwaita`), falling back to Adwaita.
- Make Quick Look follow `StyleManager::dark` like the details pane already does (`update_text_scheme`).
- Use `org.gnome.desktop.interface monospace-font-name` for previews instead of GtkSourceView's default; Penumbra can then set JetBrainsMono there once for every GTK app.

### 6. Motion

Route every transition and animation through the `--lia-fast/normal/slow` tokens, and turn them off when `gtk-enable-animations` is false (GTK does this for its own transitions; the thumbnail fade-in keyframes are ours).

### 7. Custom actions

Penumbra's "Set as Wallpaper" lives in a Dolphin service menu. A generic version here: read `*.desktop` files from `$XDG_DATA_HOME/lia-files/actions/` (and the system data dirs) with `MimeType=` and `Exec=` (`%F`/`%U`), and list matching ones in the context menu under a separator. Close enough to KDE service menus that Penumbra's existing file needs only a renamed copy.

## In Penumbra

### 1. A template for Lia Files

`palette/templates/apps/lia-files/config/lia-files/penumbra.css`, installed only when the `lia-files` command exists (it's already the folder name, so no `APP_COMMANDS` entry). It sets libadwaita's variables directly, which also fixes gap 1 for this app, plus the tokens that differ:

```css
/* PENUMBRA colours for Lia Files. Generated by tools/build-palette.py; do not edit. */
:root {
  --accent-color: {{accent}};
  --accent-bg-color: {{selection}};
  --accent-fg-color: {{selectionText}};
  --error-color: {{rose}};
  --window-bg-color: {{surface}};
  --view-bg-color: {{base}};
  --sidebar-bg-color: {{surfaceAlt}};
  /* … the rest of gtk.css's list, as variables … */

  --lia-hover-bg: {{hover}};
  --lia-fast: 160ms;
  --lia-normal: 260ms;
  --lia-slow: 420ms;
}
```

`penumbra-theme` already rewrites installed templates in the chosen mode and accent, and Lia Files reloads on its own (§2 above), so `reload_apps` needs nothing new.

### 2. Fix `--accent-color` for every libadwaita app

The same lines (`:root { --accent-color: …; --error-color: …; --destructive-color: …; --success-color: …; --warning-color: …; }`) in `palette/templates/config/gtk-4.0/gtk.css` give every libadwaita app Penumbra's text-on-surface accent instead of libadwaita's derived one. Keep the `@define-color` lines for older apps. This is independent of Lia Files and could go in first.

### 3. Corners

In `bin/penumbra-corners`, next to the rofi block:

```bash
if [[ -d $config/lia-files ]]; then
 if [[ $current = sharp ]]; then
  echo ':root { --lia-radius-small: 0; --lia-radius: 0; --lia-radius-large: 0; --lia-radius-popover: 0; --lia-radius-chip: 0; }' > "$config/lia-files/corners.css"
 else
  echo '/* Rounded: Lia Files as it is. Written by penumbra-corners. */' > "$config/lia-files/corners.css"
 fi
fi
```

And libadwaita's own window corners: `window.csd { border-radius: 0; }` in the same file for sharp mode, so the window outline matches Hyprland's `rounding = 0`.

### 4. Glass to match the shell

In the template, `--lia-window-opacity` from the shell's levels (0.72 dark, 0.78 light, or a bit more for a file list). Optional: a "Glass windows" switch on the Look page that writes `1` instead.

### 5. Source view schemes

`palette/templates/share/gtksourceview-5/styles/penumbra.xml` (and `penumbra-light.xml`), built from the same colours as bat's `Penumbra.tmTheme`. Any GtkSourceView app can use them, and Lia Files would pick them up through §5.

### 6. Make it the file manager

Keeping the multi-distro rule in mind (only packaging goes in `distro/`):
- `install.sh`: `set_default file_manager io.github.lia.Files` when `lia-files` is installed, Dolphin otherwise; the `--file-manager` help text; `hyprland.lua`'s fallback.
- `distro/arch/distro.sh`: Lia Files isn't in the Arch repos, so either leave Dolphin in `PACKAGES` as the fallback, or document building `build-aux/PKGBUILD` (or the Flatpak) in the README.
- Ship `share/lia-files/actions/penumbra-wallpaper.desktop` once §7 exists, and mention it next to the Dolphin one in the README.
- `gsettings set org.gnome.desktop.interface monospace-font-name 'JetBrainsMono Nerd Font 10'` alongside the other GSettings in step 6 of the install.

## Suggested order

1. **Penumbra: `--accent-color` fix in `gtk.css`** (§Penumbra 2): small, helps every app, no Lia Files change needed.
2. **Lia Files: tokens + watched user stylesheet** (§1, §2): everything else builds on it. Then check gap 2 for real.
3. **Penumbra: the Lia Files template and corners** (§Penumbra 1, 3).
4. **Glass** on both sides, tuned by eye against the dawn and eclipse wallpapers in both modes.
5. **Text previews, motion, custom actions.**
6. **Switch the default file manager** in the installer.

## How gap 1 was checked

A small PyGObject window under Penumbra's `gtk.css` resolved `color: var(--x)` and `color: @x` for each colour and printed both. Every surface and `--accent-bg-color` matched Penumbra; `--accent-color` and `--error-color` came out as libadwaita's OKLab-derived values (`--accent-color` red/blue channels above 1.0 before clamping) while the `@`-named versions were Penumbra's.
