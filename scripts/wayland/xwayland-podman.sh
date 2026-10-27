#!/bin/sh
# Off-device stand-in for the chroot's Xwayland (MOON_XWAYLAND): Alpine
# 3.24's Xwayland in a container, given Moon's WAYLAND_SOCKET and -displayfd
# descriptors (3 and 4).
#
# The container shares the host's /tmp/.X11-unix (Moon connects through the
# socket file) but not its /tmp lock files, so Xwayland must never pick its
# own display: on a desktop host it would take :0 and replace the desktop
# server's socket file. It gets the first display from 60 up that nothing
# on the host uses (socket file, lock file or abstract socket), or the one
# Moon pins if that one is free.
set -eu
in_use() {
    [ -e "/tmp/.X11-unix/X$1" ] || [ -e "/tmp/.X$1-lock" ] ||
        grep -q "@/tmp/.X11-unix/X$1\$" /proc/net/unix
}
case "${1:-}" in
:*)
    # Moon pinned one (MOON_XWAYLAND_DISPLAY): only if nothing uses it.
    display="${1#:}"
    shift
    if in_use "$display"; then
        echo "xwayland-podman: display :$display is in use on this host" >&2
        exit 1
    fi
    ;;
*)
    display=60
    while in_use "$display"; do
        display=$((display + 1))
        [ "$display" -lt 200 ] || { echo "no free X display" >&2; exit 1; }
    done
    ;;
esac
exec podman run --rm -i --preserve-fds=2 --userns=keep-id --platform linux/amd64 \
    -e WAYLAND_SOCKET=3 -e WAYLAND_DEBUG="${WAYLAND_DEBUG:-}" -v /tmp/.X11-unix:/tmp/.X11-unix \
    localhost/moon-xwl:amd64 Xwayland ":$display" "$@"
