#!/system/bin/sh
set -u

STATE_DIR=/data/local/tmp/a26-xorg-control
LOG="$STATE_DIR/restore.log"
mkdir -p "$STATE_DIR"
exec >>"$LOG" 2>&1

LOCK="$STATE_DIR/restore.lock"
if ! mkdir "$LOCK" 2>/dev/null; then
    owner="$(cat "$LOCK/pid" 2>/dev/null || true)"
    case "$owner" in
        ''|*[!0-9]*) owner='' ;;
    esac
    if [ -n "$owner" ] && kill -0 "$owner" 2>/dev/null; then
        echo "restore already running as pid=$owner"
        exit 0
    fi
    rm -rf "$LOCK"
    mkdir "$LOCK" 2>/dev/null || exit 0
fi
echo $$ >"$LOCK/pid"
cleanup_lock() { rm -rf "$LOCK"; }
trap cleanup_lock EXIT HUP INT TERM

echo "restore begin $(date --iso-8601=seconds 2>/dev/null || date)"

# Stop only the Linux display processes recorded by our launcher.  Never use a
# broad kill pattern: Android also has processes with graphics-related names.
for pidfile in \
    /data/local/a26-linux/run/a26-xterm.pid \
    /data/local/a26-linux/run/a26-xorg.pid
do
    [ -r "$pidfile" ] || continue
    pid="$(cat "$pidfile" 2>/dev/null || true)"
    case "$pid" in
        ''|*[!0-9]*) continue ;;
    esac
    if kill -0 "$pid" 2>/dev/null; then
        echo "terminating Linux display pid=$pid from $pidfile"
        kill -TERM "$pid" 2>/dev/null || true
    fi
done

# If an earlier chroot wrapper exited before its /run pidfile could be read,
# Xorg may have been reparented to Android init.  There is no Android Xorg
# process on this device, so this exact-name fallback is still narrowly scoped.
for pid in $(pidof Xorg 2>/dev/null || true); do
    echo "terminating orphaned Xorg pid=$pid"
    kill -TERM "$pid" 2>/dev/null || true
done
sleep 2
for pidfile in \
    /data/local/a26-linux/run/a26-xterm.pid \
    /data/local/a26-linux/run/a26-xorg.pid
do
    [ -r "$pidfile" ] || continue
    pid="$(cat "$pidfile" 2>/dev/null || true)"
    case "$pid" in
        ''|*[!0-9]*) continue ;;
    esac
    kill -KILL "$pid" 2>/dev/null || true
done
for pid in $(pidof Xorg 2>/dev/null || true); do
    kill -KILL "$pid" 2>/dev/null || true
done

# The pre-authorized AudioTrack bridge belongs only to the Moon session. Stop
# its exact recorded process before Android resumes ownership of media policy.
if [ -x /data/adb/moon/moon-audio-stop.sh ]; then
    /system/bin/sh /data/adb/moon/moon-audio-stop.sh 2>/dev/null || true
fi

# moon-telephonyd is rild's client in Moon mode and may own a mobile data
# interface; end it so Android's phone process is the only client.
if [ -x /data/adb/moon/moon-telephony-stop.sh ]; then
    /system/bin/sh /data/adb/moon/moon-telephony-stop.sh 2>/dev/null || true
fi

# Native Wi-Fi owns wlan0 and Android policy table 1023 while Moon is active.
# Always remove that state before restarting Android's framework/network stack.
if [ -x /data/local/tmp/a26-wifi-cleanup-android.sh ]; then
    /system/bin/sh /data/local/tmp/a26-wifi-cleanup-android.sh 2>/dev/null || true
fi

# GPU mode: moon-compositor holds DRM master; release it before the composer.
if [ -x /data/adb/moon/moon-gpu-session.sh ]; then
    /system/bin/sh /data/adb/moon/moon-gpu-session.sh stop 2>/dev/null || true
fi

# Restore the three services deliberately stopped for DRM-master handoff.  The
# composer must be ready before SurfaceFlinger reconnects to it.
/system/bin/start vendor.hwcomposer-2-4 2>/dev/null || true
sleep 2
/system/bin/start ExynosHWCServiceTW 2>/dev/null || true
sleep 1
/system/bin/start surfaceflinger 2>/dev/null || true

