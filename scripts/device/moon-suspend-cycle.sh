#!/system/bin/sh
set -u

ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
PERSIST=/data/adb/moon
RUNTIME=/data/local/tmp/moon-suspend
LOG="$PERSIST/suspend.log"
STATE="$RUNTIME/state"
MOON_PID="${1:-}"
MOON_SESSION="${2:-}"
TEST_SECONDS="${3:-0}"

mkdir -p "$PERSIST" "$RUNTIME"
chmod 0700 "$PERSIST" "$RUNTIME"
exec </dev/null >>"$LOG" 2>&1

log() {
    echo "moon-suspend: $* [$(date --iso-8601=seconds 2>/dev/null || date)]"
}

case "$MOON_PID:$MOON_SESSION:$TEST_SECONDS" in
    *[!0-9:]*|:*|*::*) log 'invalid handoff arguments'; exit 10 ;;
esac
[ "$MOON_PID" -gt 1 ] && [ "$MOON_SESSION" -gt 1 ] || {
    log 'unsafe handoff identity'
    exit 10
}
case "$TEST_SECONDS" in
    0) ;;
    *) [ "$TEST_SECONDS" -ge 2 ] && [ "$TEST_SECONDS" -le 30 ] || {
        log 'RTC proof duration is out of range'
        exit 10
    } ;;
esac

LOCK="$RUNTIME/lock"
if ! mkdir "$LOCK" 2>/dev/null; then
    owner="$(cat "$LOCK/pid" 2>/dev/null || true)"
    case "$owner" in ''|*[!0-9]*) owner='' ;; esac
    if [ -n "$owner" ] && kill -0 "$owner" 2>/dev/null; then
        log "another suspend cycle owns the lock pid=$owner"
        exit 11
    fi
    rm -rf "$LOCK"
    mkdir "$LOCK" 2>/dev/null || exit 11
fi
echo $$ >"$LOCK/pid"
echo $$ >"$RUNTIME/active.new"
chmod 0600 "$RUNTIME/active.new"
mv -f "$RUNTIME/active.new" "$RUNTIME/active"

power=/sys/devices/platform/acpm_mfd_bus@11a00000/i2c-11/11-001f/s2mpu13-power-keys/power/wakeup
rtc=/sys/devices/platform/acpm_mfd_bus@11a00000/i2c-11/11-001f/s2mpu13-rtc/power/wakeup
volume=/sys/devices/platform/gpio_keys/power/wakeup
usbpd=/sys/devices/platform/13860000.hsi2c/i2c-3/3-003c/power_supply/usbpd-manager/power/wakeup
usb=/sys/devices/platform/samsung_mobile_device/samsung_mobile_device:battery/power_supply/usb/power/wakeup
wake_saved=0

restore_wake_policy() {
    [ "$wake_saved" = 1 ] || return 0
    printf '%s\n' "$power_before" >"$power" 2>/dev/null || true
    printf '%s\n' "$rtc_before" >"$rtc" 2>/dev/null || true
    printf '%s\n' "$volume_before" >"$volume" 2>/dev/null || true
    printf '%s\n' "$usbpd_before" >"$usbpd" 2>/dev/null || true
    printf '%s\n' "$usb_before" >"$usb" 2>/dev/null || true
    echo 0 >/sys/class/rtc/rtc0/wakealarm 2>/dev/null || true
    wake_saved=0
}

read_count() {
    value="$(sed -n 's/^count=//p' "$STATE" 2>/dev/null | tail -1)"
    case "$value" in ''|*[!0-9]*) echo 0 ;; *) echo "$value" ;; esac
}

write_state() {
    count=$1
    duration_ms=$2
    error=$3
    {
        echo 'version=1'
        echo "count=$count"
        echo "last_suspend_ms=$duration_ms"
        echo "last_error=$error"
    } >"$STATE.new.$$"
    chmod 0600 "$STATE.new.$$"
    mv -f "$STATE.new.$$" "$STATE"
}

restore_android() {
    /system/bin/sh /data/local/tmp/a26-android-graphics-restore.sh >/dev/null 2>&1 || true
}

fail() {
    code=$1
    log "cycle failed code=$code"
    restore_wake_policy
    write_state "$(read_count)" '' "$code"
    restore_android
    rm -f "$RUNTIME/active"
    rm -rf "$LOCK"
    exit 20
}

trap 'fail suspend_helper_interrupted' HUP INT TERM

