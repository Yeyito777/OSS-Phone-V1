#!/system/bin/sh
# Moon's post-ready stage. The supervisor starts it detached once Moon answers
# IPC, so nothing here delays the lock screen:
#   1. native Wi-Fi (a26-system's backend; it autoconnects to saved networks)
#   2. direct boot only: stop the Android services Moon does not use
#      (moon-mode-stop.list); the Android restore path starts them again
#   3. network time: sync once Wi-Fi is up, then twice a day
# It exits as soon as Android owns the phone again.
set -u

ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
PERSIST=/data/adb/moon
RUNTIME=/data/local/tmp/moon-boot
LOG="$PERSIST/post-ready.log"
STOP_LIST="$PERSIST/moon-mode-stop.list"
STOPPED="$RUNTIME/stopped-services"
CHROOT_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
NTP_SERVERS='time.cloudflare.com time.google.com pool.ntp.org'

DIRECT=0
[ "${1:-}" != --direct ] || DIRECT=1

mkdir -p "$RUNTIME"
if [ -f "$LOG" ] && [ "$($BB stat -c %s "$LOG" 2>/dev/null || echo 0)" -gt 262144 ]; then
    mv -f "$LOG" "$LOG.1"
fi
exec </dev/null >>"$LOG" 2>&1

log() {
    echo "moon-post-ready: $* [$(date --iso-8601=seconds 2>/dev/null || date) up=$(cut -d' ' -f1 /proc/uptime)]"
}

in_chroot() {
    "$BB" chroot "$ROOT" /usr/bin/env -i HOME=/root PATH="$CHROOT_PATH" "$@"
}

moon_owns_phone() {
    [ -n "$(pidof Xorg 2>/dev/null || true)" ] && [ "$(getprop moon.boot.mode)" != android ] &&
        [ ! -e "$PERSIST/disabled" ]
}

boot_id="$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || echo unknown)"
log "start boot_id=$boot_id direct=$DIRECT"

if [ -x "$ROOT/opt/a26-system/libexec/a26-wifi-start" ]; then
    if in_chroot /opt/a26-system/libexec/a26-wifi-start; then
        log 'native Wi-Fi started'
    else
        log "native Wi-Fi start failed rc=$?; continuing without Wi-Fi"
    fi
fi

# Stopped services are recorded with this boot's ID so the restore path never
# acts on a stale list. `stop` also marks them disabled, so nothing in init
# starts them again until Android's restore path does.
stop_unused_services() {
    [ -r "$STOP_LIST" ] || return 0
    [ "$(sed -n 1p "$STOPPED" 2>/dev/null)" = "boot_id=$boot_id" ] || echo "boot_id=$boot_id" >"$STOPPED"
    stopped=''
    for service in $(sed -e 's/#.*//' "$STOP_LIST"); do
        case "$(getprop "init.svc.$service")" in
            running|restarting) ;;
            *) continue ;;
        esac
        moon_owns_phone || return 0
        /system/bin/stop "$service" 2>/dev/null || continue
        grep -qx "$service" "$STOPPED" || echo "$service" >>"$STOPPED"
        stopped="$stopped $service"
    done
    [ -z "$stopped" ] || log "stopped:$stopped"
}

if [ "$DIRECT" = 1 ]; then
    # Some services only start from boot-completed triggers, so look twice.
    "$BB" sleep 5
    moon_owns_phone && stop_unused_services
    "$BB" sleep 55
    moon_owns_phone && stop_unused_services
fi

# The phone keeps time only in its RTC without the framework. Sync once
# network time is reachable, then write the RTC (UTC) as Android does.
sync_time() {
    servers=''
    for server in $NTP_SERVERS; do servers="$servers -p $server"; done
    result="$(in_chroot timeout 25 ntpd -n -q -d $servers 2>&1)" || return 1
    [ ! -c /dev/rtc0 ] || in_chroot hwclock -w -u -f /dev/rtc0 2>/dev/null || true
    log "network time synced: $(echo "$result" | grep -E 'setting time|offset:' | tail -1)"
}

retry=30
while moon_owns_phone; do
    if sync_time; then
        retry=30
        "$BB" sleep 43200
    else
        "$BB" sleep "$retry"
        [ "$retry" -ge 900 ] || retry=$((retry * 2))
    fi
done
log 'Android owns the phone; exiting'
exit 0
