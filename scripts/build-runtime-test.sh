#!/bin/bash
# Debian dynamic linker/libc + a private-mmap and pthread integration probe.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/kernel/ldk/build/runtime.cpio"
mkdir -p "$(dirname "$OUT")"
TEMP=$(mktemp "${OUT}.XXXXXX")
trap 'rm -f "$TEMP"' EXIT
docker run --rm --platform linux/arm64 -v "$ROOT/kernel/init:/src:ro" nethos-ldk sh -ec '
    mkdir -p /tmp/root/etc /tmp/root/lib/aarch64-linux-gnu /tmp/root/dev /tmp/root/tmp
    gcc -fPIE -pie -O2 -pthread /src/runtime.c -o /tmp/root/nk-init
    cp -L /lib/ld-linux-aarch64.so.1 /tmp/root/lib/
    cp -L /lib/aarch64-linux-gnu/libc.so.6 /tmp/root/lib/aarch64-linux-gnu/
    python3 -c "open(\"/tmp/root/etc/mapping-data\",\"wb\").write(bytes(4096)+b\"second page\")"
    cd /tmp/root
    find . -print | LC_ALL=C sort | cpio -o -H newc
' > "$TEMP"
mv "$TEMP" "$OUT"
echo "Built $OUT"
