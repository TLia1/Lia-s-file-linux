#!/usr/bin/env python3
"""Writes the Flatpak sources that vendor every crate in Cargo.lock, so that
flatpak-builder can build without network access.

Usage: build-aux/flatpak-cargo-sources.py [Cargo.lock] [output.json]

A dependency-free equivalent of flatpak-builder-tools'
flatpak-cargo-generator.py for crates.io-only lock files. Rerun it whenever
Cargo.lock changes (CI checks that the output is up to date).
"""

import json
import sys
import tomllib

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
VENDOR_DIR = "cargo/vendor"
CARGO_CONFIG = f"""[source.vendored-sources]
directory = "{VENDOR_DIR}"

[source.crates-io]
replace-with = "vendored-sources"
"""


def sources(lock):
    out = []
    for package in lock["package"]:
        source = package.get("source")
        if source is None:
            continue  # this project
        if source != CRATES_IO:
            sys.exit(f"unsupported source for {package['name']}: {source}")
        name, version, checksum = package["name"], package["version"], package["checksum"]
        dest = f"{VENDOR_DIR}/{name}-{version}"
        out.append(
            {
                "type": "archive",
                "archive-type": "tar-gzip",
                "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                "sha256": checksum,
                "dest": dest,
            }
        )
        out.append(
            {
                "type": "inline",
                "contents": json.dumps({"package": checksum, "files": {}}),
                "dest": dest,
                "dest-filename": ".cargo-checksum.json",
            }
        )
    out.append(
        {
            "type": "inline",
            "contents": CARGO_CONFIG,
            "dest": "cargo",
            "dest-filename": "config.toml",
        }
    )
    return out


def main():
    lock_path = sys.argv[1] if len(sys.argv) > 1 else "Cargo.lock"
    out_path = sys.argv[2] if len(sys.argv) > 2 else "build-aux/cargo-sources.json"
    with open(lock_path, "rb") as f:
        lock = tomllib.load(f)
    with open(out_path, "w") as f:
        json.dump(sources(lock), f, indent=4)
        f.write("\n")


if __name__ == "__main__":
    main()
