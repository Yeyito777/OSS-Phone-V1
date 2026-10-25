#!/system/bin/sh
# Moon's session supervisor. Two entry points:
#   (no argument)  legacy: Magisk service.d after Android fully booted; suspends
#                  the Java framework, then takes over the display
#   --direct       direct boot: init service moon-session (boot ramdisk) when
#                  moon.boot.mode=moon; the framework was never started
set -u

DIRECT=0
[ "${1:-}" != --direct ] || DIRECT=1

ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
PERSIST=/data/adb/moon
RUNTIME=/data/local/tmp/moon-boot
XORG_STATE=/data/local/tmp/a26-xorg-control
LOG="$PERSIST/boot.log"
CONFIG="$PERSIST/config"

mkdir -p "$PERSIST" "$RUNTIME" "$XORG_STATE"
chmod 0700 "$PERSIST" "$RUNTIME"
if [ -f "$LOG" ] && [ "$($BB stat -c %s "$LOG" 2>/dev/null || echo 0)" -gt 1048576 ]; then
    mv -f "$LOG" "$LOG.1"
fi
exec </dev/null >>"$LOG" 2>&1

log() {
    echo "moon-boot: $* [$(date --iso-8601=seconds 2>/dev/null || date) up=$(cut -d' ' -f1 /proc/uptime)]"
}

read_uint() {
    value="$(cat "$1" 2>/dev/null || true)"
    case "$value" in
        ''|*[!0-9]*) echo "${2:-0}" ;;
        *) echo "$value" ;;
    esac
}

write_uint() {
    file=$1
    value=$2
    echo "$value" >"$file.new.$$"
    chmod 0600 "$file.new.$$"
    mv -f "$file.new.$$" "$file"
}

config_uint() {
    key=$1
    default=$2
    value="$(sed -n "s/^$key=//p" "$CONFIG" 2>/dev/null | tail -1)"
    case "$value" in
        ''|*[!0-9]*) echo "$default" ;;
        *) echo "$value" ;;
    esac
}

boot_id="$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || echo unknown-$$)"
LOCK="$RUNTIME/lock"
if ! mkdir "$LOCK" 2>/dev/null; then
    owner="$(cat "$LOCK/pid" 2>/dev/null || true)"
    owner_boot="$(cat "$LOCK/boot-id" 2>/dev/null || true)"
    case "$owner" in ''|*[!0-9]*) owner='' ;; esac
    if [ "$owner_boot" = "$boot_id" ] && [ -n "$owner" ] && kill -0 "$owner" 2>/dev/null; then
        log "supervisor already running pid=$owner"
        exit 0
    fi
    rm -rf "$LOCK"
    mkdir "$LOCK" 2>/dev/null || exit 0
fi
echo $$ >"$LOCK/pid"
echo "$boot_id" >"$LOCK/boot-id"
cleanup_lock() { rm -rf "$LOCK"; }
trap cleanup_lock EXIT HUP INT TERM

log "supervisor start boot_id=$boot_id reason=$(getprop ro.boot.bootreason) direct=$DIRECT"

if [ "$DIRECT" = 0 ]; then
    # With the direct-boot ramdisk, its policy already decided this boot.
    boot_mode="$(getprop moon.boot.mode)"
    [ -z "$boot_mode" ] || {
        log "direct boot owns this boot (moon.boot.mode=$boot_mode)"
        exit 0
    }
    [ ! -e "$PERSIST/disabled" ] || { log 'autostart disabled'; exit 0; }

    if [ -e "$PERSIST/skip-once" ]; then
        rm -f "$PERSIST/skip-once"
        log 'consumed skip-once; leaving Android active'
        exit 0
    fi
fi

previous_attempt="$(cat "$PERSIST/last-attempt-boot-id" 2>/dev/null || true)"
[ "$previous_attempt" != "$boot_id" ] || {
    log 'this boot was already attempted; refusing a duplicate takeover'
    exit 0
}
echo "$boot_id" >"$PERSIST/last-attempt-boot-id"
chmod 0600 "$PERSIST/last-attempt-boot-id"

