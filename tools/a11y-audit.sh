#!/bin/sh
# Checks that the main window and the dialogs have no unnamed controls, as
# a screen reader sees them (AT-SPI). Runs the app in a nested headless
# KWin with its own D-Bus session, so it doesn't touch the desktop or the
# user's settings, and opens each dialog through the app's D-Bus actions.
#
# Usage: tools/a11y-audit.sh [path/to/lia-files]   (default: target/debug)
# Needs: kwin_wayland, at-spi2-core, python-gobject. Exits 1 if a control
# has no accessible name.
set -eu

if [ -z "${LIA_AUDIT_WORK:-}" ]; then
    bin=$(realpath "${1:-target/debug/lia-files}")
    tools=$(realpath "$(dirname "$0")")
    # The nested session's command line can't take paths with spaces.
    work=$(mktemp -d)
    trap 'rm -rf "$work"' EXIT
    ln -s "$bin" "$work/lia-files"
    cp "$tools/a11y-audit.sh" "$tools/a11y_audit.py" "$work/"
    mkdir -p "$work/folder/sub"
    for f in notes.txt photo.png report.pdf; do echo x > "$work/folder/$f"; done
    LIA_AUDIT_WORK=$work GSETTINGS_BACKEND=memory \
        XDG_DATA_HOME="$work/data" XDG_STATE_HOME="$work/state" \
        dbus-run-session -- kwin_wayland --virtual --no-lockscreen \
        --width 1300 --height 820 --socket lia-a11y-audit \
        --exit-with-session "$work/a11y-audit.sh" \
        > "$work/session.log" 2>&1 < /dev/null || true
    cat "$work/report.txt"
    ! grep -q '^UNNAMED' "$work/report.txt"
    exit
fi

# ── Inside the nested session ───────────────────────────────────────────────
work=$LIA_AUDIT_WORK
# D-Bus activation of the registry needs systemd, absent here.
/usr/lib/at-spi-bus-launcher --launch-immediately &
sleep 1
/usr/lib/at-spi2-registryd &
sleep 1

# activate <app|win> <action>[=<string parameter>]
activate() {
    object=/io/github/lia/Files
    [ "$1" = win ] && object=$object/window/1
    parameter="[]"
    case $2 in *=*) parameter="[<'${2#*=}'>]" ;; esac
    gdbus call --session --dest io.github.lia.Files --object-path "$object" \
        --method org.gtk.Actions.Activate "${2%%=*}" "$parameter" "{}" > /dev/null
}

: > "$work/report.txt"
while read -r scenario steps; do
    "$work/lia-files" "$work/folder" &
    app=$!
    sleep 4
    for step in $steps; do
        activate "${step%%.*}" "${step#*.}"
        sleep 1
    done
    echo "== $scenario" >> "$work/report.txt"
    python3 -I "$work/a11y_audit.py" >> "$work/report.txt" 2>&1
    kill $app
    wait $app 2> /dev/null || true
done << SCENARIOS
main-window
grid-view win.view-mode=grid
preferences app.preferences
keyboard-shortcuts app.shortcuts
about app.about
connect-to-server win.connect-to-server
properties win.select-all win.properties
folder-properties win.location-properties=
batch-rename win.select-all win.rename
compress win.select-all win.compress
quick-look win.select-all win.quick-look
open-with win.select-all win.open-with
SCENARIOS
