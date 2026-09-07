#!/bin/bash
# libdrm through dlopen: the Mesa loading mechanism, at a size that fits.
#
# Mesa itself is 152MB (libLLVM alone is 118) and cannot go in a rootfs that
# lives in Linux's 64MB pool. This builds the same mechanism at 1/1000th the
# size, so the memory work that Mesa needs is the only unknown left.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/kernel/ldk/build/drm.cpio"
mkdir -p "$(dirname "$OUT")"
TEMP=$(mktemp "${OUT}.XXXXXX")
trap 'rm -f "$TEMP"' EXIT
docker run --rm --platform linux/arm64 -v "$ROOT/kernel/init:/src:ro" nethos-ldk sh -ec '
    apt-get -qq update >/dev/null 2>&1
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        libdrm2 >/dev/null 2>&1
    R=/tmp/root
    mkdir -p $R/lib/aarch64-linux-gnu $R/dev $R/tmp
    gcc -fPIE -pie -O2 /src/drmprobe.c -o $R/nk-init -ldl
    cp -L /lib/ld-linux-aarch64.so.1 $R/lib/
    for l in libc.so.6 libm.so.6 libdrm.so.2; do
        cp -L /lib/aarch64-linux-gnu/$l $R/lib/aarch64-linux-gnu/
    done
    cd $R && find . -print | LC_ALL=C sort | cpio -o -H newc
' > "$TEMP"
mv "$TEMP" "$OUT"
echo "Built $OUT ($(du -h "$OUT" | cut -f1))"