MIN_START_BATTERY="$(config_uint MIN_START_BATTERY 20)"
LOW_BATTERY_FALLBACK="$(config_uint LOW_BATTERY_FALLBACK 8)"
MAX_START_FAILURES="$(config_uint MAX_START_FAILURES 3)"
OVERRIDE_WINDOW_SECONDS="$(config_uint OVERRIDE_WINDOW_SECONDS 8)"

DIRECT_OVERRIDE_SECONDS="$(config_uint DIRECT_OVERRIDE_SECONDS 3)"

failures="$(read_uint "$PERSIST/start-failures" 0)"
if [ "$DIRECT" = 0 ] && [ "$failures" -ge "$MAX_START_FAILURES" ]; then
    log "failure limit reached ($failures/$MAX_START_FAILURES); leaving Android active"
    exit 0
fi

# Hand a direct boot to Android exactly as a normal boot would have continued:
# the framework services were stopped right after zygote-start and
# SurfaceFlinger/the composer are still running.
start_android_framework() {
    setprop moon.boot.mode android
    write_uint "$PERSIST/start-failures" "$((failures > 0 ? failures - 1 : 0))"
    /system/bin/start zygote
    /system/bin/start zygote_secondary
}

if [ "$DIRECT" = 1 ]; then
    # Physical escape hatch: either volume key from now until shortly after the
    # boot animation appears starts Android instead. Not after a deep-suspend
    # resume, whose wake source can be a volume key.
    override_log="$RUNTIME/volume-override.$$"
    event_pid=''
    rm -f "$override_log"
    case "$(getprop ro.boot.bootreason)" in
        *moon-suspend-resume*) ;;
        *)
            if [ "$DIRECT_OVERRIDE_SECONDS" -gt 0 ]; then
                for _ in $($BB seq 1 50); do
                    [ -c /dev/input/event0 ] && break
                    "$BB" sleep 0.1
                done
                /system/bin/getevent -ql /dev/input/event0 >"$override_log" 2>&1 &
                event_pid=$!
            fi
            ;;
    esac
    volume_pressed() {
        [ -n "$event_pid" ] && grep -Eq 'KEY_VOLUME(UP|DOWN)[[:space:]]+DOWN' "$override_log" 2>/dev/null
    }

    # The composer and SurfaceFlinger initialize the panel; the boot animation
    # starting means SurfaceFlinger finished. Xorg then takes over the display
    # exactly as after a full Android boot.
    ready=0
    sf_ticks=0
    shown_ticks=0
    for _ in $($BB seq 1 600); do
        if volume_pressed; then
            kill "$event_pid" 2>/dev/null || true
            rm -f "$override_log"
            log 'volume-key override requested; starting the Android framework'
            start_android_framework
            exit 0
        fi
        if [ "$(getprop init.svc.surfaceflinger)" = running ] &&
           [ -c /dev/dri/card0 ] && [ -c /dev/input/event0 ] &&
           [ -x "$ROOT/a26-enter-chroot.sh" ]; then
            sf_ticks=$((sf_ticks + 1))
            # 100 ms ticks; proceed without an animation after 5 s.
            if [ "$(getprop init.svc.bootanim)" = running ] || [ "$sf_ticks" -ge 50 ]; then
                shown_ticks=$((shown_ticks + 1))
                if [ "$shown_ticks" -ge $((DIRECT_OVERRIDE_SECONDS * 10)) ]; then
                    ready=1
                    break
                fi
            fi
        fi
        "$BB" sleep 0.1
    done
    [ -z "$event_pid" ] || kill "$event_pid" 2>/dev/null || true
    rm -f "$override_log"
    [ "$ready" = 1 ] || { log 'SurfaceFlinger/display bootstrap timed out'; exit 20; }
    log "display bootstrap ready; boot animation=$(getprop init.svc.bootanim)"
