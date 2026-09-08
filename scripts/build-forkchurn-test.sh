#!/bin/bash
# Fork/exit churn through Linux's CPU handover, minimally. Seconds per
# iteration, no disk. Same shape as build-exitpoll-test.sh.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/kernel/ldk/build/forkchurn.cpio"
mkdir -p "$(dirname "$OUT")"
TEMP=$(mktemp "${OUT}.XXXXXX")
trap 'rm -f "$TEMP"' EXIT
docker run --rm --platform linux/arm64 -v "$ROOT/kernel/init:/src:ro" nethos-ldk sh -ec '
    mkdir -p /tmp/root/lib/aarch64-linux-gnu /tmp/root/dev
    gcc -fPIE -pie -O2 /src/forkchurn.c -o /tmp/root/nk-init
    cp -L /lib/ld-linux-aarch64.so.1 /tmp/root/lib/
    cp -L /lib/aarch64-linux-gnu/libc.so.6 /tmp/root/lib/aarch64-linux-gnu/
    cd /tmp/root && find . -print | LC_ALL=C sort | cpio -o -H newc
' > "$TEMP"
mv "$TEMP" "$OUT"
echo "Built $OUT"
