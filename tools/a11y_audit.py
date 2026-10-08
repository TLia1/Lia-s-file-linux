"""Lists the visible interactive widgets of the running app that have no
accessible name (what a screen reader would announce as just "button").

Used by a11y-audit.sh, which runs the app in a nested session with its own
accessibility bus. Prints one line per unnamed widget, then a summary.
"""

import sys
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi  # noqa: E402

INTERACTIVE = {
    Atspi.Role.PUSH_BUTTON,
    Atspi.Role.TOGGLE_BUTTON,
    Atspi.Role.CHECK_BOX,
    Atspi.Role.RADIO_BUTTON,
    Atspi.Role.ENTRY,
    Atspi.Role.TEXT,
    Atspi.Role.SPIN_BUTTON,
    Atspi.Role.COMBO_BOX,
    Atspi.Role.SLIDER,
    Atspi.Role.PAGE_TAB,
    Atspi.Role.LINK,
    Atspi.Role.MENU_ITEM,
    Atspi.Role.CHECK_MENU_ITEM,
    Atspi.Role.RADIO_MENU_ITEM,
    Atspi.Role.SWITCH,
    Atspi.Role.LIST_BOX,
    Atspi.Role.TABLE,
    Atspi.Role.TREE_TABLE,
    Atspi.Role.PROGRESS_BAR,
    Atspi.Role.LEVEL_BAR,
}


def find_app(timeout=30):
    deadline = time.time() + timeout
    while time.time() < deadline:
        desktop = Atspi.get_desktop(0)
        for i in range(desktop.get_child_count()):
            app = desktop.get_child_at_index(i)
            if app is not None and app.get_name() in ("lia-files", "Lia Files"):
                return app
        time.sleep(0.5)
    sys.exit("the app isn't on the accessibility bus")


def walk(node, path, found, depth=0):
    try:
        role = node.get_role()
        name = node.get_name() or ""
        showing = node.get_state_set().contains(Atspi.StateType.SHOWING)
    except gi.repository.GLib.Error:
        return
    if not showing and depth > 2:
        return
    path = f"{path}/{node.get_role_name()}" + (f"[{name[:30]}]" if name else "")
    if role in INTERACTIVE and showing:
        found.append((bool(name), path))
    for i in range(node.get_child_count()):
        child = node.get_child_at_index(i)
        if child is not None:
            walk(child, path, found, depth + 1)


def main():
    found = []
    walk(find_app(), "", found)
    unnamed = [path for named, path in found if not named]
    for path in unnamed:
        print("UNNAMED", "/".join(path.split("/")[-6:]))
    print(f"{len(found)} interactive widgets, {len(unnamed)} without a name")


main()