for required in \
    "$BB" \
    "$ROOT/usr/bin/xrandr" \
    /data/local/tmp/a26-android-graphics-restore.sh
 do
    [ -x "$required" ] || fail suspend_helper_missing
 done
[ -e /sys/power/state ] && grep -qw mem /sys/power/state || fail mem_suspend_unavailable
[ -e /sys/power/mem_sleep ] && grep -qw deep /sys/power/mem_sleep || fail deep_suspend_unavailable
[ -e /sys/class/rtc/rtc0/wakealarm ] || fail rtc_unavailable
for source in "$power" "$rtc" "$volume" "$usbpd" "$usb"; do
    [ -e "$source" ] || fail wake_policy_unavailable
 done

log "cycle begin moon=$MOON_PID session=$MOON_SESSION rtc_test=${TEST_SECONDS}s battery=$(cat /sys/class/power_supply/battery/capacity 2>/dev/null || echo unknown)"

# Moon has already committed the lock frame, app unmaps, touchscreen-off event,
# and backlight zero. Let it answer the initiating IPC request and exit cleanly,
# then terminate every remaining process in its old session before DSI teardown.
for _ in $($BB seq 1 50); do
    kill -0 "$MOON_PID" 2>/dev/null || break
    "$BB" sleep 0.1
 done
kill -TERM "-$MOON_SESSION" 2>/dev/null || true
for _ in $($BB seq 1 30); do
    alive=0
    for stat in /proc/[0-9]*/stat; do
        set -- $(cat "$stat" 2>/dev/null || true)
        if [ "${6:-}" = "$MOON_SESSION" ]; then alive=1; break; fi
    done
    [ "$alive" = 0 ] && break
    "$BB" sleep 0.1
 done
[ "${alive:-0}" = 0 ] || kill -KILL "-$MOON_SESSION" 2>/dev/null || true

# Merely setting brightness to zero leaves Samsung's DPU domain active. A raw
# mem/deep request in that state resets the phone in pmucal_local_disable. RandR
# output disable is therefore a required, verified precondition, not an effect.
"$BB" chroot "$ROOT" /bin/sh -lc 'DISPLAY=:0 xrandr --output DSI-1 --off' || \
    fail dpu_quiesce_failed
dpu_ready=0
for _ in $($BB seq 1 50); do
    if [ "$(cat /sys/devices/platform/14940000.drmdecon/power/runtime_status 2>/dev/null)" = suspended ] &&
       [ "$(cat /sys/devices/platform/148c0000.drmdsim/power/runtime_status 2>/dev/null)" = suspended ] &&
       [ "$(cat /sys/class/drm/card0-DSI-1/enabled 2>/dev/null)" = disabled ]; then
        dpu_ready=1
        break
    fi
    "$BB" sleep 0.1
 done
[ "$dpu_ready" = 1 ] || fail dpu_quiesce_failed

power_before="$(cat "$power")"
rtc_before="$(cat "$rtc")"
volume_before="$(cat "$volume")"
usbpd_before="$(cat "$usbpd")"
usb_before="$(cat "$usb")"
wake_saved=1
printf 'enabled\n' >"$power" || fail wake_policy_failed
printf 'enabled\n' >"$rtc" || fail wake_policy_failed
printf 'disabled\n' >"$volume" || fail wake_policy_failed
# A configured host repeatedly wakes this kernel if either USB source remains
# enabled. Disable both only for the deep interval; the mandatory warm reboot
# after resume reconstructs Samsung's Type-C/DWC3 state and restores USB.
printf 'disabled\n' >"$usbpd" || fail wake_policy_failed
printf 'disabled\n' >"$usb" || fail wake_policy_failed

# Android AlarmManager may have left an RTC alarm armed before the framework
# handoff. Moon cannot service those Android alarms, so never let one convert a
# normal screen-off into an unsolicited full wake: clear it, then arm Moon's
# own. moon-notifyd keeps the next alarm/timer time (Unix seconds) in
# next-wake; wake 60 s early because the warm reboot after a wake takes ~45 s
# to bring Moon back, and moon-notifyd rings anything less than 15 minutes
# late. The root-only proof command arms its own bounded alarm instead.
echo 0 >/sys/class/rtc/rtc0/wakealarm || fail rtc_arm_failed
alarm=0
wake_reason=''
NEXT_WAKE="$ROOT/var/lib/moon-notify/next-wake"
if [ "$TEST_SECONDS" -gt 0 ]; then
    now="$(cat /sys/class/rtc/rtc0/since_epoch)"
    echo "+$TEST_SECONDS" >/sys/class/rtc/rtc0/wakealarm || fail rtc_arm_failed
    alarm="$(cat /sys/class/rtc/rtc0/wakealarm)"
    [ "$alarm" -ge $((now + TEST_SECONDS - 1)) ] || fail rtc_arm_failed
    wake_reason=rtc_test
