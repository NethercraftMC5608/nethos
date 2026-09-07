#!/bin/bash
# Shared-writeback initrd: munmap/msync writeback, private isolation, anon.
#
# Same shape as build-soak-test.sh. /tmp must exist: the probes create
# their files with mkstemp, and Linux's rootfs has no /tmp until the
# archive ships one.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/kernel/ldk/build/writeback.cpio"
mkdir -p "$(dirname "$OUT")"
TEMP=$(mktemp "${OUT}.XXXXXX")
trap 'rm -f "$TEMP"' EXIT
docker run --rm --platform linux/arm64 -v "$ROOT/kernel/init:/src:ro" nethos-ldk sh -ec '
    mkdir -p /tmp/root/etc /tmp/root/lib/aarch64-linux-gnu /tmp/root/dev /tmp/root/tmp
    gcc -fPIE -pie -O2 /src/writeback.c -o /tmp/root/nk-init
    cp -L /lib/ld-linux-aarch64.so.1 /tmp/root/lib/
    cp -L /lib/aarch64-linux-gnu/libc.so.6 /tmp/root/lib/aarch64-linux-gnu/
    cd /tmp/root
    find . -print | LC_ALL=C sort | cpio -o -H newc
' > "$TEMP"
mv "$TEMP" "$OUT"
echo "Built $OUT ($(du -h "$OUT" | cut -f1))"
