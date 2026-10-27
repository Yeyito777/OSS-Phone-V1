#!/system/bin/sh
set -u

ROOT=/data/local/a26-linux
STATE=/data/local/tmp/a26-xorg-control
LOG="$STATE/takeover.log"
TOKEN="${1:-xorg-takeover-$(date +%s)-$$}"
mkdir -p "$STATE"
exec </dev/null >>"$LOG" 2>&1

uptime_now() { cut -d' ' -f1 /proc/uptime; }
echo "takeover begin token=$TOKEN $(date --iso-8601=seconds 2>/dev/null || date) up=$(uptime_now)"

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

# Wait (100 ms polls, bounded) until init has reaped every named service.
wait_stopped() {
    limit=$1
    shift
    for _ in $(seq 1 "$limit"); do
        pending=''
        for service in "$@"; do
            case "$(getprop "init.svc.$service")" in
                stopped|'') ;;
                *) pending=1 ;;
            esac
        done
        [ -n "$pending" ] || return 0
        sleep 0.1
    done
    echo "still stopping after $((limit / 10)) s: $*"
}
card_in_use() {
    /system/bin/toybox lsof /dev/dri/card0 2>/dev/null | grep -q /dev/dri/card0
}

# A direct boot gets here with the boot animation still playing. SurfaceFlinger
# must be gone before its composer is stopped.
/system/bin/stop bootanim
/system/bin/stop surfaceflinger
wait_stopped 20 surfaceflinger bootanim
/system/bin/stop vendor.hwcomposer-2-4
/system/bin/stop ExynosHWCServiceTW
wait_stopped 30 vendor.hwcomposer-2-4 ExynosHWCServiceTW
for _ in $(seq 1 10); do
    card_in_use || break
    sleep 0.2
done
echo "graphics services stopped up=$(uptime_now)"

if card_in_use; then
    echo "DRM card still open after graphics handoff"
    /system/bin/toybox lsof /dev/dri/card0 2>/dev/null || true
    exit 30
fi

echo "DRM card released; entering Alpine Xorg supervisor up=$(uptime_now)"
# GPU mode (opt-in, notes/gpu/DESIGN.md): moon-compositor takes DRM master and
# Xorg runs headless. Anything short of a ready compositor is the software
# path, Xorg on the panel, exactly as before.
GPU_SESSION=/data/adb/moon/moon-gpu-session.sh
# Xwayland instead of Xorg (XWAYLAND=1 in /data/adb/moon/config, milestone 4,
# notes/wayland/MOON-WAYLAND.md): only on the GPU path, only with Xwayland in
# the chroot, and never again this boot once Moon fell back from it.
XWAYLAND_FALLBACK=/data/local/tmp/moon-gpu/xwayland-fallback
session_arg=''
rm -f "$STATE/gpu-fallback" "$XWAYLAND_FALLBACK"
if [ -x "$GPU_SESSION" ] && /system/bin/sh "$GPU_SESSION" start; then
    session_arg=--gpu
    echo "GPU compositor ready up=$(uptime_now)"
    if [ "$(sed -n 's/^XWAYLAND=//p' /data/adb/moon/config 2>/dev/null | tail -1)" = 1 ] &&
        [ -x "$ROOT/usr/bin/Xwayland" ]; then
        session_arg=--xwayland
        echo "X server: Moon's Xwayland up=$(uptime_now)"
    fi
fi
while :; do
    A26_ROOT="$ROOT" A26_BUSYBOX="$ROOT/busybox.static" \
        /system/bin/sh "$ROOT/a26-enter-chroot.sh" \
        /usr/local/sbin/a26-xorg-persistent-session $session_arg
    rc=$?
    echo "Alpine Xorg supervisor returned rc=$rc"
    # The GPU path gave up at runtime (moon-gpu-session.sh keep): run the same
    # session again on the panel instead of restoring Android.
    if [ -n "$session_arg" ] && [ -e /data/local/tmp/moon-gpu/fallback ]; then
        echo "GPU compositor failed; restarting Xorg on the panel up=$(uptime_now)"
        session_arg=''
        continue
    fi
    # Moon could not run on Xwayland (moon-boot-supervisor.sh): the same GPU
    # session with headless Xorg, as without XWAYLAND.
    if [ "$session_arg" = --xwayland ] && [ -e "$XWAYLAND_FALLBACK" ]; then
        echo "Moon failed on Xwayland; starting headless Xorg up=$(uptime_now)"
        session_arg=--gpu
        continue
    fi
    break
done
exit "$rc"