for _ in $(seq 1 30); do
    sf="$(pidof surfaceflinger 2>/dev/null || true)"
    hwc="$(pidof android.hardware.graphics.composer@2.4-service 2>/dev/null || true)"
    [ -n "$sf" ] && [ -n "$hwc" ] && break
    sleep 1
done

echo "surfaceflinger=$(pidof surfaceflinger 2>/dev/null || true)"
echo "hwcomposer=$(pidof android.hardware.graphics.composer@2.4-service 2>/dev/null || true)"
echo "ExynosHWCServiceTW=$(pidof vendor.samsung_slsi.hardware.ExynosHWCServiceTW@1.0-service 2>/dev/null || true)"

# End Moon's post-ready stage (its own session, recorded by itself) so it can
# neither stop services nor keep syncing time once Android is back.
post_ready="$(cat /data/local/tmp/moon-boot/post-ready.pid 2>/dev/null || true)"
case "$post_ready" in ''|*[!0-9]*) post_ready='' ;; esac
if [ -n "$post_ready" ] && tr '\000' ' ' <"/proc/$post_ready/cmdline" 2>/dev/null | grep -q moon-post-ready.sh; then
    kill -TERM "-$post_ready" 2>/dev/null || true
    echo "stopped Moon post-ready stage pid=$post_ready"
fi
rm -f /data/local/tmp/moon-boot/post-ready.pid

# Moon mode stopped the Android services it does not use (moon-post-ready.sh).
# Start exactly those again, from this boot's record only, before the
# framework looks for them.
STOPPED=/data/local/tmp/moon-boot/stopped-services
boot_id="$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || true)"
if [ -r "$STOPPED" ] && [ "$(sed -n 1p "$STOPPED")" = "boot_id=$boot_id" ]; then
    for service in $(sed 1d "$STOPPED"); do
        /system/bin/start "$service" 2>/dev/null || true
    done
    echo "restarted Moon-mode services: $(sed 1d "$STOPPED" | tr '\n' ' ')"
fi
rm -f "$STOPPED"

# Resume the Java framework only after the composer and SurfaceFlinger are
# available.  Starting zygote recreates system_server; the secondary zygote is
# needed for 32-bit applications on this firmware. Reset RescueParty's
# same-boot system_server restart counter first: an intentional Linux/Android
# handoff must not accumulate toward another PlatformReset recovery prompt.
/debug_ramdisk/magisk resetprop sys.rescue_boot_count 0 2>/dev/null || true
/debug_ramdisk/magisk resetprop crashrecovery.rescue_boot_count 0 2>/dev/null || true
# On a direct boot this hands the boot to Android for good; the boot ramdisk's
# session wrapper must not run its own fallback afterwards.
[ "$(getprop moon.boot.mode)" != moon ] || setprop moon.boot.mode android
echo "rescue_boot_count_before_zygote=$(getprop sys.rescue_boot_count)"
/system/bin/start zygote 2>/dev/null || true
sleep 1
/system/bin/start zygote_secondary 2>/dev/null || true
for _ in $(seq 1 60); do
    [ -n "$(pidof system_server 2>/dev/null || true)" ] && break
    sleep 1
done
echo "zygote64=$(pidof zygote64 2>/dev/null || true)"
echo "zygote=$(pidof zygote 2>/dev/null || true)"
echo "system_server=$(pidof system_server 2>/dev/null || true)"

# A takeover can occur while Android considers the panel asleep. This is only
# a wake request; it does not inject unlock credentials or otherwise operate UI.
/system/bin/input keyevent KEYCODE_WAKEUP 2>/dev/null || true

# Best-effort cleanup of only the two transient mounts that can remain if an
# Xorg child outlives the chroot wrapper.  Other bind mounts are cleaned by the
# wrapper itself and are intentionally not touched here.
/data/local/a26-linux/busybox.static umount /data/local/a26-linux/tmp 2>/dev/null || true
/data/local/a26-linux/busybox.static umount /data/local/a26-linux/dev 2>/dev/null || true

rm -f "$STATE_DIR/active-token" "$STATE_DIR/keep-token" \
    "$STATE_DIR/framework-suspended"
echo "restore complete $(date --iso-8601=seconds 2>/dev/null || date)"
