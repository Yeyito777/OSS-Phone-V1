#!/system/bin/sh
# init service moon-session: run Moon's supervisor in direct-boot mode and
# fall back to Android if it is missing, exits while Moon still owns the boot,
# or has not reported Moon ready (moon.session.ready=1) within the deadline.
SUPERVISOR=/data/adb/moon/moon-boot-supervisor.sh
FALLBACK=/debug_ramdisk/moon-fallback.sh
READY_DEADLINE=240

if [ ! -r "$SUPERVISOR" ]; then
    exec /system/bin/sh "$FALLBACK" supervisor-missing
fi
setprop moon.session.ready 0
/system/bin/sh "$SUPERVISOR" --direct &
supervisor=$!

waited=0
while [ "$(getprop moon.session.ready)" != 1 ]; do
    if ! kill -0 "$supervisor" 2>/dev/null; then
        wait "$supervisor"
        exec /system/bin/sh "$FALLBACK" supervisor-exited-before-ready
    fi
    if [ "$(getprop moon.boot.mode)" != moon ]; then
        # The supervisor handed the boot to Android itself (volume key).
        wait "$supervisor"
        exit 0
    fi
    if [ "$waited" -ge "$READY_DEADLINE" ]; then
        kill -TERM "$supervisor" 2>/dev/null
        exec /system/bin/sh "$FALLBACK" ready-deadline
    fi
    sleep 1
    waited=$((waited + 1))
done

wait "$supervisor"
exec /system/bin/sh "$FALLBACK" supervisor-exited
