#!/bin/sh
# Invoked by Meson: build with Cargo and copy the binary to Meson's output.
# Usage: cargo-build.sh <manifest> <target-dir> <profile> <output>
set -eu
manifest="$1"
target_dir="$2"
profile="$3"
output="$4"

if [ "$profile" = "release" ]; then
    cargo build --manifest-path "$manifest" --target-dir "$target_dir" --release
else
    cargo build --manifest-path "$manifest" --target-dir "$target_dir"
fi
cp "$target_dir/$profile/lia-files" "$output"
