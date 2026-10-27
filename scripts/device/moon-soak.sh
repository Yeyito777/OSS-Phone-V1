#!/system/bin/sh
# Soak test of the default Moon configuration (scripts/a26-shell/soak.sh
# drives it). Runs detached in Android's root namespace for HOURS hours:
#   moon-soak.sh RUN_DIR HOURS NO_CHARGE
# Every minute it samples the battery, memory and, per Moon process, CPU
# ticks, RSS, threads and open files (samples.csv, procs.csv). On a schedule
# it cycles the screen, drops and rejoins Wi-Fi and fires a silent alarm
# (events.csv). NO_CHARGE=1 pauses charging (Samsung slate mode) so the idle
# drain can be measured over USB; charging resumes at the end, on stop, and
# whenever the battery falls below 25 %. `touch RUN_DIR/stop` ends the run.
set -u

ROOT=/data/local/a26-linux
# --detach: start a copy of this script in its own session and return.
if [ "${1:-}" = --detach ]; then
    shift
    "$ROOT/busybox.static" setsid /system/bin/sh "$0" "$@" </dev/null >/dev/null 2>&1 &
    exit 0
fi
RUN="${1:?run directory required}"
HOURS="${2:-24}"
NO_CHARGE="${3:-0}"
ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
CHROOT_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
SLATE=/sys/class/power_supply/battery/batt_slate_mode
BATTERY=/sys/class/power_supply/battery

mkdir -p "$RUN"
exec </dev/null >>"$RUN/soak.log" 2>&1
echo $$ >"$RUN/pid"

now() { date +%s; }
log() { echo "soak: $* [$(date --iso-8601=seconds 2>/dev/null || date)]"; }
in_chroot() {
    limit=$1
    shift
    "$BB" timeout -s KILL "$limit" "$BB" chroot "$ROOT" /usr/bin/env -i HOME=/root \
        PATH="$CHROOT_PATH" "$@"
}
ctl() { in_chroot 10 /opt/a26-shell/bin/a26-shellctl "$@"; }
notifyctl() { in_chroot 10 /opt/a26-shell/bin/moon-notifyctl "$@"; }
wifictl() { in_chroot 20 /opt/a26-system/bin/a26-wifi-ctl "$@"; }
event() { echo "$(now),$1,$2,$3" >>"$RUN/events.csv"; log "event $1 $2 $3"; }
read_file() { cat "$1" 2>/dev/null || echo "${2:-}"; }

charging_paused=0
pause_charging() {
    [ "$NO_CHARGE" = 1 ] && [ -w "$SLATE" ] || return 0
    echo 1 >"$SLATE" && charging_paused=1 && log 'charging paused (slate mode)'
}
resume_charging() {
    [ "$charging_paused" = 1 ] || return 0
    echo 0 >"$SLATE" && charging_paused=0 && log 'charging resumed'
}
finish() {
    resume_charging
    log 'run ended'
    rm -f "$RUN/pid"
    exit 0
}
trap finish EXIT HUP INT TERM

PROCS="a26-shell moon-compositor Xorg moon-notifyd moon-audiod moon-permissiond moon-telephonyd a26-wifi-service wpa_supplicant udhcpc"
[ -f "$RUN/samples.csv" ] ||
    echo "t,uptime,capacity,voltage_uv,current_ua,status,usb_online,backlight,mem_available_kb,load1,boot_mode,battery_temp,cpu_busy,cpu_total,ctxt,intr,d_state" >"$RUN/samples.csv"
[ -f "$RUN/procs.csv" ] || echo "t,proc,pid,ticks,rss_kb,threads,fds" >"$RUN/procs.csv"
[ -f "$RUN/events.csv" ] || echo "t,event,result,detail" >"$RUN/events.csv"

sample() {
    t=$(now)
    mem="$(sed -n 's/^MemAvailable:[[:space:]]*\([0-9]*\).*/\1/p' /proc/meminfo)"
    # CPU ticks busy/total (all CPUs), context switches and interrupts since
    # boot (the report differentiates), and tasks in D state: the load
    # average counts those, and Samsung's TEE/UFS/CHUB kernel threads idle
    # there (14 on this phone), so the load says little about cost.
    set -- $(head -1 /proc/stat)
    cpu_total=$(($2 + $3 + $4 + $5 + $6 + $7 + $8 + $9))
    cpu_busy=$((cpu_total - $5 - $6))
    ctxt="$(sed -n 's/^ctxt //p' /proc/stat)"
    intr="$(sed -n 's/^intr \([0-9]*\).*/\1/p' /proc/stat)"
    d_state="$(cat /proc/[0-9]*/stat 2>/dev/null | grep -c ') D ')"
    echo "$t,$(cut -d' ' -f1 /proc/uptime),$(read_file $BATTERY/capacity),$(read_file $BATTERY/voltage_now),$(read_file $BATTERY/current_now),$(read_file $BATTERY/status),$(read_file /sys/class/power_supply/usb/online),$(read_file /sys/class/backlight/panel/brightness),$mem,$(cut -d' ' -f1 /proc/loadavg),$(getprop moon.boot.mode),$(read_file $BATTERY/temp),$cpu_busy,$cpu_total,$ctxt,$intr,$d_state" >>"$RUN/samples.csv"
    for name in $PROCS; do
        for pid in $(pidof "$name" 2>/dev/null); do
            stat="$(cat /proc/$pid/stat 2>/dev/null)" || continue
            ticks=$(echo "${stat##*) }" | cut -d' ' -f12,13 | { read u s; echo $((u + s)); })
            # /proc/PID/status separates fields with tabs.
            rss="$(sed -n 's/^VmRSS:[[:space:]]*\([0-9]*\).*/\1/p' /proc/$pid/status)"
            threads="$(sed -n 's/^Threads:[[:space:]]*\([0-9]*\).*/\1/p' /proc/$pid/status)"
            fds=$(ls /proc/$pid/fd 2>/dev/null | wc -l)
            echo "$t,$name,$pid,$ticks,${rss:-0},${threads:-0},$fds" >>"$RUN/procs.csv"
        done
    done
    capacity="$(read_file $BATTERY/capacity 100)"
    if [ "$charging_paused" = 1 ] && [ "$capacity" -lt 25 ]; then
        log "battery $capacity%: ending the drain measurement"
        resume_charging
        NO_CHARGE=0
    fi
}

