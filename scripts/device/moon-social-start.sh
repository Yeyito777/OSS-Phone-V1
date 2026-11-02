#!/system/bin/sh
# Starts the social bridges (Moon mode only): one record service per social
# app, each the bridge between its service and its app (a26-system
# docs/social-protocol.md), under a small keeper that restarts each after a
# crash, with backoff. Runs in Android's root namespace and runs the bridges
# in the chroot. moon-social-stop.sh ends them.
#
# Which run: /etc/moon-social/services in the chroot (service names, one a
# line; default discord, instagram, whatsapp); a service's extra arguments
# (`--provider-module PATH`) in /etc/moon-social/SERVICE.args. Logs:
# /var/log/moon-social/SERVICE.log in the chroot.
set -u

ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
BUN=/usr/local/bin/bun
SERVICE_MAIN=/opt/moon-social/record/src/service/main.ts
RUNTIME=/data/local/tmp/moon-social
PIDFILE="$RUNTIME/keeper.pid"
CHROOT_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

[ -r "$ROOT$SERVICE_MAIN" ] || { echo "the social bridges are not installed"; exit 0; }
[ -x "$BB" ] || { echo "Moon BusyBox is unavailable" >&2; exit 20; }
mkdir -p "$RUNTIME" "$ROOT/var/log/moon-social"
chmod 0700 "$RUNTIME" "$ROOT/var/log/moon-social"

# The pidfile holds "PID STARTTIME" (/proc/PID/stat field 22), so a reused
# PID never matches; /proc/PID/cmdline is never read (it can block).
alive() {
    set -- $(cat "$1" 2>/dev/null)
    case "${1:-}" in ''|*[!0-9]*) return 1 ;; esac
    [ "$(cut -d' ' -f22 "/proc/$1/stat" 2>/dev/null)" = "${2:-}" ]
}
if alive "$PIDFILE"; then
    echo "moon-social keeper already running"
    exit 0
fi

services="$(sed -e 's/#.*//' "$ROOT/etc/moon-social/services" 2>/dev/null | tr -s ' \n' ' ')"
[ -n "${services# }" ] || services='discord instagram whatsapp'
rm -f "$RUNTIME/stop"

# The keeper: a BusyBox sh with one loop per bridge. It leaves whatever
# cgroup started it (an init service, a shell's) for the root one, and its
# own session, so nothing that ends its starter takes the bridges along.
"$BB" setsid "$BB" sh -c '
    echo $$ >/sys/fs/cgroup/cgroup.procs 2>/dev/null
    echo "$$ $(cut -d" " -f22 /proc/$$/stat)" >"$1"
    ROOT=$2 BB=$3 BUN=$4 MAIN=$5 CHROOT_PATH=$6 RUNTIME=$7
    shift 7
    keep() {
        name=$1
        log="$ROOT/var/log/moon-social/$name.log"
        backoff=2
        while [ ! -e "$RUNTIME/stop" ] && [ ! -e /data/adb/moon/disabled ] &&
            [ "$(getprop moon.boot.mode)" != android ] && [ "$(getprop moon.boot.mode)" != shutdown ]; do
            if [ -f "$log" ] && [ "$("$BB" stat -c %s "$log" 2>/dev/null || echo 0)" -gt 1048576 ]; then
                mv -f "$log" "$log.1"
            fi
            args="$(cat "$ROOT/etc/moon-social/$name.args" 2>/dev/null)"
            started=$(cut -d. -f1 /proc/uptime)
            # One GC marker thread: with bun'"'"'s default eight, every socket
            # read spun them (~90 ms of CPU and ~1000 context switches each
            # on the phone: a Discord gateway alone cost 12-15 % of a core).
            # The system allocator (Malloc=1): bmalloc'"'"'s scavenger woke
            # 10 times a second forever (an idle bridge: 11 wakeups/s and
            # 310 ms CPU a minute, 0.9/s and 20 ms with it; sync time equal).
            "$BB" nice -n 5 "$BB" chroot "$ROOT" /usr/bin/env -i HOME=/root PATH="$CHROOT_PATH" \
                BUN_JSC_numberOfGCMarkers=1 Malloc=1 \
                "$BUN" "$MAIN" --provider "$name" $args </dev/null >>"$log" 2>&1 &
            pid=$!
            echo "$pid $(cut -d" " -f22 /proc/$pid/stat 2>/dev/null)" >"$RUNTIME/$name.pid"
            wait $pid
            rc=$?
            rm -f "$RUNTIME/$name.pid"
            [ -e "$RUNTIME/stop" ] && break
            ran=$(( $(cut -d. -f1 /proc/uptime) - started ))
            [ "$ran" -lt 120 ] || backoff=2
            echo "moon-social-keeper: $name exited rc=$rc after ${ran}s; restart in ${backoff}s" >>"$log"
            sleep "$backoff"
            [ "$backoff" -ge 60 ] || backoff=$((backoff * 2))
        done
    }
    for name in "$@"; do
        keep "$name" &
    done
    wait
    rm -f "$RUNTIME/keeper.pid"
' moon-social-keeper "$PIDFILE" "$ROOT" "$BB" "$BUN" "$SERVICE_MAIN" "$CHROOT_PATH" "$RUNTIME" $services \
    </dev/null >/dev/null 2>&1 &
echo "moon-social started:$services"
