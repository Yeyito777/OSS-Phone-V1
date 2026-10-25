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

valid_bridge_pid() {
    pid="$(cat "$PIDFILE" 2>/dev/null || true)"
    case "$pid" in ''|*[!0-9]*) return 1 ;; esac
    [ -r "/proc/$pid/cmdline" ] || return 1
    tr '\000' ' ' <"/proc/$pid/cmdline" 2>/dev/null | grep -q "^$AUDIOD "
}

valid_permissiond_pid() {
    ppid="$(cat "$PERMISSION_PIDFILE" 2>/dev/null || true)"
    case "$ppid" in ''|*[!0-9]*) return 1 ;; esac
    [ -r "/proc/$ppid/cmdline" ] || return 1
    tr '\000' ' ' <"/proc/$ppid/cmdline" 2>/dev/null | grep -q "^$PERMISSIOND "
}

[ -x "$BB" ] || { echo "Moon BusyBox is unavailable" >&2; exit 20; }
[ -x "$AUDIOD" ] || { echo "Moon audio daemon is unavailable" >&2; exit 21; }
[ -x "$PERMISSIOND" ] || { echo "Moon permission service is unavailable" >&2; exit 21; }
[ -n "$(pidof audioserver 2>/dev/null || true)" ] || {
    echo "audioserver is not running" >&2
    exit 22
}
mkdir -p "$RUNTIME" "$BASE"

# AudioFlinger resolves every new track's package through system_server's
# "permission" service and aborts audioserver after a 10 s wait when it is
# missing. Without the Java framework, register Moon's native stand-in (it
# replaces a stale registration of a stopped system_server); while the
# framework runs, its real service is used instead.
if [ -z "$(pidof system_server 2>/dev/null || true)" ] && ! valid_permissiond_pid; then
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

rm -f "$RUNTIME/pcm"
mknod "$RUNTIME/pcm" p || exit 23
if [ -r "$PERSIST/volume" ]; then
    volume="$(cat "$PERSIST/volume" 2>/dev/null || echo 50)"
else
    volume=50
fi
case "$volume" in ''|*[!0-9]*) volume=50 ;; esac
[ "$volume" -le 100 ] 2>/dev/null || volume=100
printf '%s\n' "$volume" >"$RUNTIME/volume"
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
    "$RUNTIME/pcm" "$RUNTIME/volume" "$PIDFILE" "$MEDIA_HEARTBEAT" \
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