else
    next_wake="$(head -n 1 "$NEXT_WAKE" 2>/dev/null || true)"
    case "$next_wake" in
        ''|*[!0-9]*) ;;
        *)
            # wakealarm counts in RTC time; correct for any RTC offset.
            rtc_now="$(cat /sys/class/rtc/rtc0/since_epoch)"
            target=$((next_wake - 60 - $(date +%s) + rtc_now))
            [ "$target" -ge $((rtc_now + 2)) ] || target=$((rtc_now + 2))
            echo "$target" >/sys/class/rtc/rtc0/wakealarm || fail rtc_arm_failed
            alarm="$(cat /sys/class/rtc/rtc0/wakealarm)"
            [ "$alarm" = "$target" ] || fail rtc_arm_failed
            wake_reason=moon_alarm
            log "armed RTC for moon-notifyd next-wake=$next_wake rtc_alarm=$alarm"
            ;;
    esac
fi

irq_count() {
    needle=$1
    awk -v needle="$needle" '
        index($0, needle) {
            total=0
            for (field=2; field<=NF && $field ~ /^[0-9]+$/; field++) total += $field
            print total
            found=1
            exit
        }
        END { if (!found) print 0 }
    ' /proc/interrupts
}

echo deep >/sys/power/mem_sleep || fail deep_select_failed
total_elapsed=0
while :; do
    power_f_before="$(irq_count pwronf-irq)"
    power_r_before="$(irq_count pwronr-irq)"
    rtc_irq_before="$(irq_count rtc-alarm0)"
    before="$(cat /sys/class/rtc/rtc0/since_epoch)"
    log "entering mem/deep dpu=suspended rtc_alarm=$(cat /sys/class/rtc/rtc0/wakealarm 2>/dev/null || true)"
    sync
    if ! echo mem >/sys/power/state; then
        fail deep_suspend_failed
    fi
    after="$(cat /sys/class/rtc/rtc0/since_epoch)"
    elapsed=$((after - before))
    [ "$elapsed" -ge 0 ] || elapsed=0
    total_elapsed=$((total_elapsed + elapsed))
    power_f_after="$(irq_count pwronf-irq)"
    power_r_after="$(irq_count pwronr-irq)"
    rtc_irq_after="$(irq_count rtc-alarm0)"

    if [ "$power_f_after" -gt "$power_f_before" ] ||
       [ "$power_r_after" -gt "$power_r_before" ]; then
        wake_kind=power_key
        break
    fi
    if [ "$alarm" -gt 0 ] && {
       [ "$rtc_irq_after" -gt "$rtc_irq_before" ] || [ "$after" -ge "$alarm" ];
    }; then
        wake_kind=$wake_reason
        break
    fi

    # Charger, fuel-gauge, USB and incidental kernel wakeups are not user wake
    # requests. Resume only long enough to classify them, then return to deep
    # sleep with the display pipeline still safely quiesced. Avoid a tight loop
    # if a pending wake source rejects entry immediately.
    log "ignored non-user wake elapsed=${elapsed}s; returning to mem/deep"
    [ "$elapsed" -gt 0 ] || sleep 1
done

restore_wake_policy
count=$(( $(read_count) + 1 ))
write_state "$count" "$((total_elapsed * 1000))" ''
log "approved wake=$wake_kind elapsed=${total_elapsed}s; committing warm reboot"

# This firmware has two independent post-resume limitations: Xorg cannot
# re-enable a DSI CRTC once it was quiesced, and Exynos DWC3 remains physically
# detached even after configfs, core-driver, and glue-driver resets. A controlled
# warm reboot is therefore the fail-closed production boundary. Magisk service.d
# starts Moon again on the new boot, where Samsung initializes both subsystems and
# Moon reads the persisted count/duration above before presenting its lock screen.
rm -f "$RUNTIME/active"
rm -rf "$LOCK"
trap - HUP INT TERM
log "requesting warm reboot count=$count"
sync
setprop sys.powerctl reboot,moon-suspend-resume
sleep 30
fail warm_reboot_failed
