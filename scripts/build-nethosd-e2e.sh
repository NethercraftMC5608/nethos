#!/bin/bash
# nethosd on nk, end to end: the initrd half. Python comes off the npkg disk.
#
# Same shape as build-net-test.sh (busybox init, ext4 mount, lo up, PYTHON*
# env) but the payload is the real payload/nethosd/nethosd.py, unmodified,
# driven by kernel/init/nethosd-e2e.py: import, serve, GET /api/status.
#
#   scripts/build-nethosd-e2e.sh [full|import-only]
#
# Run scripts/build-npkg-disk.sh first (needs busybox in kernel/ldk/build).
set -euo pipefail
MODE="${1:-full}"
case "$MODE" in
    full|import-only) ;;
    *) echo "usage: build-nethosd-e2e.sh [full|import-only]" >&2; exit 1 ;;
esac
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/kernel/ldk/build"
BB="$BUILD/busybox"
DISK="$BUILD/npkg.img"
[ -f "$BB" ]   || { echo "no busybox at $BB -- run the Busybox test class first" >&2; exit 1; }
[ -f "$DISK" ] || { echo "no $DISK -- run scripts/build-npkg-disk.sh first" >&2; exit 1; }

R="$BUILD/nethosd-e2e-root"
rm -rf "$R"
mkdir -p "$R/bin" "$R/dev" "$R/mnt" "$R/proc" "$R/sys" "$R/tmp"
cp "$BB" "$R/bin/busybox"
cp "$ROOT/payload/nethosd/nethosd.py" "$R/nethosd.py"
cp "$ROOT/kernel/init/nethosd-e2e.py" "$R/nethosd-e2e.py"
cat > "$R/nk-init" <<'INIT'
#!/bin/busybox sh
echo "nethosd-e2e: TRACE 1 shell alive"
busybox mount -t devtmpfs devtmpfs /dev 2>/dev/null
echo "nethosd-e2e: TRACE 2 dev mounted"
busybox mount -t proc proc /proc 2>/dev/null
echo "nethosd-e2e: TRACE 3 proc mounted"
busybox mount -t sysfs sysfs /sys 2>/dev/null
echo "nethosd-e2e: TRACE 4 sys mounted"
busybox mount -t ext4 -o ro /dev/vda /mnt || { echo "nethosd-e2e: mount failed"; exit 1; }
busybox ln -s /mnt/lib /lib
echo "nethosd-e2e: TRACE 5 disk mounted"

# Loopback is not up by default; bind to 127.0.0.1 fails without this.
echo "nethosd-e2e: TRACE 6 bringing lo up"
busybox ip link set lo up 2>/dev/null || busybox ifconfig lo up 2>/dev/null || echo "nethosd-e2e: no ip/ifconfig applet"
echo "nethosd-e2e: TRACE 7 lo up"

# nethosd writes STATE_DIR (~/.local/state/nethos) and DIAG_PATH
# (~/.cache/nethos); give it a writable HOME. PREFIX=/usr/share/nethos,
# shell/lib/apps dirs and /etc/nethos-release are all optional on the
# /api/status path -- absent means defaults, not failures.
export HOME=/tmp/home
export XDG_RUNTIME_DIR=/tmp/xdg
busybox mkdir -p "$HOME" "$XDG_RUNTIME_DIR" /usr/share/nethos/shell /usr/share/nethos/lib /usr/share/nethos/apps

V=$(busybox ls /mnt/lib | busybox grep "^python3\." | busybox head -1)
export LD_LIBRARY_PATH=/mnt/lib
export PYTHONHOME=/mnt
export PYTHONPATH=/mnt/lib/$V:/mnt/lib/$V/lib-dynload
export PYTHONDONTWRITEBYTECODE=1
export PYTHONUNBUFFERED=1
# Opus decisive experiment: MODE=import-only exercises status() with no
# threads; MODE=full spawns the daemon. Export, not VAR=cmd prefix: nk's
# exec carries envp through, but the inline-prefix form is untested there
# and a silent stall before python starts is exactly what it would look like.
export NETHOSD_E2E_MODE="@MODE@"
echo "nethosd-e2e: TRACE 8 env set, launching python"
/mnt/bin/python3 /nethosd-e2e.py
echo "nethosd-e2e: exit $?"
busybox umount /mnt
INIT
chmod +x "$R/nk-init"
sed -i '' "s/@MODE@/$MODE/" "$R/nk-init"
( cd "$R" && find . -print | LC_ALL=C sort | cpio -o -H newc 2>/dev/null ) > "$BUILD/nethosd-e2e-$MODE.cpio"
echo "Built $BUILD/nethosd-e2e-$MODE.cpio"
