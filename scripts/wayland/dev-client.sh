#!/bin/sh
# Runs a stock Wayland client (foot, gtk4-demo, weston-simple-*) from an
# Alpine container against Moon's Wayland socket, for dev-run.sh:
#
#   MOON_DEV_CLIENT=foot A26_SYSTEM_APP="$PWD/scripts/wayland/dev-client.sh" \
#       scripts/wayland/dev-run.sh gpu start
#
# (A26_SYSTEM_APP is one executable, so the client comes from
# MOON_DEV_CLIENT, a command line, when no arguments are given.)
# Moon passes WAYLAND_DISPLAY (an absolute path). The image is built once:
#   podman build -t localhost/moon-wl-clients - <<<'FROM docker.io/library/alpine:3.24
#   RUN apk add --no-cache foot font-dejavu gtk4.0-demo weston-clients wayland-utils mesa-dri-gallium'
set -eu
dir="$(dirname "$WAYLAND_DISPLAY")"
exec podman run --rm --network=none -v "$dir:$dir" -e WAYLAND_DISPLAY="$WAYLAND_DISPLAY" \
    -e XDG_RUNTIME_DIR=/tmp -e GDK_BACKEND=wayland -e LIBGL_ALWAYS_SOFTWARE=1 \
    localhost/moon-wl-clients ${*:-$MOON_DEV_CLIENT}
