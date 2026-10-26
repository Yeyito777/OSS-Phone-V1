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
# The supervisor starts this with setsid; the restore path ends the group.
echo $$ >"$RUNTIME/post-ready.pid"

# moon-notifyd (notifications, alarms, timers) runs for as long as Moon owns
# the phone, restarted with backoff if it exits. It stays in this process
# group, so the Android restore path's group kill stops it too; it saves its
# state and silences the vibrator on SIGTERM.
NOTIFYD=/opt/a26-shell/bin/moon-notifyd
NOTIFY_LOG="$PERSIST/notifyd.log"
keep_notifyd() {
    delay=2
    while moon_owns_phone; do
        if [ -f "$NOTIFY_LOG" ] && [ "$($BB stat -c %s "$NOTIFY_LOG" 2>/dev/null || echo 0)" -gt 262144 ]; then
            mv -f "$NOTIFY_LOG" "$NOTIFY_LOG.1"
        fi
        started=$(cut -d. -f1 /proc/uptime)
        in_chroot "$NOTIFYD" </dev/null >>"$NOTIFY_LOG" 2>&1
        rc=$?
        moon_owns_phone || break
        # A run that lasted a while resets the backoff.
        [ $(( $(cut -d. -f1 /proc/uptime) - started )) -lt 60 ] || delay=2
        log "moon-notifyd exited rc=$rc; restarting in ${delay}s"
        "$BB" sleep "$delay"
        [ "$delay" -ge 60 ] || delay=$((delay * 2))
    done
}
if [ -x "$ROOT$NOTIFYD" ]; then
    keep_notifyd &
fi

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
    (
        "$BB" sleep 5
        moon_owns_phone && stop_unused_services
        "$BB" sleep 55
        moon_owns_phone && stop_unused_services
    ) &
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

wifi_routed() {
    ip route show table all 2>/dev/null | grep -q '^default via .* dev wlan0'
}

# Poll for a Wi-Fi route for the first three minutes, then back off.
waited=0
retry=30
while moon_owns_phone; do
    if [ "$waited" -lt 180 ] && ! wifi_routed; then
        "$BB" sleep 5
        waited=$((waited + 5))
        continue
    fi
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
