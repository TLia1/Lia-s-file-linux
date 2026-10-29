# Themes and desktop integration

Lia Files follows libadwaita's system style, icon theme and desktop monospace
font. Preferences → General → Appearance controls corners, density, selection
style, window opacity, an optional `#rrggbb` accent and GtkSourceView preview
scheme IDs. Invalid or unavailable preview schemes fall back to Adwaita.
Quick Look and the details pane both follow style and scheme changes.

Put theme CSS in `$XDG_CONFIG_HOME/lia-files/` (normally
`~/.config/lia-files/`). All regular `*.css` files are combined in filename
order. They override the application and its appearance preferences. The
directory is watched, with a 100 ms debounce, including atomic file replacements.
Deleting a stylesheet removes its overrides; invalid CSS keeps the last valid
combined stylesheet and logs the parser error. Debug builds, or launches with
`GTK_DEBUG` set, also provide Ctrl+Shift+R to reload styles.

The defaults and public tokens are at the top of
[`data/resources/style.css`](../data/resources/style.css). For example:

```css
:root {
  --lia-radius: 0px;
  --lia-radius-large: 0px;
  --lia-row-height: 40px;
  --lia-selected-bg: color-mix(in srgb, var(--accent-bg-color) 18%, transparent);
  --lia-fast: 160ms;
  --lia-normal: 260ms;
  --lia-slow: 420ms;
  --lia-window-opacity: 0.85;
  --lia-view-opacity: 0.98;
}
```

Opacity applies to surface colours rather than the widget tree, so text and
thumbnails remain opaque. Dialogs and popovers remain opaque. Glass needs a
compositor that supports transparent windows; blur is configured by the desktop.
GTK's `gtk-enable-animations` setting disables application CSS transitions and
thumbnail keyframes as well as GTK's own animations.

Inside Flatpak, the configuration directory is
`~/.var/app/io.github.lia.Files/config/lia-files/`. Themes should write there too;
no additional host configuration permission is necessary. GtkSourceView schemes
belong in `~/.local/share/gtksourceview-5/styles/`, or
`~/.var/app/io.github.lia.Files/data/gtksourceview-5/styles/` for Flatpak.

## Custom context-menu actions

Install `.desktop` files under `$XDG_DATA_HOME/lia-files/actions/` (normally
`~/.local/share/lia-files/actions/`) or the corresponding system data directories.
Files are read when the menu opens. User files override system files by filename;
`Hidden=true` suppresses a system action. An action must match **every** selected
file's MIME type. Local-file field codes are hidden for remote selections.

```ini
[Desktop Entry]
Type=Application
Name=Set as Wallpaper
Icon=preferences-desktop-wallpaper
MimeType=image/png;image/jpeg;image/webp;
Exec=penumbra-wallpaper %f
```

Supported Exec field codes are `%f`, `%F`, `%u`, `%U`, and `%%`. Singular codes
match a single selection; plural codes accept several files. GIO expands file
arguments without a shell, including spaces and shell metacharacters in filenames.
`TryExec`, `NoDisplay`, localized `Name`, and `Terminal` are supported. Install
only actions you trust: selecting one explicitly runs its command.

Flatpak actions live under `~/.var/app/io.github.lia.Files/data/lia-files/actions/`.
Their commands run on the host through `flatpak-spawn --host`, like Open in
Terminal, using the existing Flatpak permission.
