#!/bin/bash
# Run the nk test suite with each class in its own process.
#
# Every class boots QEMU from scratch in setUpClass, so the suite is a dozen
# independent boots that unittest runs one after another -- six and a half
# minutes of a machine that is idle for most of it. Run in parallel it is
# under two, and a failure names its class instead of being one line in a
# very long log.
#
#   tests/run-kernel-tests.sh            all of them
#   tests/run-kernel-tests.sh Initrd     just the classes matching a pattern
#
# Concurrency is deliberately not the core count: each job is a QEMU with a
# gigabyte of guest memory, and oversubscribing them makes the timeouts the
# thing under test.
set -uo pipefail

cd "$(dirname "$0")"
JOBS="${JOBS:-5}"
PATTERN="${1:-.}"
RESULTS="$(mktemp -d)"
trap '[ -n "${KEEP:-}" ] || rm -rf "$RESULTS"' EXIT

CLASSES=$(
    { sed -n 's/^class \([A-Za-z0-9_]*\).*/test_kernel_boot.\1/p' test_kernel_boot.py
      sed -n 's/^class \([A-Za-z0-9_]*\).*/test_kernel_elf.\1/p' test_kernel_elf.py
      sed -n 's/^class \([A-Za-z0-9_]*\).*/test_kernel_runtime.\1/p' test_kernel_runtime.py
    } | grep -- "$PATTERN"
)
[ -n "$CLASSES" ] || { echo "no test classes match $PATTERN"; exit 1; }

# The class name is passed as an argument rather than substituted into the
# script text: xargs builds one command line per job, and a script long
# enough to be useful plus a substituted name exceeds what it will assemble.
export RESULTS
# Build every variant first, one at a time.
#
# The classes run in parallel and each starts by building the kernel it needs.
# Left to themselves they take the same cargo lock at the same moment, and a
# class whose watchdog is twelve seconds can spend all twelve waiting -- a
# failure with nothing to do with the kernel. Serially here, they all find
# their build current and go straight to booting.
warm() {
    printf "  building %-18s" "$1"
    shift
    if bash ../scripts/run-kernel.sh --build-only "$@" >/dev/null 2>&1; then
        echo ok
    else
        echo "FAILED -- $*"
        exit 1
    fi
}
HELLO="../kernel/ldk/build/nk-hello"
warm plain
warm lkl --lkl
warm "port virtio-blk" --port virtio-blk
warm "port virtio-net" --port virtio-net
[ -f "$HELLO" ] && warm "lkl+init" --lkl --init "$HELLO"
echo

# The classes may now boot what is there rather than each asking cargo.
export NK_TESTS_PREBUILT=1

echo "$CLASSES" | xargs -P "$JOBS" -I{} sh -c '
    s=$(date +%s)
    python3 -m unittest "$1" > "$RESULTS/$1" 2>&1
    printf "%-40s %4ss  %s\n" "$1" "$(( $(date +%s) - s ))" "$(tail -1 "$RESULTS/$1")"
' _ {}

ran=$(ls "$RESULTS" | wc -l | tr -d ' ')
want=$(echo "$CLASSES" | wc -l | tr -d ' ')
if [ "$ran" != "$want" ]; then
    echo "only $ran of $want classes produced a result"
    exit 1
fi

failed=$(grep -l "^FAILED\|^ERROR" "$RESULTS"/* 2>/dev/null || true)
if [ -n "$failed" ]; then
    echo
    for f in $failed; do
        echo "=== $(basename "$f")"
        [ -n "${KEEP:-}" ] && echo "    full output: $f"
        # Names and assertions only, truncated: these tests match against a
        # whole boot log, so an unabridged failure is pages of serial output
        # per assertion and the useful line is the first one.
        grep -E "^(FAIL|ERROR):|^AssertionError|^[A-Za-z]*Error:" "$f" | cut -c1-160
    done
    exit 1
fi
echo
echo "all classes passed"
