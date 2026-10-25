#!/system/bin/sh
# Direct-boot policy, run by the boot ramdisk's moon-boot-mode.sh at
# post-fs-data (before zygote-start) with a 5 s timeout. Prints one word:
#   moon     start Moon instead of the Java framework
#   android  normal Android boot
#   legacy   normal Android boot with the old post-boot Magisk takeover
# Diagnostics go to stderr, which the caller appends to direct-boot.log.
set -u

ROOT=/data/local/a26-linux
PERSIST=/data/adb/moon
CONFIG="$PERSIST/config"

log() {
    echo "moon-policy: $* [$(date --iso-8601=seconds 2>/dev/null || date)]" >&2
}

decide() {
    log "$2"
    echo "$1"
    exit 0
}

read_uint() {
    value="$(cat "$1" 2>/dev/null || true)"
    case "$value" in
        ''|*[!0-9]*) echo "${2:-0}" ;;
        *) echo "$value" ;;
    esac
}

config_uint() {
    value="$(sed -n "s/^$1=//p" "$CONFIG" 2>/dev/null | tail -1)"
    case "$value" in
        ''|*[!0-9]*) echo "$2" ;;
        *) echo "$value" ;;
    esac
}

[ "$(config_uint DIRECT_BOOT 1)" = 1 ] || decide legacy 'DIRECT_BOOT=0; legacy post-boot takeover'
[ "$(getprop ro.bootmode)" != charger ] || decide android 'charger boot'
[ ! -e "$PERSIST/disabled" ] || decide android 'autostart disabled'
if [ -e "$PERSIST/skip-once" ]; then
    rm -f "$PERSIST/skip-once"
    decide android 'consumed skip-once'
fi
case "$(getprop ro.boot.bootreason)" in
    *recovery*|*download*) decide android 'recovery/download boot reason' ;;
esac
# A prior kernel panic gets one Android-only diagnostic boot instead of an
# automatic panic loop.
[ "$(cat /sys/module/sec_debug_reset_reason/parameters/reset_reason 2>/dev/null)" != 4 ] ||
    decide android 'previous boot was a kernel panic'

failures="$(read_uint "$PERSIST/start-failures" 0)"
max_failures="$(config_uint MAX_START_FAILURES 3)"
[ "$failures" -lt "$max_failures" ] ||
    decide android "failure limit reached ($failures/$max_failures)"

capacity="$(read_uint /sys/class/power_supply/battery/capacity 100)"
if [ "$capacity" -lt "$(config_uint MIN_START_BATTERY 20)" ] &&
   [ "$(read_uint /sys/class/power_supply/usb/online 0)" != 1 ] &&
   [ "$(read_uint /sys/class/power_supply/ac/online 0)" != 1 ]; then
    decide android "battery is $capacity% without external power"
fi

for required in \
    "$ROOT/busybox.static" \
    "$ROOT/a26-enter-chroot.sh" \
    "$ROOT/opt/a26-shell/bin/a26-shell" \
    "$ROOT/opt/a26-shell/bin/a26-shellctl" \
    "$ROOT/opt/a26-audio/moon-audiod" \
    "$ROOT/opt/a26-audio/moon-permissiond" \
    "$ROOT/usr/local/sbin/a26-xorg-persistent-session" \
    "$PERSIST/moon-boot-supervisor.sh" \
    "$PERSIST/moon-audio-start.sh" \
    "$PERSIST/moon-audio-stop.sh" \
    /data/local/tmp/a26-xorg-takeover.sh \
    /data/local/tmp/a26-xorg-safety-watchdog.sh \
    /data/local/tmp/a26-android-graphics-restore.sh
do
    [ -x "$required" ] || decide android "required executable missing: $required"
done

# Count the attempt now: a hang or reset before Moon is up must still count
# toward the failure limit. The supervisor resets it once Moon is running.
echo $((failures + 1)) >"$PERSIST/start-failures.new" &&
    chmod 0600 "$PERSIST/start-failures.new" &&
    mv -f "$PERSIST/start-failures.new" "$PERSIST/start-failures"
decide moon "direct boot attempt $((failures + 1))/$max_failures battery=$capacity% reason=$(getprop ro.boot.bootreason)"
