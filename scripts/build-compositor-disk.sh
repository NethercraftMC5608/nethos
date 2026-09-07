#!/bin/bash
# A real Wayland compositor on a disk, next to nethos-view's own stack.
#
# The binding stack is proven; the only thing left before nethos-view runs
# is something for GTK4 to connect to. weston is the choice for the first
# attempt: it is small, it has a headless backend that needs no input devices
# and no KMS, and it speaks xdg-shell, which is all GTK4 asks for. It does
# not speak wlr-layer-shell -- nethos-view degrades without it, and a
# wlroots compositor is the follow-up if the panel needs to be a real layer.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/kernel/ldk/build"
OUT="$BUILD/comp.img"
SIZE="${SIZE:-3G}"
mkdir -p "$BUILD"; rm -f "$OUT"

docker run --rm --platform linux/arm64 \
    -v "$ROOT/payload:/payload:ro" -v "$BUILD:/out" nethos-ldk sh -ec '
    apt-get -qq update >/dev/null 2>&1
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        weston python3 python3-gi gir1.2-gtk-4.0 gir1.2-webkit-6.0 \
        libwebkitgtk-6.0-4 gir1.2-gtk4layershell-1.0 \
        libgl1-mesa-dri libegl1 libegl-mesa0 libgles2 libgbm1 libdrm2 \
        xkb-data e2fsprogs >/dev/null 2>&1

    R=/tmp/comp
    mkdir -p $R/lib $R/bin $R/share $R/nethos $R/dri $R/glvnd
    V=$(python3 -c "import sys; print(f\"{sys.version_info[0]}.{sys.version_info[1]}\")")
    cp -L /usr/bin/python$V $R/bin/python3
    cp -a /usr/lib/python$V $R/lib/python$V
    find $R/lib/python$V -name __pycache__ -type d -exec rm -rf {} + 2>/dev/null || true
    cp -a /usr/lib/python3/dist-packages $R/lib/dist-packages
    cp -L /usr/bin/weston $R/bin/weston

    D=/usr/lib/aarch64-linux-gnu
    mkdir -p $R/share/girepository-1.0
    cp -L $D/girepository-1.0/*.typelib $R/share/girepository-1.0/ 2>/dev/null || true
    cp -L $D/dri/swrast_dri.so $R/dri/ 2>/dev/null || true
    cp -L /usr/share/glvnd/egl_vendor.d/*.json $R/glvnd/ 2>/dev/null || true

    # weston loads its backends and renderers as modules at runtime; they are
    # in nobody DT_NEEDED and must be carried explicitly or it exits saying
    # only that it could not load a backend.
    cp -a $D/weston $R/lib/ 2>/dev/null || true
    cp -a $D/libweston-* $R/lib/ 2>/dev/null || true

    for f in /usr/bin/weston /usr/bin/python$V $R/lib/python$V/lib-dynload/*.so \
             $R/lib/dist-packages/gi/*.so $D/libwebkitgtk-6.0.so.4* \
             $D/libgtk-4.so.1* $D/libgirepository-1.0.so.1* \
             $D/weston/*.so $D/libweston-*/*.so $D/libgallium*.so $D/dri/swrast_dri.so; do
        [ -e "$f" ] || continue
        ldd "$f" 2>/dev/null | sed -n "s/.*=> \(\/[^ ]*\).*/\1/p"
    done | sort -u | while read -r l; do cp -Ln "$l" $R/lib/ 2>/dev/null || true; done
    cp -L $D/libwebkitgtk-6.0.so.4 $R/lib/ 2>/dev/null || true
    cp -L /lib/ld-linux-aarch64.so.1 $R/lib/
    cp -a $D/webkitgtk-6.0 $R/lib/ 2>/dev/null || true
    cp -a /usr/share/glib-2.0 $R/share/ 2>/dev/null || true
    cp -a /usr/share/weston $R/share/ 2>/dev/null || true
    # libxkbcommon reads the keymap data at runtime. Without it weston stops
    # at "failed to create XKB context" -- a compositor with no input devices
    # still builds a keymap, so this is not optional even headless.
    mkdir -p $R/share/X11
    cp -a /usr/share/X11/xkb $R/share/X11/ 2>/dev/null || true
    cp /payload/bin/nethos-view $R/nethos/nethos-view

    echo "  payload: $(du -sh $R | cut -f1), $(ls $R/lib | wc -l) libraries"
    mke2fs -q -t ext4 -d $R -F /out/comp.img '"$SIZE"'
'
echo "Built $OUT ($(du -h "$OUT" | cut -f1))"
