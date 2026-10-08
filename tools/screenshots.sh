#!/bin/sh
# Regenerates the AppStream screenshots in data/screenshots/ from a demo
# home folder. Runs the app in a nested headless KWin with its own D-Bus
# session, English locale, Adwaita icons and in-memory settings, so the
# result doesn't depend on (or touch) the desktop it runs on.
#
# Usage: tools/screenshots.sh [path/to/lia-files]   (default: target/debug)
# Needs: kwin_wayland, ImageMagick.
set -eu

if [ -z "${LIA_SHOTS_WORK:-}" ]; then
    bin=$(realpath "${1:-target/debug/lia-files}")
    out=$(realpath "$(dirname "$0")/../data/screenshots")
    # The nested session's command line can't take paths with spaces.
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    ln -s "$bin" "$work/lia-files"
    cp "$0" "$work/screenshots.sh"

    home=$work/home
    mkdir -p "$home/.config/gtk-3.0" "$home/.config/gtk-4.0" "$home/Projects"
    for dir in Desktop Documents Downloads Music Pictures Videos Templates Public; do
        mkdir -p "$home/$dir"
        upper=$(echo "$dir" | tr '[:lower:]' '[:upper:]' | sed 's/DOWNLOADS/DOWNLOAD/; s/PUBLIC/PUBLICSHARE/')
        echo "XDG_${upper}_DIR=\"\$HOME/$dir\"" >> "$home/.config/user-dirs.dirs"
    done
    echo "file://$home/Projects" > "$home/.config/gtk-3.0/bookmarks"
    printf '[Settings]\ngtk-icon-theme-name=Adwaita\n' > "$home/.config/gtk-4.0/settings.ini"

    docs=$home/Documents
    mkdir -p "$docs/Invoices" "$docs/Recipes" "$docs/Travel 2026" "$docs/Work"
    for entry in "Budget 2026.ods:18400" "Cover letter.odt:24900" "Thesis draft.pdf:1284000" \
        "Reading list.txt:900" "Tax return.pdf:412000" "Floor plan.svg:56000" "Résumé.pdf:98000"; do
        head -c "${entry##*:}" /dev/zero > "$docs/${entry%:*}"
    done
    cat > "$docs/Meeting notes.md" << 'EOF'
# Weekly sync — 6 October

## Decisions
- Ship **Lia Files 1.0** on Friday
- Flatpak first, AUR package the same day

## Action items
1. Final screenshots for the store page
2. Check the French translation
3. Tag `v1.0.0`

> Remember: delete goes to the Trash, and everything can be undone.
EOF
    pics=$home/Pictures
    mkdir -p "$pics/Screenshots" "$pics/Holidays"
    magick -size 1600x1067 gradient:'#1c71d8-#99c1f1' -swirl 60 "$pics/Lake.jpg"
    magick -size 1600x1067 plasma:'#26a269-#f6d32d' -blur 0x6 "$pics/Meadow.jpg"
    magick -size 1600x1067 gradient:'#e66100-#613583' "$pics/Sunset.jpg"
    magick -size 1200x1600 plasma:'#c64600-#f9f06b' -blur 0x8 "$pics/Autumn.jpg"
    magick -size 1600x1067 radial-gradient:'#f6f5f4-#241f31' "$pics/Night sky.png"
    magick -size 1600x1067 plasma:'#3584e4-#ffffff' -blur 0x3 "$pics/Clouds.jpg"
    magick -size 1600x1067 gradient:'#2ec27e-#1a5fb4' -wave 40x400 "$pics/Coast.jpg"
    magick -size 1080x1080 plasma:'#e01b24-#ffbe6f' -blur 0x10 "$pics/Poppies.jpg"
    # Varied dates, the notes edited just now.
    i=0
    for f in "$docs"/* "$pics"/*; do
        i=$((i + 1))
        touch -d "$((i * 7 % 20 + 1)) days ago" "$f"
    done
    touch "$docs/Meeting notes.md"

    LIA_SHOTS_WORK=$work dbus-run-session -- kwin_wayland --virtual --no-lockscreen \
        --width 1300 --height 820 --socket lia-screenshots \
        --exit-with-session "$work/screenshots.sh" \
        > "$work/session.log" 2>&1 < /dev/null || true
    for shot in main grid quicklook; do
        magick "$work/$shot.png" -strip -define png:compression-level=9 "$out/$shot.png"
        echo "$out/$shot.png"
    done
    exit
fi

# ── Inside the nested session ───────────────────────────────────────────────
work=$LIA_SHOTS_WORK
home=$work/home

# fm <method> <uris>: an org.freedesktop.FileManager1 call
fm() {
    gdbus call --session --dest org.freedesktop.FileManager1 \
        --object-path /org/freedesktop/FileManager1 \
        --method "org.freedesktop.FileManager1.$1" "$2" "" > /dev/null
}
# win <action> <parameter>
win() {
    gdbus call --session --dest io.github.lia.Files --object-path /io/github/lia/Files/window/1 \
        --method org.gtk.Actions.Activate "$1" "$2" "{}" > /dev/null
}
# start <screenshot name> <prefer-light|prefer-dark>
start() {
    rm -rf "${work:?}/data" "${work:?}/state"
    env HOME="$home" XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$work/data" \
        XDG_STATE_HOME="$work/state" XDG_CACHE_HOME="$work/cache" \
        GDK_DEBUG=no-portals LANG=en_US.UTF-8 LC_ALL=en_US.UTF-8 GSETTINGS_BACKEND=memory \
        ADW_DEBUG_COLOR_SCHEME="$2" LIA_SCREENSHOT="$work/$1.png" LIA_SCREENSHOT_DELAY=6000 \
        "$work/lia-files" --gapplication-service > "$work/$1.log" 2>&1 &
    sleep 2
}

start main prefer-light
fm ShowItems "['file://$home/Documents/Meeting%20notes.md']"
sleep 1.5
win toggle-details-pane "[]"
sleep 7

start grid prefer-dark
fm ShowFolders "['file://$home/Pictures', 'file://$home/Documents', 'file://$home/Downloads']"
sleep 1
win view-mode "[<'grid'>]"
sleep 7

start quicklook prefer-light
fm ShowItems "['file://$home/Pictures/Sunset.jpg']"
sleep 1
win view-mode "[<'grid'>]"
sleep 1.5
win quick-look "[]"
sleep 7
