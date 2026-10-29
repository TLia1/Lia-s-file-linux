#!/usr/bin/env python3
"""Install the native picker for the current user on recent xdg-desktop-portal.

Build first with cargo build --release. This does not replace the normal file
manager launcher. --enable changes only the FileChooser portal preference.
"""
import argparse
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent


def atomic_copy(source, target, mode=0o644):
    target.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.NamedTemporaryFile(dir=target.parent, delete=False) as output:
        temporary = Path(output.name)
        with open(source, "rb") as stream:
            shutil.copyfileobj(stream, output)
    temporary.chmod(mode)
    temporary.replace(target)


def enable(config_dir):
    directory = config_dir / "xdg-desktop-portal"
    candidates = [directory / f"{name.lower()}-portals.conf"
                  for name in os.environ.get("XDG_CURRENT_DESKTOP", "").split(":") if name]
    candidates.append(directory / "portals.conf")
    config = next((path for path in candidates if path.exists()), candidates[-1])
    original = config.read_text() if config.exists() else ""
    lines = original.splitlines(keepends=True)
    start = next((i for i, line in enumerate(lines) if line.strip() == "[preferred]"), None)
    setting = "org.freedesktop.impl.portal.FileChooser=lia;gtk\n"
    if start is None:
        lines.extend(["\n[preferred]\n", setting])
    else:
        end = next((i for i in range(start + 1, len(lines)) if lines[i].lstrip().startswith("[")), len(lines))
        matches = [i for i in range(start + 1, end)
                   if re.match(r"\s*org\.freedesktop\.impl\.portal\.FileChooser\s*=", lines[i])]
        if matches:
            lines[matches[0]] = setting
            for index in reversed(matches[1:]):
                del lines[index]
        else:
            if end and not lines[end - 1].endswith("\n"):
                lines[end - 1] += "\n"
            lines.insert(end, setting)
    updated = "".join(lines)
    directory.mkdir(parents=True, exist_ok=True)
    if original and updated != original:
        with tempfile.NamedTemporaryFile(prefix=config.name + ".before-lia-", dir=directory, delete=False) as backup:
            backup.write(original.encode())
            print(f"Previous settings: {backup.name}")
    with tempfile.NamedTemporaryFile(mode="w", dir=directory, delete=False) as output:
        output.write(updated)
        temporary = Path(output.name)
    temporary.replace(config)
    print(f"File chooser preference: {config}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=ROOT / "target/release/lia-files")
    parser.add_argument("--enable", action="store_true")
    args = parser.parse_args()
    if not args.binary.is_file() or not os.access(args.binary, os.X_OK):
        parser.error("Build an executable first with cargo build --release")
    data = Path(os.environ.get("XDG_DATA_HOME", Path.home() / ".local/share"))
    config = Path(os.environ.get("XDG_CONFIG_HOME", Path.home() / ".config"))
    binary = data / "lia-files/libexec/lia-files-portal"
    atomic_copy(args.binary, binary, 0o755)
    atomic_copy(ROOT / "data/lia.portal", data / "xdg-desktop-portal/portals/lia.portal")
    schema_dir = data / "glib-2.0/schemas"
    atomic_copy(ROOT / "data/io.github.lia.Files.gschema.xml", schema_dir / "io.github.lia.Files.gschema.xml")
    subprocess.run(["glib-compile-schemas", str(schema_dir)], check=True)
    services = data / "dbus-1/services"
    services.mkdir(parents=True, exist_ok=True)
    service = services / "org.freedesktop.impl.portal.desktop.lia.service"
    service.write_text("[D-BUS Service]\nName=org.freedesktop.impl.portal.desktop.lia\n"
                       f"Exec={shlex.quote(str(binary))} --file-chooser-portal\n")
    if args.enable:
        enable(config)
    print(f"Installed picker: {binary}")
    print("Reload D-Bus service discovery before restarting the portal:")
    print("gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.ReloadConfig")
    print("Then restart xdg-desktop-portal.service and affected apps to apply.")


if __name__ == "__main__":
    main()
