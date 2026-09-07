#!/bin/bash
# Syscall soak initrd: socketpair/SCM_RIGHTS, epoll, eventfd, memfd, poll.
#
# Same shape as build-runtime-test.sh: Debian's arm64 GCC builds one PIE
# binary against a real libc, which becomes /nk-init next to the loader it
# needs. No disk, no network -- every facility here is asked of the LKL
# Linux nk forwards to, so a failure names the forwarding, not the device.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/kernel/ldk/build/soak.cpio"
mkdir -p "$(dirname "$OUT")"
TEMP=$(mktemp "${OUT}.XXXXXX")
trap 'rm -f "$TEMP"' EXIT
docker run --rm --platform linux/arm64 -v "$ROOT/kernel/init:/src:ro" nethos-ldk sh -ec '
    mkdir -p /tmp/root/etc /tmp/root/lib/aarch64-linux-gnu /tmp/root/dev /tmp/root/tmp
    gcc -fPIE -pie -O2 /src/soak.c -o /tmp/root/nk-init
    cp -L /lib/ld-linux-aarch64.so.1 /tmp/root/lib/
    cp -L /lib/aarch64-linux-gnu/libc.so.6 /tmp/root/lib/aarch64-linux-gnu/
    cd /tmp/root
    find . -print | LC_ALL=C sort | cpio -o -H newc
' > "$TEMP"
mv "$TEMP" "$OUT"
echo "Built $OUT ($(du -h "$OUT" | cut -f1))"
