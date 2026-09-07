#!/bin/bash
# Verify nk EL0 probes in one invocation: one kernel build, one boot each.
#
#   scripts/nk-verify.sh [--no-build] [--no-build-probes] [--timeout N] [probe ...]
#   scripts/nk-verify.sh --list
#
# Each shell invocation costs a round trip and each run-kernel.sh boot
# rebuilds or re-links the kernel, so running soak/writeback/signals as
# separate calls meant one kernel build per probe for a single question
# ("is the desktop path green?"). This builds the kernel once
# (--lkl --build-only), builds any missing probe initrds, then boots each
# probe with --no-build and checks its markers. Probes default to soak
# writeback signals; runtime and drm are opt-in (drm needs --gpu and
# minutes, so it never runs by default).
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

BUILD_KERNEL=1
BUILD_PROBES=1
TIMEOUT=30
PROBES=()
while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) BUILD_KERNEL=0; shift ;;
        --no-build-probes) BUILD_PROBES=0; shift ;;
        --timeout) TIMEOUT="${2:?--timeout needs seconds}"; shift 2 ;;
        --list)
            printf 'soak      SOAK_SOCKETPAIR_OK SOAK_SCM_RIGHTS_OK SOAK_EPOLL_OK SOAK_EVENTFD_OK SOAK_MEMFD_OK SOAK_POLL_OK\n'
            printf 'writeback WB_MUNMAP_OK WB_MSYNC_OK WB_PRIVATE_OK WB_ANON_OK\n'
            printf 'signals   SIG_HANDLER_OK SIG_INFO_OK SIG_CHLD_OK SIG_DISP_OK\n'
            printf 'runtime   DYNAMIC_LIBC_OK PRIVATE_FILE_MMAP_OK FIXED_MAPPING_OK PTHREAD_TLS_JOIN_OK\n'
            printf 'drm       DEVTMPFS_OK DLOPEN_OK DLSYM_OK DRMPROBE_OK + [DRM_DRIVER virtio_gpu] (boots with --gpu)\n'
            exit 0 ;;
        -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
        -*) printf 'ERROR: unknown option: %s\n' "$1" >&2; exit 1 ;;
        soak|writeback|signals|runtime|drm) PROBES+=("$1"); shift ;;
        all) PROBES+=(soak writeback signals); shift ;;
        *) printf 'ERROR: unknown probe: %s\n' "$1" >&2; exit 1 ;;
    esac
done
[ "${#PROBES[@]}" -gt 0 ] || PROBES=(soak writeback signals)

if [ "$BUILD_KERNEL" -eq 1 ]; then
    printf '==> kernel build (once)\n'
    bash "$ROOT/scripts/run-kernel.sh" --lkl --build-only || exit 1
fi

pass=0; fail=0
for p in "${PROBES[@]}"; do
    cpio=""; markers=""; extra=""; timeout="$TIMEOUT"; driver=""
    case "$p" in
        soak) cpio="$ROOT/kernel/ldk/build/soak.cpio"
            markers="SOAK_SOCKETPAIR_OK SOAK_SCM_RIGHTS_OK SOAK_EPOLL_OK SOAK_EVENTFD_OK SOAK_MEMFD_OK SOAK_POLL_OK" ;;
        writeback) cpio="$ROOT/kernel/ldk/build/writeback.cpio"
            markers="WB_MUNMAP_OK WB_MSYNC_OK WB_PRIVATE_OK WB_ANON_OK" ;;
        signals) cpio="$ROOT/kernel/ldk/build/signals.cpio"
            markers="SIG_HANDLER_OK SIG_INFO_OK SIG_CHLD_OK SIG_DISP_OK" ;;
        runtime) cpio="$ROOT/kernel/ldk/build/runtime.cpio"
            markers="DYNAMIC_LIBC_OK PRIVATE_FILE_MMAP_OK FIXED_MAPPING_OK PTHREAD_TLS_JOIN_OK" ;;
        drm) cpio="$ROOT/kernel/ldk/build/drm.cpio"
            markers="DEVTMPFS_OK DLOPEN_OK DLSYM_OK DRMPROBE_OK"
            driver="DRM_DRIVER virtio_gpu"; extra="--gpu"; timeout=150 ;;
    esac
    if [ "$BUILD_PROBES" -eq 1 ]; then
        bash "$ROOT/scripts/build-probe.sh" "$p" || { printf 'FAIL %-9s probe build failed\n' "$p"; fail=$((fail+1)); continue; }
    elif [ ! -f "$cpio" ]; then
        printf 'FAIL %-9s missing %s (drop --no-build-probes)\n' "$p" "$cpio"
        fail=$((fail+1)); continue
    fi
    # $extra splits on purpose: empty, or --gpu.
    # shellcheck disable=SC2086
    out=$(bash "$ROOT/scripts/run-kernel.sh" --lkl --no-build --initrd "$cpio" $extra --timeout "$timeout" 2>&1)
    missing=""
    # $markers splits on purpose: one token per marker.
    # shellcheck disable=SC2086
    for m in $markers; do
        n=$(grep -c -F -- "$m" <<<"$out" || true)
        [ "$n" -eq 1 ] || missing="$missing $m(x$n)"
    done
    if [ -n "$driver" ]; then
        n=$(grep -c -F -- "$driver" <<<"$out" || true)
        [ "$n" -ge 1 ] || missing="$missing [$driver]"
    fi
    case "$out" in
        *"nk: done."*) ;;
        *) missing="$missing [nk: done.]" ;;
    esac
    case "$out" in
        *"!! kernel panic"*|*"!!EXC"*) missing="$missing [fault]" ;;
    esac
    if [ -z "$missing" ]; then
        printf 'PASS %-9s\n' "$p"
        pass=$((pass+1))
    else
        printf 'FAIL %s:%s\n' "$p" "$missing"
        fail=$((fail+1))
    fi
done
printf '%d/%d probes passed\n' "$pass" "$((pass+fail))"
[ "$fail" -eq 0 ]
