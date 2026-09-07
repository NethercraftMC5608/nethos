#!/bin/bash
# Can nethosd run on nk? The initrd half; Python comes off the npkg disk.
#
# nethosd is the desktop's API and it is stdlib Python on a
# ThreadingHTTPServer, so the gate is not the language -- npkg settled that --
# but whether nk gives Linux a loopback interface and an AF_INET stack. This
# reuses kernel/ldk/build/npkg.img rather than building a second 35MB
# interpreter: run scripts/build-npkg-disk.sh first.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/kernel/ldk/build"
BB="$BUILD/busybox"
DISK="$BUILD/npkg.img"
[ -f "$BB" ]   || { echo "no busybox at $BB -- run the Busybox test class first" >&2; exit 1; }
[ -f "$DISK" ] || { echo "no $DISK -- run scripts/build-npkg-disk.sh first" >&2; exit 1; }

R="$BUILD/net-root"
rm -rf "$R"
mkdir -p "$R/bin" "$R/dev" "$R/mnt" "$R/proc" "$R/sys" "$R/tmp"
cp "$BB" "$R/bin/busybox"
cp "$ROOT/kernel/init/netprobe.py" "$R/netprobe.py"
cat > "$R/nk-init" <<'INIT'
#!/bin/busybox sh
busybox mount -t devtmpfs devtmpfs /dev 2>/dev/null
busybox mount -t proc proc /proc 2>/dev/null
busybox mount -t sysfs sysfs /sys 2>/dev/null
busybox mount -t ext4 -o ro /dev/vda /mnt || { echo "net: mount failed"; exit 1; }
busybox ln -s /mnt/lib /lib
echo "net: disk mounted"

# Loopback is not up by default: Linux creates `lo` and leaves it down, and a
# bind to 127.0.0.1 on a down interface fails in a way that reads like a
# missing stack. Bring it up first so a failure below is the stack's.
busybox ip link set lo up 2>/dev/null || busybox ifconfig lo up 2>/dev/null || echo "net: no ip/ifconfig applet"
echo "net: interfaces:"
busybox ip -o link show 2>/dev/null; busybox ip -o addr show 2>/dev/null || busybox ifconfig -a 2>/dev/null | busybox head -6

V=$(busybox ls /mnt/lib | busybox grep "^python3\." | busybox head -1)
export LD_LIBRARY_PATH=/mnt/lib
export PYTHONHOME=/mnt
export PYTHONPATH=/mnt/lib/$V:/mnt/lib/$V/lib-dynload
export PYTHONDONTWRITEBYTECODE=1
export PYTHONUNBUFFERED=1
/mnt/bin/python3 /netprobe.py
echo "net: exit $?"
busybox umount /mnt
INIT
chmod +x "$R/nk-init"
( cd "$R" && find . -print | LC_ALL=C sort | cpio -o -H newc 2>/dev/null ) > "$BUILD/net.cpio"
echo "Built $BUILD/net.cpio"
