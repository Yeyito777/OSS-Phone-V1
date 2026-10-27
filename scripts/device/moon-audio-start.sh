#!/system/bin/sh
set -u

ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
PERSIST=/data/adb/moon
BASE="$ROOT/opt/a26-audio"
RUNTIME=/data/local/tmp/moon-audio
AUDIOD="$BASE/moon-audiod"
PERMISSIOND="$BASE/moon-permissiond"
LOG="$BASE/bridge.log"
PERMISSION_LOG="$BASE/permissiond.log"
PERMISSION_PIDFILE="$RUNTIME/permissiond.pid"
PIDFILE="$RUNTIME/bridge.pid"
MEDIA_HEARTBEAT="$RUNTIME/browser-media-active"

# Pidfiles live on /data and outlive reboots, so a recorded PID can belong to
# any process now. Identify it by /proc/PID/exe, never /proc/PID/cmdline:
# reading cmdline takes the target's mmap lock and blocks for as long as that
# process is stuck, which is how one boot's audio start hung for ~245 s.
pid_runs() {
    case "$2" in ''|*[!0-9]*) return 1 ;; esac
    [ "$(readlink "/proc/$2/exe" 2>/dev/null)" = "$1" ]
}
valid_bridge_pid() {
    pid="$(cat "$PIDFILE" 2>/dev/null || true)"
    pid_runs "$AUDIOD" "$pid"
}

valid_permissiond_pid() {
    ppid="$(cat "$PERMISSION_PIDFILE" 2>/dev/null || true)"
    pid_runs "$PERMISSIOND" "$ppid"
}

[ -x "$BB" ] || { echo "Moon BusyBox is unavailable" >&2; exit 20; }
[ -x "$AUDIOD" ] || { echo "Moon audio daemon is unavailable" >&2; exit 21; }
[ -x "$PERMISSIOND" ] || { echo "Moon permission service is unavailable" >&2; exit 21; }
# init's service state, not pidof: pidof reads every process's cmdline.
[ "$(getprop init.svc.audioserver)" = running ] || {
    echo "audioserver is not running" >&2
    exit 22
}
mkdir -p "$RUNTIME" "$BASE"

# AudioFlinger resolves every new track's package through system_server's
# "permission" service and aborts audioserver after a 10 s wait when it is
# missing. Without the Java framework, register Moon's native stand-in (it
# replaces a stale registration of a stopped system_server); while the
# framework runs, its real service is used instead.
if [ "$(getprop init.svc.zygote)" != running ] && ! valid_permissiond_pid; then
    rm -f "$PERMISSION_PIDFILE"
    "$BB" nohup "$BB" setsid "$PERMISSIOND" "$PERMISSION_PIDFILE" \
        >"$PERMISSION_LOG" 2>&1 </dev/null &
    for _ in $($BB seq 1 25); do
        valid_permissiond_pid && break
        "$BB" sleep 0.2
    done
    valid_permissiond_pid || {
        echo "Moon permission service failed to start" >&2
        cat "$PERMISSION_LOG" >&2 2>/dev/null || true
        exit 25
    }
fi

if valid_bridge_pid && kill -0 "$pid" 2>/dev/null; then
    echo "moon audio bridge already running pid=$pid"
    exit 0
fi
rm -f "$PIDFILE" "$MEDIA_HEARTBEAT"

rm -f "$RUNTIME/pcm" "$RUNTIME/alerts"
mknod "$RUNTIME/pcm" p || exit 23
# moon-notifyd's alarm, timer and notification sounds, played as a second
# (ALARM) stream; root-only, like the daemon that writes it.
mknod "$RUNTIME/alerts" p || exit 23
chmod 0600 "$RUNTIME/alerts"
# The live volume file belongs to Moon, which may already be running (audio
# is never waited for at boot) and owns the user's value. Never rewrite a valid
# one: a truncate-and-write here once raced Moon's startup read and reset the
# volume to Moon's default. Seed it only when missing or invalid, atomically.
valid_volume() {
    case "$1" in ''|*[!0-9]*) return 1 ;; esac
    [ "$1" -le 100 ]
}
volume="$(cat "$RUNTIME/volume" 2>/dev/null || true)"
if ! valid_volume "$volume"; then
    volume="$(cat "$PERSIST/volume" 2>/dev/null || true)"
    valid_volume "$volume" || volume=50
    printf '%s\n' "$volume" >"$RUNTIME/volume.new.$$"
    mv -f "$RUNTIME/volume.new.$$" "$RUNTIME/volume"
fi
chown 1000:1000 "$RUNTIME" "$RUNTIME/pcm" "$RUNTIME/volume"
chmod 0770 "$RUNTIME"
chmod 0660 "$RUNTIME/pcm" "$RUNTIME/volume"
: >"$LOG"
chmod 0640 "$LOG"

# Runs as root: audioserver exempts service UIDs from AppOps, which is part of
# system_server too. The music stream's speaker index is the loudness ceiling.
stream_index="$(sed -n 's/^AUDIO_STREAM_INDEX=//p' "$PERSIST/config" 2>/dev/null | tail -1)"
case "$stream_index" in ''|*[!0-9]*) stream_index=5 ;; esac
MOON_AUDIO_STREAM_INDEX="$stream_index" "$BB" nohup "$BB" setsid "$AUDIOD" \
    "$RUNTIME/pcm" "$RUNTIME/volume" "$PIDFILE" "$MEDIA_HEARTBEAT" "$RUNTIME/alerts" \
    >>"$LOG" 2>&1 </dev/null &
launcher=$!

for _ in $($BB seq 1 150); do
    if valid_bridge_pid && kill -0 "$pid" 2>/dev/null &&
       grep -q 'moon audio bridge ready' "$LOG" 2>/dev/null; then
        echo "moon audio bridge ready pid=$pid"
        exit 0
    fi
    if ! kill -0 "$launcher" 2>/dev/null; then
        break
    fi
    "$BB" sleep 0.2
done

kill "$launcher" 2>/dev/null || true
valid_bridge_pid && kill "$pid" 2>/dev/null || true
echo 'Moon audio bridge failed to become ready' >&2
cat "$LOG" >&2 2>/dev/null || true
exit 24
