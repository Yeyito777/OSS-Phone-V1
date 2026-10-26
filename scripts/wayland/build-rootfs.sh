#!/bin/sh
# Build the Wayland experiment rootfs on a Linux host with podman and
# qemu-aarch64 binfmt (whale): a separate Alpine aarch64 tree with weston,
# cage (wlroots), Xwayland and the bench tools. It is never installed into
# Moon's chroot, so Xorg's libraries are untouched.
#
#   scripts/wayland/build-rootfs.sh [OUTDIR]     (default ~/wayland-exp)
#
# Output: OUTDIR/a26-wayland-rootfs.tar.gz and OUTDIR/bin/{kmsbench,wlbench}.
# BENCH_ONLY=1 rebuilds just the two bench binaries.
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
OUT="${1:-$HOME/wayland-exp}"
ALPINE="${ALPINE:-docker.io/library/alpine:3.24}"
mkdir -p "$OUT/bench" "$OUT/bin"
cp "$HERE"/bench/*.c "$OUT/bench/"

PACKAGES="
    busybox musl alpine-baselayout alpine-keys apk-tools
    weston weston-backend-drm weston-shell-desktop weston-shell-fullscreen
    weston-xwayland weston-clients weston-terminal
    xwayland cage wlroots0.19 libliftoff
    libudev-zero libseat seatd libinput libinput-tools libdrm drm_info
    pixman wayland-utils xeyes xclock xdpyinfo xwininfo xev font-dejavu
    strace
"

podman run --rm --arch arm64 -v "$OUT:/w" "$ALPINE" sh -euc "
    apk add --no-cache build-base wayland-dev wayland-protocols libdrm-dev pixman-dev mesa-dev linux-headers >/dev/null
    cd /w/bench
    P=/usr/share/wayland-protocols
    for x in stable/xdg-shell/xdg-shell stable/presentation-time/presentation-time \
             stable/linux-dmabuf/linux-dmabuf-v1; do
        n=\${x##*/}
        wayland-scanner client-header \$P/\$x.xml \$n-client-protocol.h
        wayland-scanner private-code \$P/\$x.xml \$n-protocol.c
    done
    CFLAGS='-O2 -Wall -Wextra -Wno-unused-parameter'
    gcc \$CFLAGS -I/usr/include/libdrm -I/usr/include/pixman-1 -o /w/bin/kmsbench kmsbench.c \
        -ldrm -lgbm -lpixman-1 -lpthread -lm
    gcc \$CFLAGS -I. -I/usr/include/libdrm -I/usr/include/pixman-1 -o /w/bin/wlbench wlbench.c \
        xdg-shell-protocol.c presentation-time-protocol.c linux-dmabuf-v1-protocol.c \
        -lwayland-client -ldrm -lpixman-1 -lm
    [ \"${BENCH_ONLY:-0}\" = 1 ] && exit 0

    rm -rf /tmp/rootfs && mkdir -p /tmp/rootfs/etc/apk
    cp -r /etc/apk/keys /tmp/rootfs/etc/apk/
    cp /etc/apk/repositories /tmp/rootfs/etc/apk/
    apk add --root /tmp/rootfs --initdb --no-cache $(echo $PACKAGES) >/w/apk.log 2>&1 || { tail -30 /w/apk.log; exit 1; }
    install -m 0755 /w/bin/kmsbench /w/bin/wlbench /tmp/rootfs/usr/local/bin/
    apk --root /tmp/rootfs info -v 2>/dev/null | sort >/w/packages.txt
    tar -C /tmp/rootfs -czf /w/a26-wayland-rootfs.tar.gz .
"
ls -l "$OUT/a26-wayland-rootfs.tar.gz" "$OUT/bin"
