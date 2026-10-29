#!/bin/sh
# Host-side driver for the Wayland experiment (run from the Mac with the phone
# on adb). Nothing here touches /data/adb, the boot image or Moon's chroot.
#
#   scripts/wayland/phone.sh install [TARBALL]  push the rootfs to /data/local/a26-wayland
#   scripts/wayland/phone.sh scripts            push only the scripts and bench binaries
#   scripts/wayland/phone.sh handoff            stop Moon/Xorg, freeze the supervisor
#   scripts/wayland/phone.sh check              handed-off state (no Xorg/SF/HWC/zygote, moon-session alive)
#   scripts/wayland/phone.sh run CMD...         run CMD in the Wayland rootfs (after check)
#   scripts/wayland/phone.sh restore            adb reboot and wait until Moon is ready
#   scripts/wayland/phone.sh status             Moon/Xorg/CRTC state
#
# The tarball comes from build-rootfs.sh (default: fetched from whale).
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SERIAL="${A26_SERIAL:-R5CY323ES7X}"
WHALE_OUT="${WHALE_OUT:-whale:wayland-exp}"
CACHE="$REPO/build/wayland"
DEV=/data/local/tmp/a26-wayland
WL_ROOT=/data/local/a26-wayland
adb() { command adb -s "$SERIAL" "$@"; }
su_sh() { adb shell "su -c '$*'"; }

push_scripts() {
    mkdir -p "$CACHE"
    scp -q "$WHALE_OUT/bin/kmsbench" "$WHALE_OUT/bin/wlbench" "$CACHE/"
    adb shell "su -c 'mkdir -p $DEV'"
    for f in "$HERE"/device/*.sh "$REPO/scripts/device/a26-enter-chroot.sh"; do
        adb push -q "$f" /data/local/tmp/ >/dev/null
        su_sh "mv -f /data/local/tmp/$(basename "$f") $DEV/ && chmod 0755 $DEV/$(basename "$f")"
    done
    for f in "$HERE"/rootfs/*.sh "$CACHE/kmsbench" "$CACHE/wlbench"; do
        b="$(basename "$f")"
        adb push -q "$f" "/data/local/tmp/$b" >/dev/null
        su_sh "mv -f /data/local/tmp/$b $WL_ROOT/usr/local/bin/$b && chmod 0755 $WL_ROOT/usr/local/bin/$b"
    done
    [ ! -f "$HERE/rootfs/weston.ini" ] || {
        adb push -q "$HERE/rootfs/weston.ini" /data/local/tmp/weston.ini >/dev/null
        su_sh "mkdir -p $WL_ROOT/etc/xdg/weston && mv -f /data/local/tmp/weston.ini $WL_ROOT/etc/xdg/weston/weston.ini"
    }
}

case "${1:-}" in
install)
    tarball=''
    force=0
    for argument in "${@:2}"; do
        if [ "$argument" = --force ]; then force=1; else tarball="$argument"; fi
    done
    if [ -z "$tarball" ]; then
        mkdir -p "$CACHE"
        scp -q "$WHALE_OUT/a26-wayland-rootfs.tar.gz" "$CACHE/"
        tarball="$CACHE/a26-wayland-rootfs.tar.gz"
    fi
    # rm -rf through live bind mounts (/dev, /proc, /sys, /data) deletes
    # what they point at: stop unless the owner passes --force.
    if su_sh "grep -q \" $WL_ROOT/\" /proc/mounts"; then
        su_sh "grep \" $WL_ROOT/\" /proc/mounts" >&2
        if [ "$force" = 1 ]; then
            echo "WARNING: replacing $WL_ROOT with mounts inside it (--force)" >&2
        else
            echo "refusing: something is still mounted under $WL_ROOT (override: --force; rm -rf would reach through the mounts)" >&2
            exit 1
        fi
    fi
    adb push "$tarball" /data/local/tmp/a26-wayland-rootfs.tar.gz
    su_sh "rm -rf $WL_ROOT.new && mkdir -p $WL_ROOT.new && /data/local/a26-linux/busybox.static tar -xzf /data/local/tmp/a26-wayland-rootfs.tar.gz -C $WL_ROOT.new && rm -rf $WL_ROOT && mv $WL_ROOT.new $WL_ROOT && rm -f /data/local/tmp/a26-wayland-rootfs.tar.gz && du -sh $WL_ROOT"
    push_scripts
    ;;
scripts) push_scripts ;;
handoff)
    su_sh "sh $DEV/a26-wayland-handoff.sh"
    su_sh "tail -n 30 $DEV/handoff.log"
    ;;
check) su_sh "sh $DEV/a26-wayland-check.sh" ;;
run)
    shift
    su_sh "sh $DEV/a26-wayland-run.sh $*"
    ;;
restore)
    adb reboot || true
    echo "rebooting; waiting for Moon"
    sleep 20
    adb wait-for-device
    for _ in $(seq 1 120); do
        [ "$(adb shell getprop moon.session.ready 2>/dev/null | tr -d '\r')" = 1 ] && break
        sleep 2
    done
    "$0" status
    ;;
status)
    su_sh "echo mode=\$(getprop moon.boot.mode) ready=\$(getprop moon.session.ready) xorg=\$(pidof Xorg) moon=\$(pidof a26-shell) uptime=\$(cut -d\" \" -f1 /proc/uptime)"
    ;;
*)
    sed -n '2,14p' "$0"
    exit 2
    ;;
esac
