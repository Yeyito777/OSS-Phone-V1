#!/system/bin/sh
# Experiment only: run a command inside the separate Wayland rootfs
# (/data/local/a26-wayland), with the same chroot entry Moon uses.
#
#   su -c 'sh /data/local/tmp/a26-wayland/a26-wayland-run.sh CMD ARGS...'
#
# The environment has XDG_RUNTIME_DIR=/run/wl and LIBSEAT_BACKEND=noop, so a
# compositor opens /dev/dri/card0 and /dev/input/* directly as root (there is
# no logind, seatd or VT on this phone).
set -eu
WL_ROOT=/data/local/a26-wayland
sh /data/local/tmp/a26-wayland/a26-wayland-check.sh >&2 || { echo "refusing to run: display stack not handed off" >&2; exit 1; }
mkdir -p "$WL_ROOT/run" "$WL_ROOT/tmp"
# The same CPU floor the supervisor gives Xorg (uclamp.min 60), so the numbers
# compare with Moon's on Xorg.
[ ! -w /dev/cpuctl/moon-display/tasks ] || echo $$ >/dev/cpuctl/moon-display/tasks
exec env A26_ROOT="$WL_ROOT" A26_BUSYBOX=/data/local/a26-linux/busybox.static \
    /system/bin/sh /data/local/tmp/a26-wayland/a26-enter-chroot.sh \
    /bin/sh -c 'mkdir -p -m 0700 /run/wl; export XDG_RUNTIME_DIR=/run/wl LIBSEAT_BACKEND=noop; exec "$@"' sh "$@"
