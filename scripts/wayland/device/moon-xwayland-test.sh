#!/system/bin/sh
# Experiment only (milestone 4 on the phone): Moon on Xwayland instead of
# the headless Xorg of the GPU session, without touching /data/adb or the
# boot path.
#
#   su -c 'sh /data/local/tmp/a26-wayland/moon-xwayland-test.sh start [debug]'
#
# Needs the GPU session (moon-compositor on the panel, Xorg headless) and
# Xwayland in the chroot (moon-phone.sh xwayland-install). Like the earlier
# handoff experiment, the supervisor is SIGSTOPped (its liveness loop would
# see Xorg gone and restore Android) and the Xorg takeover chain SIGKILLed
# (its EXIT traps stop Wi-Fi and restore Android). moon-compositor keeps the
# panel. Moon's session and headless Xorg stop, and Moon starts again with
# MOON_XWAYLAND: it starts Xwayland on :0 itself and reads the touchscreen.
# The way back is `adb reboot`, which boots into Moon as every boot does.
set -u
ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
LOG=/data/local/tmp/a26-wayland/xwayland-test.log
mkdir -p /data/local/tmp/a26-wayland

fail() { echo "refusing: $*" >&2; exit 1; }

moon_ping() {
    "$BB" timeout -s KILL 2 "$BB" chroot "$ROOT" /opt/a26-shell/bin/a26-shellctl ping >/dev/null 2>&1
}

case "${1:-}" in
start)
    shift
    extra=''
    for option in "$@"; do
        case "$option" in
            debug) extra="$extra MOON_WAYLAND_DEBUG=1" ;;
            *) fail "unknown option $option" ;;
        esac
    done
    [ "$(getprop moon.boot.mode)" = moon ] || fail "moon.boot.mode is not moon"
    [ "$(getprop moon.session.ready)" = 1 ] || fail "Moon session is not ready"
    [ -n "$(pidof moon-compositor 2>/dev/null)" ] || fail "no GPU session (moon-compositor)"
    [ -x "$ROOT/usr/bin/Xwayland" ] || fail "Xwayland is not in the chroot"
    xorg="$(pidof Xorg 2>/dev/null | cut -d' ' -f1)"
    [ -n "$xorg" ] || fail "Xorg is not running"
    sup="$(getprop moon.supervisor.pid)"
    case "$sup" in ''|*[!0-9]*) fail "no supervisor pid" ;; esac
    [ -d "/proc/$sup" ] || fail "supervisor $sup is gone"
    exec 3>&2 >>"$LOG" 2>&1
    echo "xwayland test begin $(date) up=$(cut -d' ' -f1 /proc/uptime)"

    # 1. Freeze the supervisor; 2. kill the takeover chain without traps.
    kill -STOP "$sup" || fail "cannot stop supervisor"
    echo "supervisor $sup stopped"
    args_of() { "$BB" timeout -s KILL 2 cat "/proc/$1/cmdline" 2>/dev/null | tr '\000' ' '; }
    for p in /proc/[0-9]*; do
        pid="${p#/proc/}"
        case "$(args_of "$pid")" in
            *a26-xorg-takeover.sh*|*a26-xorg-persistent-session*|*a26-xorg-safety-watchdog.sh*)
                echo "SIGKILL $pid: $(args_of "$pid")"
                kill -KILL "$pid" 2>/dev/null ;;
        esac
    done

    # 3. Moon's session group, as the supervisor stops it.
    session="$(cat /data/local/tmp/moon-boot/moon-session.pid 2>/dev/null || true)"
    case "$session" in
        ''|*[!0-9]*) echo "no Moon session pid" ;;
        *)
            kill -TERM "-$session" 2>/dev/null
            for _ in $(seq 1 25); do kill -0 "-$session" 2>/dev/null || break; sleep 0.2; done
            kill -KILL "-$session" 2>/dev/null || true
            echo "Moon session $session stopped" ;;
    esac

    # 4. Headless Xorg (moon-compositor keeps the panel).
    kill -TERM "$xorg" 2>/dev/null
    for _ in $(seq 1 25); do [ -d "/proc/$xorg" ] || break; sleep 0.2; done
    [ -d "/proc/$xorg" ] && kill -KILL "$xorg" 2>/dev/null
    echo "Xorg $xorg stopped"
    rm -f "$ROOT/tmp/.X0-lock" "$ROOT/tmp/.X11-unix/X0"

    # 5. Moon on Xwayland, through the same chroot wrapper the supervisor uses.
    : >"$ROOT/root/a26-shell.log"
    "$BB" nohup "$BB" setsid /system/bin/sh "$ROOT/a26-enter-chroot.sh" \
        /usr/bin/env DISPLAY=:0 A26_SHELL_CONFIG=/etc/a26-shell/config.json \
        MOON_GPU_SOCKET=/run/moon-gpu/compositor.sock MOON_WAYLAND_DIR=/run/moon-wayland \
        MOON_XWAYLAND=/usr/bin/Xwayland MOON_XWAYLAND_DISPLAY=:0 $extra \
        /opt/a26-shell/bin/a26-shell \
        >>"$ROOT/root/a26-shell.log" 2>&1 </dev/null &
    echo "$!" >/data/local/tmp/moon-boot/moon-session.pid
    for _ in $(seq 1 60); do moon_ping && break; sleep 0.5; done
    if moon_ping; then
        echo "Moon answers on Xwayland (session $(cat /data/local/tmp/moon-boot/moon-session.pid))" | tee /proc/self/fd/3
    else
        echo "Moon did not answer within 30 s; restore with adb reboot" | tee /proc/self/fd/3
        exit 1
    fi
    grep -E '^(xwayland|touch|gpu: compositor|compositor):' "$ROOT/root/a26-shell.log" >&3
    ;;
*)
    echo "usage: $0 start [debug]   (the way back is adb reboot)" >&2
    exit 2
    ;;
esac
