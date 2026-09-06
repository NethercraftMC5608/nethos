#!/bin/bash
# Build nk's uncompressed newc rootfs. All generated files stay in build/.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUTPUT="$ROOT/build/initrd/nethos.cpio"
CACHE="$ROOT/build/initrd/busybox-static-arm64"
IMAGE="nethos-ldk"
VERSION=""
REFRESH=0
COPIES=0
usage() {
    cat <<'HELP'
Usage: scripts/build-initrd.sh [options]
  --output FILE           Archive (default: build/initrd/nethos.cpio)
  --cache DIR             BusyBox inputs: busybox, applets, version
  --image NAME            Linux build image (default: nethos-ldk)
  --package-version VER   Require this exact Debian busybox-static version
  --refresh               Replace cached inputs from Debian
  --copies                Copy BusyBox for each applet instead of symlinks
  -h, --help              Show this help

A populated cache allows offline rebuilds without Docker. Archive ownership is
root:root; timestamps use SOURCE_DATE_EPOCH (default 0). See payload/initrd/README.md.
HELP
}
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output) OUTPUT="${2:?--output needs a file}"; shift 2 ;;
        --cache) CACHE="${2:?--cache needs a directory}"; shift 2 ;;
        --image) IMAGE="${2:?--image needs a name}"; shift 2 ;;
        --package-version) VERSION="${2:?--package-version needs a version}"; shift 2 ;;
        --refresh) REFRESH=1; shift ;;
        --copies) COPIES=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) printf 'Unknown option: %s\n' "$1" >&2; usage >&2; exit 2 ;;
    esac
done
command -v python3 >/dev/null || { echo 'python3 is required' >&2; exit 1; }
mkdir -p "$(dirname "$OUTPUT")" "$(dirname "$CACHE")"
WORK=$(mktemp -d "$(dirname "$CACHE")/.initrd-inputs.XXXXXX")
trap 'rm -rf "$WORK"' EXIT
if [ "$REFRESH" -eq 1 ] || [ ! -f "$CACHE/busybox" ] || [ ! -f "$CACHE/applets" ] || [ ! -f "$CACHE/version" ]; then
    command -v docker >/dev/null || { echo 'Docker is required to fetch BusyBox (or supply a populated --cache)' >&2; exit 1; }
    # Package-manager output must never enter the binary stream. Query the
    # installed binary itself so its applets always match this exact build.
    docker run --rm --platform linux/arm64 -e "BB_VERSION=$VERSION" "$IMAGE" sh -ec '
        apt-get update -qq >&2
        package=busybox-static
        if [ -n "$BB_VERSION" ]; then package="$package=$BB_VERSION"; fi
        DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends "$package" >&2
        mkdir -p /tmp/nk-busybox
        cp /bin/busybox /tmp/nk-busybox/busybox
        /bin/busybox --list > /tmp/nk-busybox/applets
        dpkg-query -W busybox-static | cut -f2 > /tmp/nk-busybox/version
        tar -C /tmp/nk-busybox -cf - busybox applets version
    ' > "$WORK/bundle.tar"
    python3 - "$WORK" <<'PY'
import pathlib, sys, tarfile
work = pathlib.Path(sys.argv[1])
with tarfile.open(work / 'bundle.tar') as bundle:
    for name in ('busybox', 'applets', 'version'):
        member = bundle.getmember(name)
        if not member.isfile():
            raise SystemExit(f'Unexpected BusyBox bundle member: {name}')
        (work / name).write_bytes(bundle.extractfile(member).read())
PY
    INPUT="$WORK"
else
    INPUT="$CACHE"
fi
python3 - "$ROOT/payload/initrd" "$INPUT" "$OUTPUT" "$VERSION" "$COPIES" <<'PY'
import hashlib
import os
from pathlib import Path
import struct
import sys
import tempfile

