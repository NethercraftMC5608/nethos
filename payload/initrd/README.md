# nk initrd

Build an uncompressed `newc` cpio archive from Debian's arm64
`busybox-static` and the account files in `etc/`:

```sh
bash scripts/build-initrd.sh
bash scripts/run-kernel.sh --lkl --initrd build/initrd/nethos.cpio --timeout 60
```

The first build needs Docker and the existing `nethos-ldk` image. Package
installation logs go to stderr; only the BusyBox bundle travels through
stdout. The builder obtains `busybox --list` from the installed binary and
caches the binary, applet list and Debian version together under
`build/initrd/busybox-static-arm64/`. Subsequent builds use that cache and
need only Bash and Python 3, including on macOS.

The archive contains `/bin/busybox`, every other applet as a relative symlink
beside it (`/bin/cat -> busybox`, including `sh`, `[` and `[[`), and `/nk-init`
as a full executable copy. BusyBox lists itself as an applet; the builder
keeps its real binary instead of replacing it with a self-referencing link.
The nk unpacker now creates symlinks using Linux `symlinkat`.

`/dev`, `/tmp`, `/proc` and `/etc` exist. `/tmp` has mode 1777; other
directories use 0755, executables 0755 and account files 0644. The archive
contains no device nodes: nk creates `/dev/console`. Proc is only a mount
point, not a mounted procfs. The root account uses `/bin/sh`; this minimal
payload does not configure password login.

All archive entries have uid/gid zero, sorted names, stable inode numbers,
and timestamps from `SOURCE_DATE_EPOCH` (zero by default). Identical cached
inputs and payload files produce byte-identical output. An uncached build
selects the current Debian package; use `--package-version VERSION` to pin
it, or retain the cached inputs for offline reproduction. The builder prints
the installed package version and binary SHA256. `--refresh` deliberately
replaces cached inputs. It does not silently replace a mismatched pinned
version. `--output FILE`, `--cache DIR` and `--image NAME` override paths and
the build image; paths containing spaces are supported.

`--copies` emits full regular-file applet copies for unpackers without
symlinks. This produces hundreds of megabytes and exceeds nk's current
64 MiB Linux memory pool; the default symlink archive avoids that cost.

## Startup contract

`/nk-init` is a copy of BusyBox as requested, not an init script or a renamed
shell. BusyBox selects an applet from `argv[0]`: nk must invoke it with an
appropriate name/arguments (for example `sh`), or execute `/bin/sh` instead.
With the current `/nk-init` argv[0], BusyBox reports `nk-init: applet not
found`. The archive builder does not modify the kernel's process startup.

## Builder checks

```sh
python3 payload/initrd/check_builder.py
```

These offline tests inspect archive headers, layout, permissions, applet
links/copies, determinism, version pinning and rejection of wrong-architecture
or dynamically linked binaries. They also check that a failed build preserves
an existing archive. The Python fixture is not part of the initrd: only the
three explicitly named `etc/` files are included from this directory.

Verified on nk: the default archive unpacks all 281 entries, then BusyBox
exits 127 because of the startup name above. A separate diagnostic archive
with a small launcher executing `/bin/sh -c 'exec cat /etc/hostname'` prints
`nethos` and exits zero in both processes, exercising both applet symlinks.
A longer shell sequence currently encounters a separate repeated-fork error
513 after its first successful cat; that kernel path is outside this change.
