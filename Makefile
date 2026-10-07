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
