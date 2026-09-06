# The machine ldk compiles Linux drivers on.
#
# In a container because the Linux source tree cannot live on macOS at all:
# it contains filenames that differ only in case (xt_CONNMARK.h beside
# xt_connmark.h, among others), and a case-insensitive APFS volume silently
# loses one of every such pair on extraction. The same reason the x86 kernel
# build already keeps its tree in the `nethos-kernel-src` docker volume rather
# than on a bind mount -- see payload/bin/nethos-kernel.
#
# arm64 host, so the native compiler already targets aarch64 and no
# CROSS_COMPILE is needed. That is the whole reason the target is aarch64
# first; an x86 port would want crossbuild-essential-amd64 here.
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential flex bison bc libssl-dev libelf-dev \
        python3 make perl xz-utils cpio kmod rsync \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
