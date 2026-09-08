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
# The initrd half: busybox, nethos-view's probe, and an init that starts
# weston and connects the stack to it.
BB="$BUILD/busybox"
[ -f "$BB" ] || { echo "no busybox at $BB" >&2; exit 1; }
R="$BUILD/comp-root"
rm -rf "$R"
mkdir -p "$R/bin" "$R/dev" "$R/mnt" "$R/proc" "$R/sys" "$R/tmp" \
         "$R/run/user/0" "$R/root/.config" "$R/usr/lib"
chmod 0700 "$R/run/user/0"
cp "$BB" "$R/bin/busybox"
cp "$ROOT/kernel/init/viewprobe.py" "$R/viewprobe.py"
cat > "$R/nk-init" <<'INIT'
#!/bin/busybox sh
busybox mount -t devtmpfs devtmpfs /dev 2>/dev/null
busybox mount -t proc proc /proc 2>/dev/null
busybox mount -t sysfs sysfs /sys 2>/dev/null
busybox mount -t ext4 -o ro /dev/vda /mnt || { echo "comp: mount failed"; exit 1; }
busybox ln -s /mnt/lib /lib
# Everything on this disk has /usr paths compiled in -- weston's backend
# modules, libxkbcommon's keymaps, glib's schemas -- and Debian puts
# libraries under a multiarch triplet the disk flattens away. Rebuild that
# shape with symlinks rather than an environment variable per library.
busybox mkdir -p /usr/lib
busybox ln -s /mnt/lib /usr/lib/aarch64-linux-gnu
busybox ln -s /mnt/share /usr/share
busybox ln -s /mnt/bin /usr/bin
echo "comp: disk mounted"

export LD_LIBRARY_PATH=/mnt/lib
# weston refuses a runtime dir that is not mode 0700 and owned by us, and
# says so on stderr rather than in --log.
busybox mkdir -p /run/user/0
busybox chmod 0700 /run/user/0
export XDG_RUNTIME_DIR=/run/user/0
# weston looks for weston.ini through the XDG config search path, and with
# HOME unset the search itself is what fails rather than the lookup.
busybox mkdir -p /root/.config
export HOME=/root
export XDG_CONFIG_HOME=/root/.config
# libxkbcommon has /usr/share/X11/xkb compiled in and there is no /usr here.
export XKB_CONFIG_ROOT=/mnt/share/X11/xkb
export LIBGL_DRIVERS_PATH=/mnt/dri
export __EGL_VENDOR_LIBRARY_DIRS=/mnt/glvnd
export GALLIUM_DRIVER=llvmpipe
export LIBGL_ALWAYS_SOFTWARE=1
export WESTON_MODULE_MAP=
# Headless: no input devices, no KMS. weston's pixman renderer needs neither,
# and GTK4 only needs xdg-shell on the other end of the socket.
echo "comp: uname:"; busybox uname -a || echo "  uname FAILED"
echo "comp: runtime dir:"; busybox ls -ld $XDG_RUNTIME_DIR
echo "comp: starting weston headless"
# kiosk-shell: xdg-shell without weston's own desktop UI. desktop-shell
# launches helper clients from /usr/libexec that are not on this disk, and
# their absence is fatal to it; a client of our own is the point here.
/mnt/bin/weston --backend=headless --renderer=pixman --width=1024 --height=768 \
    --shell=kiosk-shell.so --no-config --socket=wayland-0 --log=/tmp/w.log &
busybox sleep 6
echo "comp: socket:"
busybox ls -l $XDG_RUNTIME_DIR/ | busybox grep wayland || echo "  (no socket)"

echo "comp: connecting nethos-view's stack to it"
V=$(busybox ls /mnt/lib | busybox grep "^python3\." | busybox head -1)
export PYTHONHOME=/mnt
export PYTHONPATH=/mnt/lib/$V:/mnt/lib/$V/lib-dynload:/mnt/lib/dist-packages
export PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
export GI_TYPELIB_PATH=/mnt/share/girepository-1.0
export GDK_BACKEND=wayland
export WAYLAND_DISPLAY=wayland-0
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
/mnt/bin/python3 /viewprobe.py
echo "comp: view exit $?"
echo "comp: weston log tail:"
busybox tail -6 /tmp/w.log
echo "comp: done"
busybox umount /mnt 2>/dev/null
INIT
chmod +x "$R/nk-init"
( cd "$R" && find . -print | LC_ALL=C sort | cpio -o -H newc 2>/dev/null ) \
    > "$BUILD/comp.cpio"

echo "Built $OUT ($(du -h "$OUT" | cut -f1))"
echo "Built $BUILD/comp.cpio"