else
# Magisk service.d runs before Android has necessarily finished initializing
# Samsung's vendor services. Let Android initialize/decrypt everything first.
ready=0
for _ in $($BB seq 1 360); do
    if [ "$(getprop sys.boot_completed)" = 1 ] &&
       [ -c /dev/dri/card0 ] && [ -c /dev/input/event0 ] &&
       [ -x "$ROOT/a26-enter-chroot.sh" ]; then
        ready=1
        break
    fi
    sleep 1
done
[ "$ready" = 1 ] || { log 'Android/vendor bootstrap timed out'; exit 20; }

case "$(getprop ro.boot.bootreason)" in
    *recovery*|*download*)
        log 'recovery/download boot detected; leaving Android active'
        exit 0
        ;;
esac

# A prior kernel panic gets one Android-only diagnostic boot instead of an
# automatic panic loop. A normal restart/power cycle reports another value.
reset_reason="$(cat /sys/module/sec_debug_reset_reason/parameters/reset_reason 2>/dev/null || true)"
if [ "$reset_reason" = 4 ]; then
    log 'previous boot was classified as kernel panic; leaving Android active'
    exit 0
fi

# Physical one-boot escape hatch: press either volume key during this short
# post-boot window. Input is observed but never consumed exclusively.
override_log="$RUNTIME/volume-override.$$"
rm -f "$override_log"
if [ -c /dev/input/event0 ] && [ "$OVERRIDE_WINDOW_SECONDS" -gt 0 ]; then
    /system/bin/getevent -ql /dev/input/event0 >"$override_log" 2>&1 &
    event_pid=$!
    sleep "$OVERRIDE_WINDOW_SECONDS"
    kill "$event_pid" 2>/dev/null || true
    wait "$event_pid" 2>/dev/null || true
    if grep -Eq 'KEY_VOLUME(UP|DOWN)[[:space:]]+DOWN' "$override_log"; then
        rm -f "$override_log"
        log 'volume-key override requested; leaving Android active'
        exit 0
    fi
fi
rm -f "$override_log"

capacity="$(read_uint /sys/class/power_supply/battery/capacity 100)"
if [ "$capacity" -lt "$MIN_START_BATTERY" ]; then
    online="$(read_uint /sys/class/power_supply/usb/online 0)"
    ac_online="$(read_uint /sys/class/power_supply/ac/online 0)"
    if [ "$online" != 1 ] && [ "$ac_online" != 1 ]; then
        log "battery is $capacity% without external power; leaving Android active"
        exit 0
    fi
    log "battery is $capacity%; waiting for $MIN_START_BATTERY% before Moon takeover"
    while [ "$capacity" -lt "$MIN_START_BATTERY" ]; do
        [ ! -e "$PERSIST/disabled" ] || { log 'disabled while charging'; exit 0; }
        sleep 60
        capacity="$(read_uint /sys/class/power_supply/battery/capacity 100)"
    done
fi
fi

for required in \
    "$BB" \
    "$ROOT/a26-enter-chroot.sh" \
    "$ROOT/opt/a26-shell/bin/a26-shell" \
    "$ROOT/opt/a26-shell/bin/a26-shellctl" \
    "$PERSIST/moon-audio-start.sh" \
    "$PERSIST/moon-audio-stop.sh" \
    "$ROOT/opt/a26-audio/moon-audiod" \
    "$ROOT/opt/a26-audio/moon-permissiond" \
    /data/local/tmp/a26-xorg-takeover.sh \
    /data/local/tmp/a26-xorg-safety-watchdog.sh \
    /data/local/tmp/a26-android-graphics-restore.sh \
    "$ROOT/usr/local/sbin/a26-xorg-persistent-session"
do
    [ -x "$required" ] || {
        log "required executable missing: $required"
        exit 21
    }
done
# The direct-boot policy already counted this attempt before zygote-start.
if [ "$DIRECT" = 0 ]; then
    failures=$((failures + 1))
    write_uint "$PERSIST/start-failures" "$failures"
