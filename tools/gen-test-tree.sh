#!/bin/sh
# Generates a large flat folder for stress-testing listing and scrolling.
# Usage: gen-test-tree.sh <dir> [count]
set -eu
dir="${1:?usage: $0 <dir> [count]}"
count="${2:-100000}"
mkdir -p "$dir"
cd "$dir"
seq 1 "$count" | awk '{
    ext = substr("txt png pdf md  rs  jpg", 1 + ($1 % 6) * 4, 3); gsub(/ /, "", ext)
    printf "file%d.%s\n", $1, ext
}' | xargs touch
mkdir -p folder-a folder-b folder-c
echo "Created $count files in $dir"
