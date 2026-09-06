#!/bin/bash
# Boot nk -- the NETHOS kernel -- on QEMU's aarch64 `virt` machine.
#
#   scripts/run-kernel.sh                  build and boot, serial on stdio
#   scripts/run-kernel.sh --debug          debug profile (no LTO, real panics)
#   scripts/run-kernel.sh --no-build       boot what is already built
#   scripts/run-kernel.sh --disk FILE      attach FILE as virtio-blk  (stage 3)
#   scripts/run-kernel.sh --net            attach virtio-net, user mode (stage 4)
#   scripts/run-kernel.sh --smp N          more CPUs than the one boot.s uses
#   scripts/run-kernel.sh --port NAME      link an ldk port's Linux drivers in
#   scripts/run-kernel.sh --tcg            emulate instead of using HVF
#   scripts/run-kernel.sh --gdb            wait for gdb on :1234
#   scripts/run-kernel.sh --timeout N      kill after N seconds (for tests)
#
# Nothing here touches build/, the images, or anything scripts/run.sh uses.
# This is the sibling kernel; run.sh still boots NETHOS proper on Debian's.
#
# Quit QEMU with Ctrl-A X. The serial console is the only output there is --
# there is no display, deliberately: nk cannot draw yet and an empty window
# only makes it look as though something failed.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PROFILE=release
BUILD=1
SMP=1
MEM=512
DISK=""
NET=0
GDB=0
TCG=0
PORT=""
TIMEOUT=0

while [ $# -gt 0 ]; do
    case "$1" in
        --debug)     PROFILE=debug; shift ;;
        --release)   PROFILE=release; shift ;;
        --no-build)  BUILD=0; shift ;;
        --disk)      DISK="${2:?--disk needs a file}"; shift 2 ;;
        --net)       NET=1; shift ;;
        --smp)       SMP="${2:?--smp needs a count}"; shift 2 ;;
        --mem)       MEM="${2:?--mem needs MB}"; shift 2 ;;
        --port)      PORT="${2:?--port needs a name, e.g. virtio-blk}"; shift 2 ;;
        --tcg)       TCG=1; shift ;;
        --gdb)       GDB=1; shift ;;
        --timeout)   TIMEOUT="${2:?--timeout needs seconds}"; shift 2 ;;
        -h|--help)   sed -n '2,20p' "$0"; exit 0 ;;
        *) printf '\033[1;31mERROR:\033[0m unknown option: %s\n' "$1" >&2; exit 1 ;;
    esac
done

die() { printf '\033[1;31mERROR:\033[0m %s\n' "$*" >&2; exit 1; }
say() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }

# rustup installed by Homebrew is keg-only, so its bin directory is not on a
# default PATH and `cargo` is simply missing in a fresh shell. Add both the
# keg and the cargo home rather than telling everyone to edit their profile.
export PATH="/opt/homebrew/opt/rustup/bin:$HOME/.cargo/bin:$PATH"

# --port links an ldk port's Linux drivers into the image. Without it nk builds
# exactly as it did before Stage 3, which keeps it buildable and testable on a
# machine with no docker and no Linux tree.
if [ -n "$PORT" ]; then
    LIB="$ROOT/kernel/ldk/build/$PORT/libnklinux.a"
    [ -f "$LIB" ] || die "no archive for $PORT.  cd kernel/ldk && python3 ldk.py build $PORT && python3 ldk.py shim $PORT"
    export NK_LINUX_LIB="$LIB"
    say "Linking $PORT ($(du -h "$LIB" | cut -f1))"
fi

if [ "$BUILD" -eq 1 ]; then
    command -v cargo >/dev/null || die "cargo is missing.  brew install rustup && rustup default stable"
    say "Building nk ($PROFILE)"
    if [ "$PROFILE" = release ]; then
        ( cd "$ROOT/kernel" && cargo build --release )
    else
        ( cd "$ROOT/kernel" && cargo build )
    fi
fi

ELF="$ROOT/kernel/target/aarch64-unknown-none-softfloat/$PROFILE/nk"
[ -f "$ELF" ] || die "no kernel at $ELF -- drop --no-build"

