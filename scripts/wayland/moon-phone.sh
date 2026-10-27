#!/bin/sh
# Host-side driver for milestone 1 on the phone (run from the Mac with the
# phone on adb). Nothing here touches /data/adb or the boot image.
#
#   scripts/wayland/moon-phone.sh install       back up the phone's Moon, install this
#                                               branch's Moon, the demo app and the System
#                                               app's Wayland build (beside the installed one)
#   scripts/wayland/moon-phone.sh start [demo|system] [evdev] [debug] [box] [dmabuf] [sync-rw|sync-none]
#                                               restart Moon with Wayland on (the demo, or
#                                               the Wayland System build, in the System slot)
#   scripts/wayland/moon-phone.sh stop          restart Moon with Wayland off
#   scripts/wayland/moon-phone.sh restore       put the backed-up Moon back, restart it
#   scripts/wayland/moon-phone.sh xwayland-install
#                                               add Alpine's Xwayland to the chroot (three
#                                               pinned packages, offline; Xorg is untouched)
#   scripts/wayland/moon-phone.sh xwayland-start [debug]
#                                               milestone 4: Moon on Xwayland instead of the
#                                               GPU session's headless Xorg (needs install)
#   scripts/wayland/moon-phone.sh reboot        the way back from xwayland-start
#   scripts/wayland/moon-phone.sh heapbench [ROUNDS]
#                                               dma-heap vs memfd paint, read and cache
#                                               flush costs (src/moon-wl-demo/src/bin)
#   scripts/wayland/moon-phone.sh ctl ARGS...   a26-shellctl on the phone
#   scripts/wayland/moon-phone.sh log [N]       the last N lines of Moon's log
#
# Binaries are cross-built on whale (WHALE_TREE, default ~/moon-wl there, and
# WHALE_SYSTEM, default ~/Workspace/research/a26-system-wayland):
#   cargo build --release --target aarch64-unknown-linux-musl
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
SERIAL="${A26_SERIAL:-R5CY323ES7X}"
WHALE_TREE="${WHALE_TREE:-moon-wl}"
WHALE_SYSTEM="${WHALE_SYSTEM:-Workspace/research/a26-system-wayland}"
CACHE="$HERE/../../build/wayland"
ROOT=/data/local/a26-linux
BIN="$ROOT/opt/a26-shell/bin"
DEV=/data/local/tmp/a26-wayland
adb() { command adb -s "$SERIAL" "$@"; }
su_sh() { adb shell "su -c '$*'"; }

push_root() { # LOCAL REMOTE MODE
    adb push -q "$1" /data/local/tmp/moon-wl-push >/dev/null
    su_sh "cp /data/local/tmp/moon-wl-push $2.new && chown 0:0 $2.new && chmod $3 $2.new && mv -f $2.new $2 && rm -f /data/local/tmp/moon-wl-push"
}

case "${1:-}" in
install)
    mkdir -p "$CACHE"
    target=aarch64-unknown-linux-musl/release
    scp -q "whale:$WHALE_TREE/src/a26-shell/target/$target/a26-shell" "$CACHE/a26-shell"
    scp -q "whale:$WHALE_TREE/src/moon-wl-demo/target/$target/moon-wl-demo" "$CACHE/moon-wl-demo"
    scp -q "whale:$WHALE_TREE/src/moon-wl-demo/target/$target/heapbench" "$CACHE/heapbench"
    scp -q "whale:$WHALE_SYSTEM/target/$target/a26-system" "$CACHE/a26-system-wayland"
    # One backup: the Moon that was installed before the first install.
    su_sh "[ -e $BIN/a26-shell.before-wayland ] || cp -p $BIN/a26-shell $BIN/a26-shell.before-wayland"
    push_root "$CACHE/a26-shell" "$BIN/a26-shell" 0755
    su_sh "mkdir -p $ROOT/opt/moon-wl-demo/bin $DEV"
    push_root "$CACHE/moon-wl-demo" "$ROOT/opt/moon-wl-demo/bin/moon-wl-demo" 0755
    push_root "$CACHE/a26-system-wayland" "$ROOT/opt/a26-system/bin/a26-system-wayland" 0755
    push_root "$HERE/device/moon-wayland-test.sh" "$DEV/moon-wayland-test.sh" 0755
    push_root "$CACHE/heapbench" "$DEV/heapbench" 0755
    su_sh "sha256sum $BIN/a26-shell $BIN/a26-shell.before-wayland $ROOT/opt/moon-wl-demo/bin/moon-wl-demo"
    echo "installed; the running Moon is unchanged until start/stop or a reboot"
    ;;
