#!/system/bin/sh
# Starts moon-telephonyd (Moon mode only) under a small keeper that restarts
# it after a crash, with backoff. Runs in Android's root namespace: the daemon
# talks to rild over /dev/hwbinder and execs the bionic call-audio helper; its
# socket reaches the chroot through the /run/moon-telephony bind mount
# (a26-enter-chroot.sh). moon-telephony-stop.sh ends it.
set -u

ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
DAEMON="$ROOT/opt/moon-telephony/moon-telephonyd"
RUNTIME=/data/local/tmp/moon-telephony
PIDFILE="$RUNTIME/keeper.pid"
LOG=/data/adb/moon/telephony.log

[ -x "$DAEMON" ] || { echo "moon-telephonyd is not installed"; exit 0; }
[ -x "$BB" ] || { echo "Moon BusyBox is unavailable" >&2; exit 20; }
mkdir -p "$RUNTIME"

keeper="$(cat "$PIDFILE" 2>/dev/null || true)"
case "$keeper" in ''|*[!0-9]*) ;; *)
    if grep -q moon-telephony-keeper "/proc/$keeper/cmdline" 2>/dev/null; then
        echo "moon-telephonyd keeper already running pid=$keeper"
        exit 0
    fi ;;
esac

if [ -f "$LOG" ] && [ "$("$BB" stat -c %s "$LOG" 2>/dev/null || echo 0)" -gt 262144 ]; then
    mv -f "$LOG" "$LOG.1"
fi
rm -f "$RUNTIME/stop"

# The keeper is a BusyBox sh; its argv carries a marker for the check above.
"$BB" setsid "$BB" sh -c '
    echo $$ >"$3"
    backoff=2
    while [ ! -e /data/adb/moon/disabled ] && [ "$(getprop moon.boot.mode)" != android ]; do
        started=$(cut -d. -f1 /proc/uptime)
        "$1" >>"$2" 2>&1 </dev/null
        rc=$?
        [ -e /data/local/tmp/moon-telephony/stop ] && break
        ran=$(( $(cut -d. -f1 /proc/uptime) - started ))
        [ "$ran" -lt 60 ] || backoff=2
        echo "moon-telephony-keeper: daemon exited rc=$rc after ${ran}s; restart in ${backoff}s" >>"$2"
        sleep "$backoff"
        [ "$backoff" -ge 60 ] || backoff=$((backoff * 2))
    done
    rm -f "$3"
' moon-telephony-keeper "$DAEMON" "$LOG" "$PIDFILE" </dev/null >/dev/null 2>&1 &
echo "moon-telephonyd started"