# Screen on for 20 s (as a wake), then off; Moon must answer throughout.
screen_cycle() {
    start=$(now)
    ctl screen on >/dev/null || { event screen_on fail "ctl screen on failed"; return; }
    sleep 20
    if ctl ping >/dev/null; then ok=ok; else ok=fail; fi
    ctl screen off >/dev/null || ok=fail
    event screen_cycle "$ok" "$(($(now) - start))s"
}

# Disconnect from the current network and rejoin it; time until routed.
wifi_drop() {
    ssid="$(wifictl status | sed -n 's/.*"ssid": *"\([^"]*\)".*/\1/p' | head -1)"
    [ -n "$ssid" ] || { event wifi_drop skip 'not connected'; return; }
    wifictl disconnect >/dev/null
    sleep 10
    start=$(now)
    wifictl connect-known "$ssid" >/dev/null
    for _ in $("$BB" seq 1 60); do
        if in_chroot 5 ping -c1 -W2 1.1.1.1 >/dev/null 2>&1; then
            event wifi_drop ok "rejoined in $(($(now) - start))s"
            return
        fi
        sleep 2
    done
    event wifi_drop fail "not routed $(($(now) - start))s after connect-known"
}

# A silent alarm (no sound, no vibration) two minutes ahead; time it fires.
# moon-notifyd's local time zone may differ from the chroot's `date` (UTC):
# create, compare next_fire_at with the target, recreate shifted if needed.
create_alarm() { # epoch-of-wall-clock -> sets id, due
    set -- $(in_chroot 5 date -u -d @"$1" '+%H %M' 2>/dev/null)
    created="$(notifyctl alarm.create hour=${1#0} minute=${2#0} label=soak sound=none vibrate=false)"
    id="$(echo "$created" | sed -n 's/.*"id": *"\([^"]*\)".*/\1/p' | head -1)"
    due="$(echo "$created" | sed -n 's/.*"next_fire_at": *\([0-9]*\).*/\1/p' | head -1)"
}
alarm_fire() {
    target=$(($(now) + 120))
    create_alarm "$target"
    [ -n "$id" ] && [ -n "$due" ] || { event alarm fail "create: $created"; return; }
    offset=$((due - target))
    if [ "$offset" -gt 90 ] || [ "$offset" -lt -90 ]; then
        notifyctl alarm.delete alarm="$id" >/dev/null
        # Whole hours (time zones), into the next day's range.
        hours=$(( (offset + 1800) / 3600 ))
        create_alarm $((target - hours * 3600))
        [ -n "$id" ] && [ -n "$due" ] || { event alarm fail "recreate: $created"; return; }
    fi
    while [ "$(now)" -lt $((due + 120)) ]; do
        if notifyctl alarm.list | tr -d ' \n' | grep -q "\"id\":\"$id\"[^}]*\"ringing\":true"; then
            event alarm ok "rang $(($(now) - due))s after due"
            notifyctl alarm.stop alarm="$id" >/dev/null
            notifyctl alarm.delete alarm="$id" >/dev/null
            sleep 5
            ctl screen off >/dev/null
            return
        fi
        sleep 2
    done
    event alarm fail "did not ring within 120 s of $due"
    notifyctl alarm.delete alarm="$id" >/dev/null
}

start=$(now)
end=$((start + HOURS * 3600))
log "run start: ${HOURS}h no_charge=$NO_CHARGE"
pause_charging
next_screen=$((start + 600))
next_alarm=$((start + 2700))
next_wifi=$((start + 5400))
while [ "$(now)" -lt "$end" ] && [ ! -e "$RUN/stop" ]; do
    sample
    t=$(now)
    if [ "$t" -ge "$next_screen" ]; then screen_cycle; next_screen=$((t + 1200)); fi
    if [ "$t" -ge "$next_alarm" ]; then alarm_fire; next_alarm=$((t + 8 * 3600)); fi
    if [ "$t" -ge "$next_wifi" ]; then wifi_drop; next_wifi=$((t + 6 * 3600)); fi
    sleep 60
done
