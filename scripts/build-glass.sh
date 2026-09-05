#!/bin/sh
# Run inside Debian trixie on the target CPU (amd64 or arm64).
# Build dependencies: build-essential meson ninja-build pkg-config wayfire-dev
# libwf-config-dev libwlroots-0.18-dev libglm-dev libgles2-mesa-dev.
set -eu
ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
ARCH=$(uname -m)
BUILD="$ROOT/build/glass-$ARCH"
OUT="$ROOT/payload/wayfire/built/$ARCH"
meson setup "$BUILD" "$ROOT/payload/wayfire/glass" --reconfigure
meson compile -C "$BUILD"
mkdir -p "$OUT"
install -m 0755 "$BUILD/libnethos-glass.so" "$OUT/libnethos-glass.so"
printf 'Wayfire 0.9; %s\n' "$ARCH" > "$OUT/abi.txt"
printf 'Built %s; install with payload/install-nethos.sh --files-only\n' "$OUT"
