#!/system/bin/sh
set -u

ROOT=/data/local/a26-linux
PERSIST=/data/adb/moon
RUNTIME=/data/local/tmp/moon-audio
PIDFILE="$RUNTIME/bridge.pid"
PERMISSION_PIDFILE="$RUNTIME/permissiond.pid"
BASE="$ROOT/opt/a26-audio"

if [ -r "$RUNTIME/volume" ]; then
    volume="$(cat "$RUNTIME/volume" 2>/dev/null || true)"
    case "$volume" in
        ''|*[!0-9]*) ;;
        *)
            [ "$volume" -le 100 ] 2>/dev/null || volume=100
            printf '%s\n' "$volume" >"$PERSIST/volume.new.$$"
            chown 0:0 "$PERSIST/volume.new.$$"
            chmod 0600 "$PERSIST/volume.new.$$"
            mv -f "$PERSIST/volume.new.$$" "$PERSIST/volume"
            ;;
    esac
fi

pid="$(cat "$PIDFILE" 2>/dev/null || true)"
case "$pid" in ''|*[!0-9]*) pid='' ;; esac
if [ -n "$pid" ] && [ -r "/proc/$pid/cmdline" ] &&
   tr '\000' ' ' <"/proc/$pid/cmdline" 2>/dev/null |
       grep -Eq "^$BASE/moon-audiod |moon\.audio\.Bridge"; then
    kill -TERM "$pid" 2>/dev/null || true
    for _ in $(seq 1 30); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
    done
    kill -KILL "$pid" 2>/dev/null || true
fi

# The native permission stand-in must be gone before the Java framework
# starts, so system_server's own "permission" service is the only one.
pid="$(cat "$PERMISSION_PIDFILE" 2>/dev/null || true)"
case "$pid" in ''|*[!0-9]*) pid='' ;; esac
if [ -n "$pid" ] && [ -r "/proc/$pid/cmdline" ] &&
   tr '\000' ' ' <"/proc/$pid/cmdline" 2>/dev/null | grep -q "^$BASE/moon-permissiond "; then
    kill -KILL "$pid" 2>/dev/null || true
fi

rm -f "$PIDFILE" "$PERMISSION_PIDFILE" "$RUNTIME/pcm" "$RUNTIME/browser-media-active"
echo 'moon audio bridge stopped'
