#!/bin/bash
# nethos-view's own stack on a disk: Python, PyGObject, GTK4, WebKitGTK 6.0.
#
# nethos-view is a Python script -- `import gi`, GTK4, WebKit 6.0, and
# gtk4-layer-shell when a compositor offers it. So the question after "does
# WebKit load" is "does the binding layer load", which is a different stack:
# gobject-introspection reads typelibs at runtime and dlopens the libraries
# they name, so it exercises paths a C probe never touches.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/kernel/ldk/build"
OUT="$BUILD/view.img"
SIZE="${SIZE:-2G}"
mkdir -p "$BUILD"; rm -f "$OUT"

docker run --rm --platform linux/arm64 \
    -v "$ROOT/payload:/payload:ro" -v "$BUILD:/out" nethos-ldk sh -ec '
    apt-get -qq update >/dev/null 2>&1
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        python3 python3-gi gir1.2-gtk-4.0 gir1.2-webkit-6.0 \
        libwebkitgtk-6.0-4 gir1.2-gtk4layershell-1.0 e2fsprogs >/dev/null 2>&1 \
    || DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        python3 python3-gi gir1.2-gtk-4.0 gir1.2-webkit-6.0 \
        libwebkitgtk-6.0-4 e2fsprogs >/dev/null 2>&1

    R=/tmp/view
    mkdir -p $R/lib $R/bin $R/share $R/nethos
    V=$(python3 -c "import sys; print(f\"{sys.version_info[0]}.{sys.version_info[1]}\")")
    cp -L /usr/bin/python$V $R/bin/python3
    cp -a /usr/lib/python$V $R/lib/python$V
    find $R/lib/python$V -name __pycache__ -type d -exec rm -rf {} + 2>/dev/null || true
    # PyGObject lives in dist-packages, outside the stdlib tree.
    cp -a /usr/lib/python3/dist-packages $R/lib/dist-packages

    D=/usr/lib/aarch64-linux-gnu
    # Typelibs are read at runtime and name the shared libraries to dlopen,
    # so they must travel with the closure or `gi.require_version` fails with
    # a namespace error that looks nothing like a missing library.
    mkdir -p $R/share/girepository-1.0
    cp -L $D/girepository-1.0/*.typelib $R/share/girepository-1.0/ 2>/dev/null || true

    # Everything the interpreter, the bindings and WebKit link against, plus
    # the extension modules python dlopens.
    for f in /usr/bin/python$V $R/lib/python$V/lib-dynload/*.so \
             $R/lib/dist-packages/gi/*.so $D/libwebkitgtk-6.0.so.4* \
             $D/libgtk-4.so.1* $D/libgirepository-1.0.so.1*; do
        [ -e "$f" ] || continue
        ldd "$f" 2>/dev/null | sed -n "s/.*=> \(\/[^ ]*\).*/\1/p"
    done | sort -u | while read -r l; do cp -Ln "$l" $R/lib/ 2>/dev/null || true; done
    cp -L $D/libwebkitgtk-6.0.so.4 $R/lib/ 2>/dev/null || true
    cp -L /lib/ld-linux-aarch64.so.1 $R/lib/
    # WebKit needs its own helper binaries and resources at runtime.
    cp -a $D/webkitgtk-6.0 $R/lib/ 2>/dev/null || true
    cp -a /usr/share/glib-2.0 $R/share/ 2>/dev/null || true

    cp /payload/bin/nethos-view $R/nethos/nethos-view
    echo "  payload: $(du -sh $R | cut -f1), $(ls $R/lib | wc -l) libraries, $(ls $R/share/girepository-1.0 | wc -l) typelibs"
    mke2fs -q -t ext4 -d $R -F /out/view.img '"$SIZE"'
'
echo "Built $OUT ($(du -h "$OUT" | cut -f1))"
