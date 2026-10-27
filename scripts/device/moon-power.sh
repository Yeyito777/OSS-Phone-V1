#!/system/bin/sh
# Clean power-off or reboot from Moon mode (docs/power.md):
#   moon-power.sh poweroff|reboot
# Runs in Android's root namespace (Moon's chroot helper starts it detached
# through /proc/1/root). Stops Moon's services in order, syncs, then lets
# Android init shut down normally, so the next boot is a normal boot.
set -u

ACTION="${1:-}"
case "$ACTION" in
    poweroff) POWERCTL=shutdown; FALLBACK='-p' ;;
    reboot) POWERCTL=reboot; FALLBACK='' ;;
    *) echo "usage: $0 poweroff|reboot" >&2; exit 2 ;;
esac

ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
PERSIST=/data/adb/moon
RUNTIME=/data/local/tmp/moon-boot
LOG="$PERSIST/power.log"
CHROOT_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

mkdir -p "$PERSIST" "$RUNTIME"
exec </dev/null >>"$LOG" 2>&1

log() {
    echo "moon-power: $* [$(date --iso-8601=seconds 2>/dev/null || date) up=$(cut -d' ' -f1 /proc/uptime)]"
}

# Once per boot: a second request while one runs is ignored. /data/local/tmp
# outlives a reboot, so the lock records the boot it belongs to.
boot_id="$(cat /proc/sys/kernel/random/boot_id 2>/dev/null)"
LOCK="$RUNTIME/power.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
    if [ "$(cat "$LOCK/boot-id" 2>/dev/null)" = "$boot_id" ]; then
        log "$ACTION ignored: already in progress"
        exit 0
    fi
    rm -rf "$LOCK"
    mkdir "$LOCK" || exit 1
fi
echo "$boot_id" >"$LOCK/boot-id"
log "$ACTION requested (battery $(cat /sys/class/power_supply/battery/capacity 2>/dev/null)%)"

in_chroot() {
    limit=$1
    shift
    "$BB" timeout -s KILL "$limit" "$BB" chroot "$ROOT" /usr/bin/env -i HOME=/root \
        PATH="$CHROOT_PATH" "$@"
}

# TERM the processes running `exe` (a path as seen from here), KILL what is
# left after `wait` seconds.
stop_exe() {
    exe=$1
    wait=$2
    pids=''
    for pid in $(pidof "${exe##*/}" 2>/dev/null); do
        # A binary replaced since the process started reads "... (deleted)".
        case "$(readlink "/proc/$pid/exe" 2>/dev/null)" in
            "$exe"|"$exe (deleted)") pids="$pids $pid" ;;
        esac
    done
    [ -n "$pids" ] || return 0
    kill -TERM $pids 2>/dev/null
    for _ in $("$BB" seq 1 $((wait * 5))); do
        alive=''
        for pid in $pids; do [ -e "/proc/$pid" ] && alive="$alive $pid"; done
        [ -n "$alive" ] || { log "stopped ${exe##*/}"; return 0; }
        sleep 0.2
    done
    kill -KILL $alive 2>/dev/null
    log "killed ${exe##*/}:$alive"
}

# 1. Nothing restarts or falls back from here on.
setprop moon.boot.mode shutdown
supervisor="$(getprop moon.supervisor.pid)"
case "$supervisor" in
    ''|*[!0-9]*) ;;
    *) kill -TERM "$supervisor" 2>/dev/null && log "stopped the supervisor pid=$supervisor" ;;
esac

# 2. Services, in order.
in_chroot 3 /opt/a26-shell/bin/a26-shellctl quit >/dev/null 2>&1
stop_exe "$ROOT/opt/a26-shell/bin/a26-shell" 3
stop_exe "$ROOT/opt/a26-shell/bin/moon-notifyd" 3
if [ -x "$PERSIST/moon-telephony-stop.sh" ]; then
    "$BB" timeout -s KILL 10 /system/bin/sh "$PERSIST/moon-telephony-stop.sh" >/dev/null 2>&1
    log 'telephony stopped'
fi
if [ -x "$PERSIST/moon-audio-stop.sh" ]; then
    "$BB" timeout -s KILL 10 /system/bin/sh "$PERSIST/moon-audio-stop.sh" >/dev/null 2>&1
    log 'audio stopped'
fi
if [ -x "$ROOT/opt/a26-system/libexec/a26-wifi-stop" ]; then
    in_chroot 15 /opt/a26-system/libexec/a26-wifi-stop >/dev/null 2>&1
    log "Wi-Fi stopped rc=$?"
fi
stop_exe "$ROOT/usr/libexec/Xorg" 3
stop_exe "$ROOT/usr/bin/Xwayland" 3
stop_exe "$ROOT/opt/moon-gpu/moon-compositor" 3

# 3. A normal Android shutdown.
sync
log "services stopped; sys.powerctl $POWERCTL"
sync
setprop sys.powerctl "$POWERCTL"
sleep 30
log "still up 30 s after sys.powerctl; /system/bin/reboot $FALLBACK"
sync
/system/bin/reboot $FALLBACK
