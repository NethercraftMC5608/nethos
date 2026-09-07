#!/bin/bash
# WebKit on an ext4 disk. The closure is ~100MB installed; nothing that big
# goes in an initrd that lives in Linux's memory pool.
#
# Staged like Mesa was: this builds the probe that answers "does the closure
# load", not a browser. Rendering is a later disk.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/kernel/ldk/build"
OUT="$BUILD/webkit.img"
SIZE="${SIZE:-1G}"
mkdir -p "$BUILD"; rm -f "$OUT"

docker run --rm --platform linux/arm64 \
    -v "$ROOT/kernel/init:/src:ro" -v "$BUILD:/out" nethos-ldk sh -ec '
    apt-get -qq update >/dev/null 2>&1
    # libwebkitgtk-6.0-4 is the GTK4 build on trixie; fall back to 4.1 if the
    # archive disagrees. --no-install-recommends or this pulls a desktop.
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        libwebkitgtk-6.0-4 e2fsprogs >/dev/null 2>&1 \
    || DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-0 e2fsprogs >/dev/null 2>&1

    R=/tmp/wk
    mkdir -p $R/lib $R/bin
    gcc -fPIE -pie -O2 /src/wkprobe.c -o $R/bin/wkprobe -ldl

    D=/usr/lib/aarch64-linux-gnu
    # The closure by ldd from the WebKit library itself, plus the loader.
    # WebKit dlopens more at runtime (gstreamer, gio modules); this is the
    # link-time closure, which is what the probe needs.
    WK=$(ls $D/libwebkitgtk-6.0.so.4* $D/libwebkit2gtk-4.1.so.0* 2>/dev/null | head -1)
    [ -n "$WK" ] || { echo "no webkit library found" >&2; exit 1; }
    echo "  webkit: $(basename $WK)"
    for f in $WK $R/bin/wkprobe; do
        ldd $f 2>/dev/null | sed -n "s/.*=> \(\/[^ ]*\).*/\1/p"
    done | sort -u | while read -r l; do cp -Ln "$l" $R/lib/ 2>/dev/null || true; done
    cp -L $WK $R/lib/
    cp -L /lib/ld-linux-aarch64.so.1 $R/lib/

    echo "  payload: $(du -sh $R | cut -f1), $(ls $R/lib | wc -l) libraries"
    mke2fs -q -t ext4 -d $R -F /out/webkit.img '"$SIZE"'
'
echo "Built $OUT ($(du -h "$OUT" | cut -f1))"
