#!/bin/bash
# Build an under-500 MB graphical installer plus a separate offline system.
# Docker retains downloaded/converted packages in the named Linux builder.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
CONTAINER=nethos-installer-builder
REUSE=0
while [ $# -gt 0 ]; do
    case "$1" in
        --container) CONTAINER=${2:?}; shift 2 ;;
        --reuse-root) REUSE=1; shift ;;
        *) echo 'usage: build-installer-bundle.sh [--container NAME] [--reuse-root]' >&2; exit 2 ;;
    esac
done
if ! docker inspect "$CONTAINER" >/dev/null 2>&1; then
    docker run -d --name "$CONTAINER" --platform linux/amd64 --privileged \
        -v "$ROOT":/src -w /src debian:trixie sleep infinity
else
    docker start "$CONTAINER" >/dev/null
fi
docker exec --privileged -e NETHOS_REUSE_ROOT="$REUSE" "$CONTAINER" \
    bash /src/scripts/assemble-installer-bundle.sh
