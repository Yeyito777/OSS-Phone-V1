#!/bin/sh
# Device check for "Moon starts with the display pipeline on" (f9c0dce):
# Moon is restarted while the screen is asleep, as the supervisor's recovery
# does, and the panel must come back lit and flipping. Run from the Mac with
# the phone on adb, in Moon; uses moon-wayland-test.sh (moon-phone.sh
# install) for the restart, with the Moon installed at the time.
#
#   scripts/wayland/wake-test.sh
#
# Prints PASS or FAIL with the evidence: frames not presented after the
# restart, the compositor's "panel on", the backlight level, Moon's state.
set -eu
SERIAL="${A26_SERIAL:-R5CY323ES7X}"
ROOT=/data/local/a26-linux
DEV=/data/local/tmp/a26-wayland
adb() { command adb -s "$SERIAL" "$@"; }
su_sh() { adb shell "su -c '$*'"; }
ctl() { su_sh "$ROOT/busybox.static chroot $ROOT /opt/a26-shell/bin/a26-shellctl $*"; }
field() { ctl state | grep -oE "\"$1\":(\"[^\"]*\"|[^,}]*)" | head -1 | cut -d: -f2 | tr -d '"'; }

HERE="$(cd "$(dirname "$0")" && pwd)"
adb push -q "$HERE/device/moon-wayland-test.sh" /data/local/tmp/moon-wayland-test.sh >/dev/null
su_sh "mkdir -p $DEV && mv -f /data/local/tmp/moon-wayland-test.sh $DEV/moon-wayland-test.sh"
# GPU path: moon-compositor powers the panel ("panel on" in its log).
# Software path: Xorg's DPMS, the panel connector's dpms in sysfs.
gpu="$(su_sh "pidof moon-compositor" | tr -d '\r')"
dpms() { su_sh "cat /sys/class/drm/card0-*/dpms 2>/dev/null | head -1" | tr -d '\r'; }
if [ -n "$gpu" ]; then path="GPU (moon-compositor)"; else path="software (Xorg on the panel)"; fi
echo "Moon $(su_sh "sha256sum $ROOT/opt/a26-shell/bin/a26-shell" | cut -c1-12), path: $path"
# 1. Asleep: the screen-off transaction powers the compositor (or DPMS) off.
ctl screen on >/dev/null
sleep 1
ctl power >/dev/null
for _ in $(seq 1 20); do [ "$(field phase)" = screen_off ] && break; sleep 0.5; done
[ "$(field phase)" = screen_off ] || { echo "FAIL: the screen did not go to sleep"; exit 1; }
if [ -n "$gpu" ]; then
    echo "asleep: backlight $(su_sh cat /sys/class/backlight/panel/brightness), compositor: $(su_sh "tail -n 3 /data/local/tmp/moon-gpu/compositor.log" | grep -o 'panel o[nf]*:.*' | tail -1)"
else
    echo "asleep: backlight $(su_sh cat /sys/class/backlight/panel/brightness), dpms $(dpms)"
fi

# 2. Moon restarted while asleep (as the supervisor restarts it).
su_sh "sh $DEV/moon-wayland-test.sh stop" >/dev/null
sleep 4
log="$(su_sh "cat $ROOT/root/a26-shell.log")"
backlight="$(su_sh cat /sys/class/backlight/panel/brightness | tr -d '\r')"
echo "$log" | grep -E 'initial (display|panel|touchscreen) wake failed' || true
if [ -n "$gpu" ]; then
    # The first frames may go out while the panel powers up; what matters
    # is that frames drawn afterwards flip: an animation (the lock screen's
    # keypad sliding in) must add no "not presented".
    before="$(echo "$log" | grep -c 'not presented' || true)"
    ctl swipe-up >/dev/null
    sleep 2
    after="$(su_sh "cat $ROOT/root/a26-shell.log" | grep -c 'not presented' || true)"
    panel="$(su_sh "tail -n 5 /data/local/tmp/moon-gpu/compositor.log" | grep -o 'panel o[nf]*:.*' | tail -1)"
    echo "after the restart: $before frames not presented at startup, $((after - before)) during an animation, compositor: ${panel:-no power change}, backlight $backlight, awake $(field screen_awake)"
    lit=0
    [ "$after" = "$before" ] && echo "$panel" | grep -q '^panel on' && lit=1
else
    state="$(dpms)"
    echo "after the restart: dpms $state, backlight $backlight, awake $(field screen_awake)"
    lit=0
    [ "$state" = On ] && lit=1
fi
if [ "$lit" = 1 ] && [ "$backlight" -gt 0 ]; then
    echo "PASS"
else
    echo "FAIL"
    exit 1
fi
