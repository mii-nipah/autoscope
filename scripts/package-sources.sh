#!/bin/sh
# Optional companion sources for redistribution; recipients do not need these to run Autoscope.
set -eu
repo=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
output="$repo/target/bundle"
[ -f "$output/system-sources.txt" ] || { echo 'Run scripts/build-bundle.sh first.' >&2; exit 1; }
mkdir -p "$output/sources"
podman run --rm --security-opt label=disable \
    -v "$repo:/source:ro" -v "$output:/build:rw" \
    -v autoscope-cargo-cache:/usr/local/cargo/registry \
    -v autoscope-git-cache:/usr/local/cargo/git \
    localhost/autoscope-package:22.04 sh -ec '
        mkdir -p /build/sources/autoscope /build/sources/system /build/sources/autoscope/.cargo
        cp -R Cargo.toml Cargo.lock LICENSE README.md USAGE.md src scripts plugin tests /build/sources/autoscope/
        cargo vendor --locked --versioned-dirs /build/sources/autoscope/vendor > /build/sources/autoscope/.cargo/config.toml
        sed -i "s|/build/sources/autoscope/vendor|vendor|g" /build/sources/autoscope/.cargo/config.toml
        cp /build/Autoscope.AppDir/build-info.json /build/sources/
        sed -n "s/^deb /deb-src /p" /etc/apt/sources.list > /etc/apt/sources.list.d/autoscope-sources.list
        apt-get update
        python3 scripts/packaging/sources.py /build/system-sources.txt /build/sources/system
    '
mkdir -p "$repo/target/plugin"
archive="$repo/target/plugin/autoscope-sources.tar.gz"
tar --exclude=__pycache__ -czf "$archive" -C "$output" sources
(cd "$(dirname "$archive")" && sha256sum "$(basename "$archive")") > "$archive.sha256"
printf 'Built %s\n' "$archive"