fi
token="moon-${boot_id%%-*}-$failures"
rm -f "$ROOT/root/xorg-session.status"
rm -rf "$XORG_STATE/restore.lock"
: >"$XORG_STATE/takeover.log"
log "starting autonomous takeover token=$token attempt=$failures"
"$BB" nohup "$BB" setsid /system/bin/sh \
    /data/local/tmp/a26-xorg-takeover.sh "$token" \
    >/dev/null 2>&1 </dev/null &

# Native audio (AAudio plus Moon's "permission" stand-in) needs no framework,
# only that it is gone so the stand-in is the registered service. On a direct
# boot it never ran, so audio starts alongside Xorg; the legacy path starts it
# after the takeover stopped the framework. Moon reads the volume file it
# creates at startup, and still runs without audio.
audio_pid=''
if [ "$DIRECT" = 1 ]; then
    /system/bin/sh "$PERSIST/moon-audio-start.sh" &
    audio_pid=$!
fi
finish_audio_start() {
    audio_ok=0
    if [ -n "$audio_pid" ]; then
        wait "$audio_pid" && audio_ok=1
        audio_pid=''
    fi
}

xorg_ready=0
for _ in $($BB seq 1 500); do
    status="$(cat "$ROOT/root/xorg-session.status" 2>/dev/null || true)"
    case "$status" in
        XORG_READY=1*) xorg_ready=1; break ;;
        XORG_READY=0*) break ;;
    esac
    "$BB" sleep 0.2
done
if [ "$xorg_ready" != 1 ]; then
    log 'Xorg readiness was not proven; restoring Android'
    finish_audio_start
    /system/bin/sh /data/local/tmp/a26-android-graphics-restore.sh
    exit 30
fi
echo "$token" >"$XORG_STATE/keep-token"
log "Xorg ready pid=$(pidof Xorg 2>/dev/null || true); safety token committed locally"

finish_audio_start
if [ "$audio_ok" = 1 ] || /system/bin/sh "$PERSIST/moon-audio-start.sh"; then
    log "audio ready pid=$(cat /data/local/tmp/moon-audio/bridge.pid 2>/dev/null || true)"
else
    log 'native audio did not become ready; continuing without audio'
fi

start_moon() {
    rm -f "$ROOT/run/a26-shell/control.sock"
    : >"$ROOT/root/a26-shell.log"
    "$BB" nohup "$BB" setsid /system/bin/sh "$ROOT/a26-enter-chroot.sh" \
        /usr/bin/env DISPLAY=:0 A26_SHELL_CONFIG=/etc/a26-shell/config.json \
        /opt/a26-shell/bin/a26-shell \
        >>"$ROOT/root/a26-shell.log" 2>&1 </dev/null &
    session_pid=$!
    echo "$session_pid" >"$RUNTIME/moon-session.pid.new"
    chmod 0600 "$RUNTIME/moon-session.pid.new"
    mv -f "$RUNTIME/moon-session.pid.new" "$RUNTIME/moon-session.pid"
}

moon_control() {
    # A live PID is insufficient: a synchronous X11 wait can leave the process
    # sleeping forever while its UI and control socket are both unresponsive.
    # Bound every probe so the supervisor itself can never join that deadlock.
    probe_timeout="${1:?probe timeout required}"
    shift
    "$BB" timeout -s KILL "$probe_timeout" "$BB" chroot "$ROOT" \
        /opt/a26-shell/bin/a26-shellctl "$@"
}

