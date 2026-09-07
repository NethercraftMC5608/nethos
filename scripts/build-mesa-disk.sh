#!/bin/bash
# Mesa on an ext4 disk, because it does not fit anywhere else.
#
# nk's rootfs is an initrd unpacked into Linux's memory pool, which is 64MB.
# libLLVM alone is 118MB and libgallium has it in DT_NEEDED, so llvmpipe
# cannot live there. It can live on the virtio-blk disk that already works:
# nk's execve and dynamic loader both go through Linux's VFS, so a binary and
# its libraries on a mounted ext4 need nothing switch_root would have given.
#
# mke2fs -d populates the filesystem from a directory, so none of this needs
# root or a loopback mount -- neither of which macOS has for ext4 anyway.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/kernel/ldk/build/mesa.img"
SIZE="${SIZE:-512M}"
mkdir -p "$(dirname "$OUT")"
rm -f "$OUT"

docker run --rm --platform linux/arm64 \
    -v "$ROOT/kernel/init:/src:ro" -v "$ROOT/kernel/ldk/build:/out" nethos-ldk sh -ec '
    apt-get -qq update >/dev/null 2>&1
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        libgl1-mesa-dri libegl1 libegl-mesa0 libgles2 libgbm1 libdrm2 \
        libegl-dev libgles-dev e2fsprogs >/dev/null 2>&1

    R=/tmp/mesa
    mkdir -p $R/lib $R/dri
    gcc -fPIE -pie -O2 /src/eglprobe.c -o $R/eglprobe -lEGL -lGLESv2 -ldl

    # The closure by ldd, plus the pieces that are only ever dlopened and so
    # appear in nobody DT_NEEDED: the DRI driver and the gallium library
    # behind it. Missing one of those is a runtime failure with a message
    # about a driver rather than about a file.
    D=/usr/lib/aarch64-linux-gnu
    cp -L $D/dri/swrast_dri.so $R/dri/
    cp -L $D/dri/kms_swrast_dri.so $R/dri/ 2>/dev/null || true
    EXTRA="$D/dri/swrast_dri.so $D/libgallium"*.so" $D/libEGL_mesa.so.0"
    for f in $R/eglprobe $EXTRA; do
        cp -L $f $R/lib/ 2>/dev/null || true
        ldd $f 2>/dev/null | sed -n "s/.*=> \(\/[^ ]*\).*/\1/p"
    done | sort -u | while read -r l; do cp -Ln "$l" $R/lib/ 2>/dev/null || true; done
    cp -L /lib/ld-linux-aarch64.so.1 $R/lib/
    rm -f $R/lib/eglprobe

    # libEGL.so.1 is glvnd, not Mesa: it is a dispatch layer that finds its
    # vendor by reading JSON out of a directory. Without this file
    # eglGetDisplay fails before Mesa is ever loaded, which reads like an EGL
    # bug and is a missing 200-byte config.
    mkdir -p $R/glvnd
    cp -L /usr/share/glvnd/egl_vendor.d/*.json $R/glvnd/

    echo "  payload: $(du -sh $R | cut -f1)"
    mke2fs -q -t ext4 -d $R -F /out/mesa.img '"$SIZE"'
'
# The initrd is the small half: busybox, and an init that mounts the disk and
# runs the probe off it. nk's execve and dynamic loader both go through
# Linux's VFS, so a binary on a mounted filesystem needs nothing more than
# this -- which is why Mesa did not have to wait for switch_root.
BB="$ROOT/kernel/ldk/build/busybox"
if [ ! -f "$BB" ]; then
    echo "no busybox at $BB -- run the Busybox test class first" >&2
    exit 1
fi
R="$ROOT/kernel/ldk/build/mesa-root"
rm -rf "$R"
mkdir -p "$R/bin" "$R/dev" "$R/mnt" "$R/proc" "$R/sys"
cp "$BB" "$R/bin/busybox"
cat > "$R/nk-init" <<'INIT'
#!/bin/busybox sh
busybox mount -t devtmpfs devtmpfs /dev 2>/dev/null
busybox mount -t proc proc /proc 2>/dev/null
busybox mount -t sysfs sysfs /sys 2>/dev/null
busybox mount -t ext4 /dev/vda /mnt || { echo "mesa: mount failed"; exit 1; }
echo "mesa: disk mounted"
# /proc is not decoration: LLVM reads /proc/cpuinfo to pick its code path and
# says so loudly when it cannot.
export LD_LIBRARY_PATH=/mnt/lib
export LIBGL_DRIVERS_PATH=/mnt/dri
# libEGL.so.1 is glvnd, a dispatch layer that finds Mesa by reading JSON out
# of a directory. Without this it returns EGL_NO_DISPLAY and says nothing.
export __EGL_VENDOR_LIBRARY_DIRS=/mnt/glvnd
# Surfaceless: no window system to stand up, and llvmpipe rather than the GPU
# -- virtio-gpu without virgl gives Linux a display and dumb buffers, not a
# command stream a 3D driver could use.
export EGL_PLATFORM=surfaceless
export GALLIUM_DRIVER=llvmpipe
export LIBGL_ALWAYS_SOFTWARE=1
/mnt/lib/ld-linux-aarch64.so.1 --library-path /mnt/lib /mnt/eglprobe
echo "mesa: exit $?"
INIT
chmod +x "$R/nk-init"
( cd "$R" && find . -print | LC_ALL=C sort | cpio -o -H newc 2>/dev/null ) \
    > "$ROOT/kernel/ldk/build/mesa.cpio"

echo "Built $OUT ($(du -h "$OUT" | cut -f1))"
echo "Built $ROOT/kernel/ldk/build/mesa.cpio"
