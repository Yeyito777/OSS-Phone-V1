#!/system/bin/sh
set -u

ROOT=/data/local/a26-linux
STATE=/data/local/tmp/a26-xorg-control
LOG="$STATE/takeover.log"
TOKEN="${1:-xorg-takeover-$(date +%s)-$$}"
mkdir -p "$STATE"
exec </dev/null >>"$LOG" 2>&1

echo "takeover begin token=$TOKEN $(date --iso-8601=seconds 2>/dev/null || date)"

if [ -n "$(pidof Xorg 2>/dev/null || true)" ]; then
    echo "Xorg is already running"
    exit 0
fi

# Every start is bounded until the host observes a working X protocol and
# writes this same token to keep-token.  Runtime crashes are separately handled
# by the foreground chroot/supervisor path and restore Android immediately.
echo "$TOKEN" >"$STATE/active-token"
rm -f "$STATE/keep-token"
BB="$ROOT/busybox.static"
"$BB" nohup "$BB" setsid /system/bin/sh \
    /data/local/tmp/a26-xorg-safety-watchdog.sh "$TOKEN" 120 \
    >/dev/null 2>&1 </dev/null &

restore() {
    trap - EXIT HUP INT TERM
    /system/bin/sh /data/local/tmp/a26-android-graphics-restore.sh
}
trap restore EXIT HUP INT TERM

# SurfaceFlinger is a hard dependency of system_server's display and input
# services.  Leaving system_server alive while taking DRM master eventually
# triggers Samsung's platform watchdog and RescueParty.  Intentionally stop the
# two zygote services first; this cleanly suspends the Java framework while
# preserving init, adbd, Magisk, native HALs, and the USB transport.
echo "suspending Android Java framework"
echo 1 >"$STATE/framework-suspended"
/system/bin/stop zygote_secondary 2>/dev/null || true
/system/bin/stop zygote
for _ in $(seq 1 30); do
    [ -z "$(pidof system_server 2>/dev/null || true)" ] && break
    sleep 1
done
if [ -n "$(pidof system_server 2>/dev/null || true)" ]; then
    echo "system_server did not stop; refusing unsafe graphics handoff"
    exit 31
fi

# MTP/PTP are FunctionFS functions served by MediaProvider, which just stopped
# with the framework. With a daemonless function in the configuration the
# kernel unbinds the whole gadget on the next cable disconnect and only the
# framework would rebind it, so a replug while Moon runs would lose USB
# entirely. Keep only adb (and Samsung's kernel-side function) until Android
# resumes and its UsbDeviceManager reapplies the user's USB mode.
case "$(getprop sys.usb.config)" in
    *mtp*|*ptp*)
        echo "switching USB from $(getprop sys.usb.config) to adb-only"
        setprop sys.usb.config none
        for _ in $(seq 1 20); do
            [ "$(getprop sys.usb.state)" = none ] && break
            sleep 0.5
        done
        setprop sys.usb.config adb
        for _ in $(seq 1 20); do
            [ "$(getprop sys.usb.state)" = adb ] && break
            sleep 0.5
        done
        echo "USB state is $(getprop sys.usb.state)"
        ;;
esac

# A direct boot gets here with the boot animation still playing.
/system/bin/stop bootanim
/system/bin/stop surfaceflinger
sleep 2
/system/bin/stop vendor.hwcomposer-2-4
/system/bin/stop ExynosHWCServiceTW
sleep 3

if /system/bin/toybox lsof /dev/dri/card0 2>/dev/null | grep -q /dev/dri/card0; then
    echo "DRM card still open after graphics handoff"
    /system/bin/toybox lsof /dev/dri/card0 2>/dev/null || true
    exit 30
fi

echo "DRM card released; entering Alpine Xorg supervisor"
A26_ROOT="$ROOT" A26_BUSYBOX="$ROOT/busybox.static" \
    /system/bin/sh "$ROOT/a26-enter-chroot.sh" \
    /usr/local/sbin/a26-xorg-persistent-session
rc=$?
echo "Alpine Xorg supervisor returned rc=$rc"
exit "$rc"
