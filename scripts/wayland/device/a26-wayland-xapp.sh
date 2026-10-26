#!/system/bin/sh
# Experiment only: while exp-compositor.sh holds weston
# (a26-wayland-run.sh env HOLD_SECONDS=50 exp-compositor.sh weston 3 0.5),
# run one of Moon's real X11 apps from Moon's own chroot against weston's
# Xwayland.
#
#   su -c 'sh /data/local/tmp/a26-wayland/a26-wayland-xapp.sh DISPLAY_NUM SECONDS [system|browser]'
#
# Each Magisk su session has its own mount namespace, so the Wayland rootfs'
# /tmp (with weston's X socket) exists only in weston's. This enters that
# namespace and bind-mounts the socket directory into Moon's chroot /tmp
# there (weston 14 listens on the filesystem socket only).
set -u
d="${1:?X display number}"
secs="${2:-15}"
app="${3:-system}"
ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
case "$app" in
    system) bin=/opt/a26-system/bin/a26-system ;;
    browser) bin=/opt/vimbrowser-a26/bin/vimbrowser-a26 ;;
    *) echo "unknown app $app" >&2; exit 2 ;;
esac
weston="$(pidof weston | cut -d' ' -f1)"
[ -n "$weston" ] || { echo "weston is not running" >&2; exit 1; }
out=/data/local/tmp/a26-wayland/xapp-$app.log
/system/bin/toybox nsenter -t "$weston" -m -- /system/bin/sh -c "
    mkdir -p $ROOT/tmp/.X11-unix
    grep -q ' $ROOT/tmp/.X11-unix ' /proc/mounts ||
        $BB mount -o bind /data/local/a26-wayland/tmp/.X11-unix $ROOT/tmp/.X11-unix
    $BB timeout -s KILL $secs $BB chroot $ROOT /usr/bin/env -i HOME=/root \
        PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
        DISPLAY=:$d $bin >$out 2>&1
    echo \"$app exited rc=\$? (137 = still running when the ${secs}s limit killed it)\"
"
tail -n 15 "$out"
