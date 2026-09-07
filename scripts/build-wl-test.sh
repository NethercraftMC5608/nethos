#!/bin/bash
# M2 wlprobe initrd + M3 framebuffer initrd: hand-rolled Wayland display +
# client (wlprobe.c, std C only) and the slate framebuffer holder (m3fb.c).
#
#   scripts/build-wl-test.sh [--force] [wl|m3fb|all]
#
# wl (default, M2/M3-step-A): builds kernel/init/wlprobe.c with the docker
# toolchain (gcc -fPIE -pie, nethos-ldk) and packs a cpio initrd whose
# /nk-init sets XDG_RUNTIME_DIR and runs the probe (server forks, client
# connects, prints WL_GLOBAL lines + WL_REGISTRY_OK, then binds wl_shm and
# hands a memfd pool over SCM_RIGHTS for SHM_OK). No disk needed:
# unix-socket IPC only, so boot with run-kernel.sh --lkl --no-build
# --initrd <this.cpio> --timeout 60.
#
# m3fb (M3 step B): builds kernel/init/m3fb.c static and packs an initrd
# whose /nk-init mounts devtmpfs and runs /m3fb: it fills /dev/fb0 with the
# shell's slate pixel, modesets via FBIOPUT_VSCREENINFO + FBIOPAN_DISPLAY
# (folio of fbtest.c), prints M3_SPIN_READY, and sleeps forever so the host
# can screendump the scanout via the QEMU monitor. Boot with
# run-kernel.sh --lkl --no-build --gpu --initrd <m3fb.cpio>.
#
# Single docker run emits ONLY the cpio on stdout (diagnostics go to
# stderr); busybox + nk-init enter via a staged host dir, so no host
# cpio is needed (macOS ships bsdcpio, which cannot read newc).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

FORCE=0
TARGET="wl"
while [ $# -gt 0 ]; do
    case "$1" in
        --force|-f) FORCE=1; shift ;;
        wl|m3fb|all) TARGET="$1"; shift ;;
        -h|--help) sed -n '2,24p' "$0"; exit 0 ;;
        -*) printf 'ERROR: unknown option: %s\n' "$1" >&2; exit 1 ;;
        *) printf 'ERROR: no positional args (usage above)\n' >&2; exit 1 ;;
    esac
done

BB="$ROOT/kernel/ldk/build/busybox"
[ -f "$BB" ] || { echo "no busybox at $BB (shared build dir via symlink ok)" >&2; exit 1; }

build_wl() {
SRC_PATH="$ROOT/kernel/init/wlprobe.c"
OUT_PATH="$ROOT/kernel/ldk/build/wl.cpio"
[ -f "$SRC_PATH" ] || { printf 'ERROR: no source: %s\n' "$SRC_PATH" >&2; exit 1; }
if [ "$FORCE" -eq 0 ] && [ -f "$OUT_PATH" ] && [ "$OUT_PATH" -nt "$SRC_PATH" ]; then
    printf 'Fresh %s\n' "$OUT_PATH"
    return 0
fi

STAGE="$(mktemp -d "${OUT_PATH}.stage.XXXXXX")"
TEMP=$(mktemp "${OUT_PATH}.XXXXXX")
trap 'rm -rf "$STAGE" "$TEMP"' EXIT
mkdir -p "$STAGE/bin"
cp "$BB" "$STAGE/bin/busybox"
cat > "$STAGE/nk-init" <<'INIT'
#!/bin/busybox sh
export XDG_RUNTIME_DIR=/tmp/xdg
busybox mkdir -p "$XDG_RUNTIME_DIR"
/wlprobe
echo "wl: exit $?"
INIT
chmod +x "$STAGE/nk-init"

docker run --rm --platform linux/arm64 \
    -v "$ROOT/kernel/init:/src:ro" \
    -v "$STAGE:/stage:ro" \
    nethos-ldk sh -ec '
    mkdir -p /tmp/wlroot/bin /tmp/wlroot/lib/aarch64-linux-gnu /tmp/wlroot/dev /tmp/wlroot/tmp
    gcc -fPIE -pie -O2 -Wall /src/wlprobe.c -o /tmp/wlroot/wlprobe
    ls -la /tmp/wlroot/wlprobe >&2
    cp -L /lib/ld-linux-aarch64.so.1 /tmp/wlroot/lib/
    cp -L /lib/aarch64-linux-gnu/libc.so.6 /tmp/wlroot/lib/aarch64-linux-gnu/
    cp /stage/bin/busybox /tmp/wlroot/bin/busybox
    cp /stage/nk-init /tmp/wlroot/nk-init
    cd /tmp/wlroot
    find . -print | LC_ALL=C sort | cpio -o -H newc
' > "$TEMP"
mv "$TEMP" "$OUT_PATH"
trap - EXIT
rm -rf "$STAGE"
echo "Built $OUT_PATH ($(du -h "$OUT_PATH" | cut -f1))"
}

build_m3fb() {
SRC_PATH="$ROOT/kernel/init/m3fb.c"
OUT_PATH="$ROOT/kernel/ldk/build/m3fb.cpio"
[ -f "$SRC_PATH" ] || { printf 'ERROR: no source: %s\n' "$SRC_PATH" >&2; exit 1; }
if [ "$FORCE" -eq 0 ] && [ -f "$OUT_PATH" ] && [ "$OUT_PATH" -nt "$SRC_PATH" ]; then
    printf 'Fresh %s\n' "$OUT_PATH"
    return 0
fi

STAGE="$(mktemp -d "${OUT_PATH}.stage.XXXXXX")"
TEMP=$(mktemp "${OUT_PATH}.XXXXXX")
trap 'rm -rf "$STAGE" "$TEMP"' EXIT
mkdir -p "$STAGE/bin" "$STAGE/dev"
cp "$BB" "$STAGE/bin/busybox"
cat > "$STAGE/nk-init" <<'INIT'
#!/bin/busybox sh
busybox mount -t devtmpfs devtmpfs /dev 2>/dev/null
/m3fb
INIT
chmod +x "$STAGE/nk-init"

# Static like fbtest (no loader/libs needed): one file in the root.
docker run --rm --platform linux/arm64 \
    -v "$ROOT/kernel/init:/src:ro" \
    -v "$STAGE:/stage" \
    nethos-ldk sh -ec '
    gcc -static -O2 -Wall /src/m3fb.c -o /tmp/m3fb
    ls -la /tmp/m3fb >&2
    cp /tmp/m3fb /stage/m3fb-tmp
    cp /stage/bin/busybox /stage/busybox-tmp
    rm -rf /tmp/m3root && mkdir -p /tmp/m3root/dev
    mv /stage/m3fb-tmp /tmp/m3root/m3fb
    mkdir -p /tmp/m3root/bin && mv /stage/busybox-tmp /tmp/m3root/bin/busybox
    cp /stage/nk-init /tmp/m3root/nk-init
    cd /tmp/m3root
    find . -print | LC_ALL=C sort | cpio -o -H newc
' > "$TEMP"
mv "$TEMP" "$OUT_PATH"
trap - EXIT
rm -rf "$STAGE"
echo "Built $OUT_PATH ($(du -h "$OUT_PATH" | cut -f1))"
}

case "$TARGET" in
    wl) build_wl ;;
    m3fb) build_m3fb ;;
    all) build_wl; build_m3fb ;;
esac