capture_moon_incident() {
    incident_dir="$PERSIST/incidents"
    mkdir -p "$incident_dir"
    incident="$incident_dir/moon-unresponsive-$(date +%Y%m%d-%H%M%S 2>/dev/null || echo unknown)-$$.log"
    incident_new="$incident.new"
    moon_pid="$(pidof a26-shell 2>/dev/null | tr ' ' '\n' | head -n 1)"
    xorg_pid="$(pidof Xorg 2>/dev/null | tr ' ' '\n' | head -n 1)"
    {
        echo 'schema=1'
        echo "captured=$(date --iso-8601=seconds 2>/dev/null || date)"
        echo "moon_pid=$moon_pid"
        if [ -n "$moon_pid" ]; then
            printf 'moon_wchan='
            cat "/proc/$moon_pid/wchan" 2>/dev/null || true
            echo
            echo '[moon_stack]'
            cat "/proc/$moon_pid/stack" 2>/dev/null || true
            echo '[moon_status]'
            grep -E '^(Name|State|Pid|PPid|Threads|voluntary_ctxt_switches|nonvoluntary_ctxt_switches):' \
                "/proc/$moon_pid/status" 2>/dev/null || true
        fi
        echo "xorg_pid=$xorg_pid"
        if [ -n "$xorg_pid" ]; then
            printf 'xorg_wchan='
            cat "/proc/$xorg_pid/wchan" 2>/dev/null || true
            echo
            echo '[xorg_stack]'
            cat "/proc/$xorg_pid/stack" 2>/dev/null || true
        fi
        echo '[moon_log_tail]'
        tail -n 200 "$ROOT/root/a26-shell.log" 2>/dev/null || true
    } >"$incident_new"
    chmod 0600 "$incident_new"
    mv -f "$incident_new" "$incident"
    log "captured unresponsive Moon diagnostics at $incident"

    # Keep diagnostics useful without allowing a recurring fault to consume
    # unbounded persistent storage.
    incident_count=0
    for old_incident in $(ls -1t "$incident_dir"/moon-unresponsive-*.log 2>/dev/null || true); do
        incident_count=$((incident_count + 1))
        [ "$incident_count" -le 10 ] || rm -f "$old_incident"
    done
}

stop_moon_session() {
    session_pid="$(cat "$RUNTIME/moon-session.pid" 2>/dev/null || true)"
    case "$session_pid" in
        ''|*[!0-9]*)
            log 'Moon session PID is missing or invalid'
            return 1
            ;;
    esac
    # Refuse a stale/reused ID. setsid makes both process-group and session IDs
    # equal to the recorded launch PID. The leader can already have exited while
    # a Chromium descendant remains, so verify any surviving member rather than
    # requiring /proc/$session_pid itself.
    session_verified=0
    for process_stat in /proc/[0-9]*/stat; do
        set -- $(cat "$process_stat" 2>/dev/null || true)
        if [ "${5:-}" = "$session_pid" ] && [ "${6:-}" = "$session_pid" ]; then
            session_verified=1
            break
        fi
    done
    if [ "$session_verified" != 1 ]; then
        rm -f "$RUNTIME/moon-session.pid"
        return 0
    fi
    kill -TERM "-$session_pid" 2>/dev/null || true
    for _ in $($BB seq 1 25); do
        kill -0 "-$session_pid" 2>/dev/null || break
        "$BB" sleep 0.2
    done
    kill -0 "-$session_pid" 2>/dev/null && kill -KILL "-$session_pid" 2>/dev/null || true
    wait "$session_pid" 2>/dev/null || true
    rm -f "$RUNTIME/moon-session.pid"
}

wait_for_moon() {
    # Twenty one-second probes plus short spacing bound initial/recovery startup
    # to approximately 24 seconds, rather than multiplying a five-second probe
    # by 100 attempts.
    for _ in $($BB seq 1 20); do
        if moon_control 1 ping >"$RUNTIME/moon-state.new" 2>/dev/null; then
            mv -f "$RUNTIME/moon-state.new" "$RUNTIME/moon-state"
            return 0
        fi
        "$BB" sleep 0.2
    done
    rm -f "$RUNTIME/moon-state.new"
    return 1
}

restart_moon_after_incident() {
    capture_moon_incident
    stop_moon_session || return 1
    start_moon
    if wait_for_moon; then
        log 'Moon recovered locally after an unresponsive-process incident'
        return 0
    fi
    log 'Moon local recovery failed'
    return 1
}

