#!/bin/sh
# Build on a fixed older Linux base; only generated files under target/ are writable.
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
output="$repo/target/bundle"
mkdir -p "$output"
podman build --pull=never -t localhost/autoscope-package:22.04 \
    -f "$repo/scripts/packaging/Containerfile" "$repo/scripts/packaging"
podman run --rm --security-opt label=disable \
    -v "$repo:/source:ro" -v "$output:/build:rw" \
    -v autoscope-cargo-cache:/usr/local/cargo/registry \
    -v autoscope-git-cache:/usr/local/cargo/git \
    localhost/autoscope-package:22.04 sh -ec '
        cargo build --release --locked --target-dir /build/cargo
        python3 scripts/packaging/bundle.py /source /build
    '
printf 'Built %s\n' "$output/Autoscope.AppDir"
