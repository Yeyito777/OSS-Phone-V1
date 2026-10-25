#!/system/bin/sh
# Give the phone back to Android: start the graphics services Moon stopped and
# then the Java framework, exactly as a normal boot would have. Self-contained
# so it works even when the scripts on /data are missing or broken.
reason="${1:-unspecified}"
LOG=/data/adb/moon/direct-boot.log
echo "moon-fallback: reason=$reason $(date 2>/dev/null)" >>"$LOG" 2>/dev/null

[ "$(getprop moon.boot.mode)" = moon ] || exit 0
setprop moon.boot.mode android

# The full restore script also stops Wi-Fi/audio state cleanly; prefer it.
RESTORE=/data/local/tmp/a26-android-graphics-restore.sh
if [ -r "$RESTORE" ] && /system/bin/timeout 120 /system/bin/sh "$RESTORE"; then
    [ -n "$(pidof system_server 2>/dev/null)" ] && exit 0
fi

for pid in $(pidof Xorg 2>/dev/null); do kill -KILL "$pid" 2>/dev/null; done
# Moon's native "permission" stand-in must not outlive the framework's start.
for pid in $(pidof moon-permissiond 2>/dev/null); do kill -KILL "$pid" 2>/dev/null; done
start vendor.hwcomposer-2-4
sleep 2
start ExynosHWCServiceTW
sleep 1
start surfaceflinger
/debug_ramdisk/magisk resetprop sys.rescue_boot_count 0 2>/dev/null
/debug_ramdisk/magisk resetprop crashrecovery.rescue_boot_count 0 2>/dev/null
start zygote
start zygote_secondary
exit 0
