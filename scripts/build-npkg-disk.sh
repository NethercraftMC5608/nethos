#!/bin/bash
# npkg on nk: NETHOS's package manager, on NETHOS's kernel.
#
# npkg is pure-stdlib Python, so this is really a CPython port -- and CPython
# is a harder test of a kernel than anything nk has run. It dlopens forty
# extension modules, threads through concurrent.futures, maps its stdlib,
# and installs signal handlers on the way up.
#
# The repository is a local directory, so an install is entirely offline and
# still the real thing: resolve dependencies, verify sha256, unpack, record
# in the database. Networking is a separate problem and not this one.
#
# Python and npkg go on an ext4 disk for the same reason Mesa did: nk's
# rootfs is an initrd in Linux's 64MB pool and the stdlib alone is 29MB.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BUILD="$ROOT/kernel/ldk/build"
OUT="$BUILD/npkg.img"
SIZE="${SIZE:-256M}"
mkdir -p "$BUILD"
rm -f "$OUT"

# The repository, built by npkg itself on the host. Two packages with a
# dependency between them, because a package manager that cannot order an
# install is not one -- resolving nk-tools has to pull in nk-greeting.
REPO="$BUILD/npkg-repo"
rm -rf "$REPO"; mkdir -p "$REPO"
python3 - "$ROOT" "$REPO" <<'PY'
import os, sys, tempfile
root, repo = sys.argv[1], sys.argv[2]
sys.path.insert(0, os.path.join(root, "pkg"))
from npkg import Manifest, Package, build_index

def make(manifest, files):
    with tempfile.TemporaryDirectory() as d:
        for path, text in files.items():
            full = os.path.join(d, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            with open(full, "w") as fh:
                fh.write(text)
        out = os.path.join(repo, f"{manifest.name}-{manifest.version}.npk")
        Package.create(manifest, d, out)

make(Manifest(name="nk-greeting", version="1.0", arch="any",
              summary="a file to prove an install happened"),
     {"usr/share/nk/greeting": "installed by npkg, running on nk\n"})

make(Manifest(name="nk-tools", version="2.1", arch="any",
              summary="depends on nk-greeting, so the solver has work to do",
              depends=["nk-greeting"]),
     {"usr/share/nk/tools.txt": "nk-tools 2.1\n",
      "usr/share/nk/notes/README": "a second file, in a subdirectory\n"})

index = build_index(repo)
print(f"  repo: {len(index['packages'])} packages")
PY

docker run --rm --platform linux/arm64 \
    -v "$ROOT/pkg:/pkg:ro" -v "$REPO:/repo:ro" -v "$BUILD:/out" nethos-ldk sh -ec '
    apt-get -qq update >/dev/null 2>&1
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        python3 python3-minimal e2fsprogs >/dev/null 2>&1

    R=/tmp/npkg
    mkdir -p $R/lib $R/bin $R/npkg $R/repo

    V=$(python3 -c "import sys; print(f\"{sys.version_info[0]}.{sys.version_info[1]}\")")
    cp -L /usr/bin/python$V $R/bin/python3
    cp -a /usr/lib/python$V $R/lib/python$V
    # Compiled caches are stale the moment the paths change and are dead
    # weight on a 64MB machine; Python regenerates what it needs.
    find $R/lib/python$V -name __pycache__ -type d -exec rm -rf {} + 2>/dev/null || true

    # The closure: the interpreter, plus every extension module it dlopens.
    # lib-dynload is the reason a static ldd of the binary is not enough --
    # _hashlib and the codecs are opened at runtime and pull their own.
    for f in /usr/bin/python$V $R/lib/python$V/lib-dynload/*.so; do
        ldd $f 2>/dev/null | sed -n "s/.*=> \(\/[^ ]*\).*/\1/p"
    done | sort -u | while read -r l; do cp -Ln "$l" $R/lib/ 2>/dev/null || true; done
    cp -L /lib/ld-linux-aarch64.so.1 $R/lib/

    cp /pkg/npkg*.py $R/npkg/
    cp -r /repo/. $R/repo/

    echo "  payload: $(du -sh $R | cut -f1)"
    mke2fs -q -t ext4 -d $R -F /out/npkg.img '"$SIZE"'
'

BB="$BUILD/busybox"
[ -f "$BB" ] || { echo "no busybox at $BB -- run the Busybox test class first" >&2; exit 1; }
R="$BUILD/npkg-root"
rm -rf "$R"
mkdir -p "$R/bin" "$R/dev" "$R/mnt" "$R/proc" "$R/sys" "$R/tmp"
cp "$BB" "$R/bin/busybox"
cat > "$R/nk-init" <<'INIT'
#!/bin/busybox sh
busybox mount -t devtmpfs devtmpfs /dev 2>/dev/null
busybox mount -t proc proc /proc 2>/dev/null
busybox mount -t sysfs sysfs /sys 2>/dev/null
# Read-only, and the install root is in Linux's tmpfs rather than on the
# disk. A test that installs into the image it booted from passes once and
# then finds the packages already there -- which is persistence working, and
# a test not working.
busybox mount -t ext4 -o ro /dev/vda /mnt || { echo "npkg: mount failed"; exit 1; }
echo "npkg: disk mounted"
# PT_INTERP is an absolute path baked into the binary -- /lib/ld-linux, not
# wherever the disk happens to be mounted -- and nk resolves it through
# Linux's VFS like any other open. So the rootfs needs a /lib.
busybox ln -s /mnt/lib /lib

V=$(busybox ls /mnt/lib | busybox grep "^python3\." | busybox head -1)
export LD_LIBRARY_PATH=/mnt/lib
export PYTHONHOME=/mnt
export PYTHONPATH=/mnt/lib/$V:/mnt/lib/$V/lib-dynload:/mnt/npkg
# Byte-code caches would be written back to a read-only-ish layout and are
# not what is being tested.
export PYTHONDONTWRITEBYTECODE=1
export PYTHONUNBUFFERED=1
# Run the binary, not the loader with the binary as an argument. Debian's
# python3 is ET_EXEC -- not a PIE -- so it must land at the 0x400000 baked
# into it, and invoking ld.so by hand puts the loader there first. Executed
# directly, nk reads PT_INTERP and places the interpreter out of the way,
# which is what Linux does and why this works there.
PY="/mnt/bin/python3"

echo "--- interpreter"
$PY -c 'import sys; print("PYTHON_OK", sys.version.split()[0])' || { echo "npkg: python failed"; exit 1; }

echo "--- npkg"
# A local repository: no network, and still a real resolve-verify-unpack.
busybox mkdir -p /tmp/target/etc/npkg
echo '{"repos": [{"name": "nk", "url": "/mnt/repo"}]}' > /tmp/target/etc/npkg/repos.json
NPKG="$PY /mnt/npkg/npkg.py --root /tmp/target"
$NPKG list
echo "--- install"
$NPKG install nk-tools
echo "--- list"
$NPKG list
echo "--- files"
$NPKG files nk-tools
echo "--- owns"
$NPKG owns /usr/share/nk/greeting
echo "--- verify"
$NPKG verify
echo "--- content"
busybox cat /tmp/target/usr/share/nk/greeting
echo "npkg: exit $?"
busybox umount /mnt
INIT
chmod +x "$R/nk-init"
( cd "$R" && find . -print | LC_ALL=C sort | cpio -o -H newc 2>/dev/null ) > "$BUILD/npkg.cpio"

echo "Built $OUT ($(du -h "$OUT" | cut -f1))"
echo "Built $BUILD/npkg.cpio"
