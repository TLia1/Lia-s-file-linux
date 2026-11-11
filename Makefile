# Developer shortcuts. `cargo run` works on its own; Meson is for installing.

.PHONY: run release build install test lint fmt clean test-tree

run:
	cargo run

release:
	cargo run --release

build:
	meson setup build --reconfigure >/dev/null 2>&1 || meson setup build
	ninja -C build

install: build
	ninja -C build install
	python3 tools/install-user-portal.py --uninstall
	-gapplication action io.github.lia.Files.Portal quit >/dev/null 2>&1
	-gdbus call --session --dest org.freedesktop.DBus --object-path /org/freedesktop/DBus --method org.freedesktop.DBus.ReloadConfig >/dev/null 2>&1
	-systemctl --user try-restart xdg-desktop-portal.service >/dev/null 2>&1

test:
	cargo test

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt

clean:
	cargo clean
	rm -rf build

# Creates a 100 000-file folder for performance testing.
test-tree:
	tools/gen-test-tree.sh /tmp/lia-files-100k 100000