# QEMU is given the flat binary, not the ELF. Handed an ELF it loads the
# segments, jumps to the entry point, and passes no device tree at all; handed
# an image carrying the arm64 header boot.s starts with, it takes the Linux
# boot path and enters with x0 pointing at a generated DTB. The ELF is still
# built and kept, because it is what carries the symbols gdb needs.
KERNEL="$ELF.bin"
OBJCOPY=$(command -v llvm-objcopy || true)
if [ -z "$OBJCOPY" ]; then
    # rustup ships llvm-objcopy in the llvm-tools component, inside the
    # toolchain rather than on PATH. Preferred over a system objcopy: macOS's
    # is cctools' and does not know ELF at all.
    for c in "$HOME"/.rustup/toolchains/*/lib/rustlib/*/bin/llvm-objcopy; do
        [ -x "$c" ] && { OBJCOPY="$c"; break; }
    done
fi
[ -n "$OBJCOPY" ] || die "llvm-objcopy is missing.  rustup component add llvm-tools"
if [ "$BUILD" -eq 1 ] || [ ! -f "$KERNEL" ]; then
    "$OBJCOPY" -O binary "$ELF" "$KERNEL"
fi

command -v qemu-system-aarch64 >/dev/null || die "qemu-system-aarch64 is missing (brew install qemu)"

# Same test scripts/run.sh uses: HVF is Apple's hypervisor and runs ARM code
# natively. -cpu host only means anything under HVF; under TCG it is not a
# valid model, so the two move together.
# --tcg forces emulation. Worth having as a first-class option rather than
# something to hack in: it is the one experiment that separates "nk is wrong"
# from "the hypervisor cannot do this", and that distinction has already been
# the whole answer once -- see the writeback note in kernel/core/src/mmio.rs.
ACCEL=tcg
CPU=max
if [ "$TCG" -eq 0 ] && [ "$(uname -m)" = "arm64" ] && [ "$(sysctl -n kern.hv_support 2>/dev/null)" = "1" ]; then
    ACCEL=hvf
    CPU=host
fi

ARGS=(
    -name "nk"
    # No pflash and no EDK2. -kernel loads the ELF straight to the address it
    # is linked at and jumps there, which is the whole boot protocol nk needs;
    # UEFI would only add a firmware to debug through.
    # gic-version=3 explicitly: QEMU's default on `virt` is still GICv2
    # (arm,cortex-a15-gic in the device tree), and GICv3 is what every ARM
    # machine made since about 2015 actually has. Verified to work under HVF.
    -machine "virt,accel=$ACCEL,gic-version=3"
    -cpu "$CPU"
    -smp "$SMP"
    -m "$MEM"
    -kernel "$KERNEL"
    -serial mon:stdio
    -display none
)

# if=none + an explicit device, so the driver under test is the one named
# rather than whatever QEMU picks for a bare `if=virtio`.
if [ -n "$DISK" ]; then
    [ -f "$DISK" ] || die "no such disk image: $DISK"
    ARGS+=( -drive "file=$DISK,if=none,format=raw,id=nkdisk"
            -device virtio-blk-device,drive=nkdisk )
fi

if [ "$NET" -eq 1 ]; then
    ARGS+=( -netdev user,id=nknet -device virtio-net-device,netdev=nknet )
fi

if [ "$GDB" -eq 1 ]; then
    ARGS+=( -S -gdb tcp::1234 )
    say "Waiting for gdb on :1234"
    say "  target remote :1234  &&  symbol-file $ELF"
fi

say "aarch64 · accel=$ACCEL · ${SMP} cpu · ${MEM}MB · nk.bin ($PROFILE, $(wc -c <"$KERNEL" | tr -d ' ') bytes)"

if [ "$TIMEOUT" -gt 0 ]; then
    # For the tests: QEMU has no self-imposed deadline and a kernel that hangs
    # would hang CI with it. No coreutils `timeout` on a stock macOS, hence
    # the background-and-wait rather than a one-liner.
    qemu-system-aarch64 "${ARGS[@]}" &
    qpid=$!
    ( sleep "$TIMEOUT"; kill -TERM "$qpid" 2>/dev/null ) &
    watchdog=$!
    wait "$qpid" || true
    kill -TERM "$watchdog" 2>/dev/null || true
else
    exec qemu-system-aarch64 "${ARGS[@]}"
fi