start_moon
if ! wait_for_moon; then
    log 'Moon control socket did not become ready; restoring Android'
    /system/bin/sh /data/local/tmp/a26-android-graphics-restore.sh
    exit 31
fi

write_uint "$PERSIST/start-failures" 0
echo "$boot_id" >"$PERSIST/last-success-boot-id"
chmod 0600 "$PERSIST/last-success-boot-id"
log 'Moon is the active default session'
if [ "$DIRECT" = 1 ]; then
    setprop moon.session.ready 1
    # No framework will ever report boot completion. Report it for the vendor
    # actions that wait for it (battery driver, zram, thermal, lmkd), and so
    # init limits critical-process crash counting to its normal time window.
    if [ "$(getprop sys.boot_completed)" != 1 ]; then
        setprop sys.boot_completed 1
        setprop dev.bootcomplete 1
        log 'reported boot completion for the native session'
    fi
fi
# Everything not needed to show the lock screen (Wi-Fi, trimming Android
# services, network time) runs detached from here on.
if [ -x "$PERSIST/moon-post-ready.sh" ]; then
    post_ready_mode=''
    [ "$DIRECT" = 0 ] || post_ready_mode=--direct
    "$BB" nohup "$BB" setsid /system/bin/sh "$PERSIST/moon-post-ready.sh" $post_ready_mode \
        >/dev/null 2>&1 </dev/null &
fi

shell_unhealthy=0
local_recoveries=0
suspend_cycle_seen=0
while :; do
    sleep 30
    # A detached, device-namespace helper owns planned deep suspend. During that
    # interval Xorg and Moon intentionally disappear and are replaced. Never
    # mistake that bounded lifecycle transition for a crash or race its clean
    # Android DRM handoff. The helper removes `active` only after new Moon IPC is
    # responsive, or after its failure path has restored Android.
    if [ -e /data/local/tmp/moon-suspend/active ]; then
        if [ "$suspend_cycle_seen" = 0 ]; then
            log 'planned deep-suspend cycle is active; deferring liveness checks'
            suspend_cycle_seen=1
        fi
        continue
    fi
    if [ "$suspend_cycle_seen" = 1 ]; then
        if [ -n "$(pidof Xorg 2>/dev/null || true)" ] && moon_control 5 ping >/dev/null 2>&1; then
            log 'planned deep-suspend cycle completed with replacement Xorg/Moon'
            shell_unhealthy=0
            suspend_cycle_seen=0
        else
            log 'planned deep-suspend cycle failed safely; leaving Android active'
            break
        fi
    fi
    [ -n "$(pidof Xorg 2>/dev/null || true)" ] || break
    if [ -e "$PERSIST/disabled" ]; then
        log 'disabled at runtime; restoring Android'
        /system/bin/sh /data/local/tmp/a26-android-graphics-restore.sh
        break
    fi
    capacity="$(read_uint /sys/class/power_supply/battery/capacity 100)"
    if [ "$capacity" -le "$LOW_BATTERY_FALLBACK" ]; then
        log "battery reached $capacity%; restoring Android charging mode"
        /system/bin/sh /data/local/tmp/a26-android-graphics-restore.sh
        break
    fi
    if moon_control 5 ping >/dev/null 2>&1; then
        shell_unhealthy=0
    else
        shell_unhealthy=$((shell_unhealthy + 1))
        log "Moon liveness probe failed ($shell_unhealthy/2)"
        if [ "$shell_unhealthy" -ge 2 ]; then
            if [ "$local_recoveries" -ge 1 ]; then
                capture_moon_incident
                log 'Moon exceeded the one-recovery-per-boot budget; restoring Android'
                /system/bin/sh /data/local/tmp/a26-android-graphics-restore.sh
                break
            elif restart_moon_after_incident; then
                local_recoveries=$((local_recoveries + 1))
                shell_unhealthy=0
            else
                log 'Moon remained unavailable; restoring Android'
                /system/bin/sh /data/local/tmp/a26-android-graphics-restore.sh
                break
            fi
        fi
    fi
done

log 'supervisor exit'
exit 0