start) shift; su_sh "sh $DEV/moon-wayland-test.sh start $*" ;;
stop) su_sh "sh $DEV/moon-wayland-test.sh stop" ;;
restore)
    su_sh "[ -e $BIN/a26-shell.before-wayland ] && cp -p $BIN/a26-shell.before-wayland $BIN/a26-shell.new && mv -f $BIN/a26-shell.new $BIN/a26-shell && rm -f $BIN/a26-shell.before-wayland && sha256sum $BIN/a26-shell"
    su_sh "rm -f $ROOT/opt/a26-system/bin/a26-system-wayland"
    su_sh "sh $DEV/moon-wayland-test.sh stop"
    ;;
xwayland-install)
    # Alpine v3.24 (the chroot's release); apk verifies them against the
    # chroot's signing keys. Nothing else is added or upgraded (checked
    # against the phone's package database: every other dependency is
    # already installed).
    mkdir -p "$CACHE"
    apks="community/aarch64/xwayland-24.1.14-r0.apk main/aarch64/libtirpc-nokrb-1.3.5-r1.apk
          main/aarch64/libtirpc-conf-1.3.5-r1.apk"
    names=''
    for apk in $apks; do
        name="${apk##*/}"
        [ -s "$CACHE/$name" ] || curl -fsSL -o "$CACHE/$name" "https://dl-cdn.alpinelinux.org/alpine/v3.24/$apk"
        adb push -q "$CACHE/$name" "/data/local/tmp/$name" >/dev/null
        su_sh "mv -f /data/local/tmp/$name $ROOT/root/$name"
        names="$names /root/$name"
    done
    su_sh "A26_ROOT=$ROOT A26_BUSYBOX=$ROOT/busybox.static sh $ROOT/a26-enter-chroot.sh /sbin/apk add --no-network --simulate$names"
    su_sh "A26_ROOT=$ROOT A26_BUSYBOX=$ROOT/busybox.static sh $ROOT/a26-enter-chroot.sh /sbin/apk add --no-network --quiet$names && rm -f$(echo "$names" | sed "s# /root/# $ROOT/root/#g") && ls -la $ROOT/usr/bin/Xwayland"
    ;;
xwayland-start)
    shift
    push_root "$HERE/device/moon-xwayland-test.sh" "$DEV/moon-xwayland-test.sh" 0755
    su_sh "sh $DEV/moon-xwayland-test.sh start $*"
    ;;
reboot)
    adb reboot
    echo "rebooting; waiting for Moon"
    for _ in $(seq 1 90); do
        sleep 2
        [ "$(adb shell getprop moon.session.ready 2>/dev/null | tr -d '\r')" = 1 ] && break
    done
    su_sh "echo mode=\$(getprop moon.boot.mode) ready=\$(getprop moon.session.ready) moon=\$(pidof a26-shell) xorg=\$(pidof Xorg) compositor=\$(pidof moon-compositor)"
    ;;
heapbench) su_sh "$DEV/heapbench ${2:-30}" ;;
ctl) shift; su_sh "$ROOT/busybox.static chroot $ROOT /opt/a26-shell/bin/a26-shellctl $*" ;;
log) su_sh "tail -n ${2:-40} $ROOT/root/a26-shell.log" ;;
*) sed -n '2,26p' "$0"; exit 2 ;;
esac
