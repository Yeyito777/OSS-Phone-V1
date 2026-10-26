#!/system/bin/sh
# Experiment only: take the display away from a running Moon/Xorg session so a
# Wayland compositor (or kmsbench) can own KMS, without letting any of Moon's
# fallbacks hand the phone to Android meanwhile.
#
#   su -c 'sh /data/local/tmp/a26-wayland/a26-wayland-handoff.sh'
#
# Nothing on /data/adb or the boot path changes. The supervisor is only
# SIGSTOPped (so its liveness loop cannot see Xorg gone and restore Android);
# the Xorg takeover script and session wrapper are SIGKILLed (so their EXIT
# traps, which restore Android, never run). The way back is `adb reboot`,
# which boots into Moon as every direct boot does (a26-wayland-restore.sh).
set -u

ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
LOG=/data/local/tmp/a26-wayland/handoff.log
mkdir -p /data/local/tmp/a26-wayland
exec >>"$LOG" 2>&1
echo "handoff begin $(date) up=$(cut -d' ' -f1 /proc/uptime)"

fail() { echo "refusing: $*"; echo "refusing: $*" >&2; exit 1; }

[ "$(getprop moon.boot.mode)" = moon ] || fail "moon.boot.mode is not moon"
[ "$(getprop moon.session.ready)" = 1 ] || fail "Moon session is not ready"
xorg="$(pidof Xorg 2>/dev/null | cut -d' ' -f1)"
[ -n "$xorg" ] || fail "Xorg is not running"
sup="$(getprop moon.supervisor.pid)"
case "$sup" in ''|*[!0-9]*) fail "no supervisor pid" ;; esac
[ -d "/proc/$sup" ] || fail "supervisor $sup is gone"

# Wake the panel through Moon first: Moon's screen-off is DPMS off, and the
# compositor should start from an active CRTC with the backlight on.
"$BB" timeout -s KILL 5 "$BB" chroot "$ROOT" /opt/a26-shell/bin/a26-shellctl screen on >/dev/null 2>&1 || true
sleep 1
brightness="$(cat /sys/class/backlight/panel/brightness 2>/dev/null || echo 0)"
echo "brightness after wake: $brightness"

# 1. Freeze the supervisor (no file markers that could outlive a reboot).
kill -STOP "$sup" || fail "cannot stop supervisor"
echo "supervisor $sup stopped"

# 2. Kill, without traps, every shell in the Xorg takeover chain.
args_of() { "$BB" timeout -s KILL 2 cat "/proc/$1/cmdline" 2>/dev/null | tr '\000' ' '; }
for p in /proc/[0-9]*; do
    pid="${p#/proc/}"
    case "$(args_of "$pid")" in
        *a26-xorg-takeover.sh*|*a26-xorg-persistent-session*|*a26-xorg-safety-watchdog.sh*)
            echo "SIGKILL $pid: $(args_of "$pid")"
            kill -KILL "$pid" 2>/dev/null ;;
        *a26-enter-chroot.sh*a26-xorg-persistent-session*)
            echo "SIGKILL $pid: $(args_of "$pid")"
            kill -KILL "$pid" 2>/dev/null ;;
    esac
done

# 3. Stop Moon's session group (Moon and its apps), as the supervisor does.
session="$(cat /data/local/tmp/moon-boot/moon-session.pid 2>/dev/null || true)"
case "$session" in
    ''|*[!0-9]*) echo "no Moon session pid" ;;
    *)
        kill -TERM "-$session" 2>/dev/null
        for _ in $(seq 1 25); do kill -0 "-$session" 2>/dev/null || break; sleep 0.2; done
        kill -KILL "-$session" 2>/dev/null || true
        echo "Moon session $session stopped" ;;
esac

# 4. Stop Xorg and wait until nothing holds the DRM card.
kill -TERM "$xorg" 2>/dev/null
for _ in $(seq 1 25); do [ -d "/proc/$xorg" ] || break; sleep 0.2; done
[ -d "/proc/$xorg" ] && kill -KILL "$xorg" 2>/dev/null
for _ in $(seq 1 25); do
    /system/bin/toybox lsof /dev/dri/card0 2>/dev/null | grep -q /dev/dri/card0 || break
    sleep 0.2
done
if /system/bin/toybox lsof /dev/dri/card0 2>/dev/null | grep -q /dev/dri/card0; then
    /system/bin/toybox lsof /dev/dri/card0
    fail "DRM card still open"
fi
[ "$brightness" -gt 0 ] 2>/dev/null || echo 128 >/sys/class/backlight/panel/brightness
echo "handoff done: Xorg and Moon stopped, card0 free, supervisor frozen. Restore with adb reboot."
echo ok >&2
