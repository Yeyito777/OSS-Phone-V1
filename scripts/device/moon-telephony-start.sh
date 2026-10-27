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

# The pidfile holds "PID STARTTIME" (/proc/PID/stat field 22), so a reused
# PID never matches. /proc/PID/cmdline is never read: it takes the target's
# mmap lock and can block (notes/direct-boot/RESEARCH.md).
keeper_alive() {
    set -- $(cat "$PIDFILE" 2>/dev/null)
    case "${1:-}" in ''|*[!0-9]*) return 1 ;; esac
    [ "$(cut -d' ' -f22 "/proc/$1/stat" 2>/dev/null)" = "${2:-}" ]
}
if keeper_alive; then
    echo "moon-telephonyd keeper already running"
    exit 0
fi

# A daemon left without its keeper (keeper killed) would fight a new one
# for rild's callbacks and the socket: end it first.
pid="$(cat "$RUNTIME/daemon.pid" 2>/dev/null || true)"
case "$pid" in ''|*[!0-9]*) ;; *)
    if [ "$(readlink "/proc/$pid/exe" 2>/dev/null)" = "$DAEMON" ]; then
        kill -TERM "$pid" 2>/dev/null
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            [ "$(readlink "/proc/$pid/exe" 2>/dev/null)" = "$DAEMON" ] || break
            sleep 0.3
        done
        [ "$(readlink "/proc/$pid/exe" 2>/dev/null)" = "$DAEMON" ] && kill -KILL "$pid" 2>/dev/null
    fi ;;
esac

if [ -f "$LOG" ] && [ "$("$BB" stat -c %s "$LOG" 2>/dev/null || echo 0)" -gt 262144 ]; then
    mv -f "$LOG" "$LOG.1"
fi
rm -f "$RUNTIME/stop"

# The keeper: a BusyBox sh loop that restarts the daemon with backoff.
"$BB" setsid "$BB" sh -c '
    echo "$$ $(cut -d" " -f22 /proc/$$/stat)" >"$3"
    backoff=2
    while [ ! -e /data/local/tmp/moon-telephony/stop ] && [ ! -e /data/adb/moon/disabled ] &&
        [ "$(getprop moon.boot.mode)" != android ]; do
        started=$(cut -d. -f1 /proc/uptime)
        "$1" >>"$2" 2>&1 </dev/null &
        echo $! >/data/local/tmp/moon-telephony/daemon.pid
        wait $!
        rc=$?
        [ -e /data/local/tmp/moon-telephony/stop ] && break
        ran=$(( $(cut -d. -f1 /proc/uptime) - started ))
        [ "$ran" -lt 60 ] || backoff=2
        echo "moon-telephony-keeper: daemon exited rc=$rc after ${ran}s; restart in ${backoff}s" >>"$2"
        sleep "$backoff"
        [ "$backoff" -ge 60 ] || backoff=$((backoff * 2))
    done
    rm -f "$3" /data/local/tmp/moon-telephony/daemon.pid
' moon-telephony-keeper "$DAEMON" "$LOG" "$PIDFILE" </dev/null >/dev/null 2>&1 &
echo "moon-telephonyd started"