payload, inputs, output = map(Path, sys.argv[1:4])
requested, copies = sys.argv[4], sys.argv[5] == '1'
binary = (inputs / 'busybox').read_bytes()
version = (inputs / 'version').read_text().strip()
if not version or (requested and version != requested):
    raise SystemExit(f'BusyBox version mismatch: requested {requested!r}, cached {version!r}; use --refresh')
# Refuse the wrong architecture or a binary needing an absent dynamic linker.
if len(binary) < 64 or binary[:7] != b'\x7fELF\x02\x01\x01' or struct.unpack_from('<H', binary, 18)[0] != 183:
    raise SystemExit('BusyBox must be a little-endian AArch64 ELF64 executable')
phoff = struct.unpack_from('<Q', binary, 32)[0]
phsize, phnum = struct.unpack_from('<HH', binary, 54)
if phsize != 56 or not phnum or phoff + phsize * phnum > len(binary):
    raise SystemExit('Malformed BusyBox ELF program headers')
if any(struct.unpack_from('<I', binary, phoff + phsize * i)[0] == 3 for i in range(phnum)):
    raise SystemExit('BusyBox requires a dynamic linker; use busybox-static')
applets = sorted(set((inputs / 'applets').read_text().splitlines()) - {'busybox'})
if not {'sh', 'cat', 'ls'}.issubset(applets):
    raise SystemExit('Applet list must include sh, cat and ls')
if any(not n or n in ('.', '..', 'busybox') or '/' in n or '\0' in n or any(c.isspace() for c in n) for n in applets):
    raise SystemExit('Invalid BusyBox applet name')
epoch = int(os.environ.get('SOURCE_DATE_EPOCH', '0'))
if not 0 <= epoch <= 0xffffffff:
    raise SystemExit('SOURCE_DATE_EPOCH must fit an unsigned 32-bit newc field')
entries = {name: (0o40755, b'') for name in ('bin', 'dev', 'etc', 'proc')}
entries['tmp'] = (0o41777, b'')
entries['bin/busybox'] = entries['nk-init'] = (0o100755, binary)
for name in applets:
    entries['bin/' + name] = (0o100755, binary) if copies else (0o120777, b'busybox')
for name in ('passwd', 'group', 'hostname'):
    entries['etc/' + name] = (0o100644, (payload / 'etc' / name).read_bytes())
# A temporary neighbour plus rename keeps a failed build from replacing a
# working archive. Every entry has a stable inode, uid/gid, mode and mtime.
fd, temporary = tempfile.mkstemp(prefix='.initrd-', dir=output.parent)
try:
    with os.fdopen(fd, 'wb') as archive:
        def entry(name, mode, data, ino):
            name = name.encode() + b'\0'
            fields = (ino, mode, 0, 0, 2 if mode & 0o170000 == 0o40000 else 1,
                      epoch, len(data), 0, 0, 0, 0, len(name), 0)
            archive.write(b'070701' + ''.join(f'{v:08x}' for v in fields).encode())
            archive.write(name)
            archive.write(b'\0' * (-archive.tell() % 4))
            archive.write(data)
            archive.write(b'\0' * (-archive.tell() % 4))
        for ino, (name, (mode, data)) in enumerate(sorted(entries.items()), 1):
            entry(name, mode, data, ino)
        entry('TRAILER!!!', 0, b'', len(entries) + 1)
        archive.write(b'\0' * (-archive.tell() % 512))
    os.replace(temporary, output)
finally:
    if os.path.exists(temporary):
        os.unlink(temporary)
print(f'Built {output.resolve()} ({output.stat().st_size} bytes)')
print(f'busybox-static {version}, arm64; {len(applets)} applets as ' + ('copies' if copies else 'symlinks'))
print(f'BusyBox SHA256: {hashlib.sha256(binary).hexdigest()}')
if copies:
    print('Full applet copies exceed nk\'s current 64 MiB Linux pool; use symlink support for booting.', file=sys.stderr)
PY
if [ "$INPUT" = "$WORK" ]; then
    mkdir -p "$CACHE"
    cp "$WORK/busybox" "$WORK/applets" "$WORK/version" "$CACHE/"
fi
