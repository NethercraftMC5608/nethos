#!/bin/bash
# Build one EL0 probe initrd: a PIE binary plus its loader and libc.
#
#   scripts/build-probe.sh soak|writeback|signals|runtime|drm [--force]
#
# One script instead of five because build-soak/writeback/signals/runtime/
# drm-test.sh were the same docker invocation with a different source file.
# Five copies means five places to fix the next rootfs layout change; the
# per-probe differences live in the case statement below.
#
# scripts/nk-verify.sh calls this for each probe it needs. Run it directly
# when iterating on one probe: it skips the build when the cpio is newer
# than the source, unless --force.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

PROBE=""
FORCE=0
while [ $# -gt 0 ]; do
    case "$1" in
        --force|-f) FORCE=1; shift ;;
        -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
        -*) printf 'ERROR: unknown option: %s\n' "$1" >&2; exit 1 ;;
        *) if [ -n "$PROBE" ]; then printf 'ERROR: one probe per run\n' >&2; exit 1; fi
           PROBE="$1"; shift ;;
    esac
done
[ -n "$PROBE" ] || { sed -n '2,12p' "$0"; exit 1; }

SRC=""; OUT=""; CFLAGS="-O2"; LIBS="libc.so.6"; LINK=""; APT=""; EXTRA=""
case "$PROBE" in
    soak)
        SRC="soak.c"; OUT="soak.cpio" ;;
    writeback)
        SRC="writeback.c"; OUT="writeback.cpio" ;;
    signals)
        SRC="signals.c"; OUT="signals.cpio" ;;
    runtime)
        SRC="runtime.c"; OUT="runtime.cpio"
        CFLAGS="-O2 -pthread"; EXTRA="mapping-data" ;;
    drm)
        SRC="drmprobe.c"; OUT="drm.cpio"
        LIBS="libc.so.6 libm.so.6 libdrm.so.2"; LINK="-ldl"; APT="libdrm2" ;;
    *) printf 'ERROR: unknown probe: %s (soak|writeback|signals|runtime|drm)\n' "$PROBE" >&2; exit 1 ;;
esac

SRC_PATH="$ROOT/kernel/init/$SRC"
OUT_PATH="$ROOT/kernel/ldk/build/$OUT"
[ -f "$SRC_PATH" ] || { printf 'ERROR: no source: %s\n' "$SRC_PATH" >&2; exit 1; }
if [ "$FORCE" -eq 0 ] && [ -f "$OUT_PATH" ] && [ "$OUT_PATH" -nt "$SRC_PATH" ]; then
    printf 'Fresh %s\n' "$OUT_PATH"
    exit 0
fi

TEMP=$(mktemp "${OUT_PATH}.XXXXXX")
trap 'rm -f "$TEMP"' EXIT
docker run --rm --platform linux/arm64 \
    -v "$ROOT/kernel/init:/src:ro" \
    -e "PROBE_SRC=$SRC" -e "PROBE_CFLAGS=$CFLAGS" \
    -e "PROBE_LIBS=$LIBS" -e "PROBE_LINK=$LINK" \
    -e "PROBE_APT=$APT" -e "PROBE_EXTRA=$EXTRA" \
    nethos-ldk sh -ec '
    mkdir -p /tmp/root/etc /tmp/root/lib/aarch64-linux-gnu /tmp/root/dev /tmp/root/tmp
    if [ -n "$PROBE_APT" ]; then
        apt-get -qq update >/dev/null 2>&1
        DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends $PROBE_APT >/dev/null 2>&1
    fi
    # shellcheck disable=SC2086: flags and libs split on purpose.
    gcc -fPIE -pie $PROBE_CFLAGS /src/$PROBE_SRC -o /tmp/root/nk-init $PROBE_LINK
    cp -L /lib/ld-linux-aarch64.so.1 /tmp/root/lib/
    for l in $PROBE_LIBS; do
        cp -L /lib/aarch64-linux-gnu/$l /tmp/root/lib/aarch64-linux-gnu/
    done
    if [ "$PROBE_EXTRA" = "mapping-data" ]; then
        python3 -c "open(\"/tmp/root/etc/mapping-data\",\"wb\").write(bytes(4096)+b\"second page\")"
    fi
    cd /tmp/root
    find . -print | LC_ALL=C sort | cpio -o -H newc
' > "$TEMP"
mv "$TEMP" "$OUT_PATH"
echo "Built $OUT_PATH ($(du -h "$OUT_PATH" | cut -f1))"
