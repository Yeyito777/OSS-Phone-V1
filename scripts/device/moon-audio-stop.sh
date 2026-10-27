#!/system/bin/sh
set -u

ROOT=/data/local/a26-linux
PERSIST=/data/adb/moon
RUNTIME=/data/local/tmp/moon-audio
PIDFILE="$RUNTIME/bridge.pid"
PERMISSION_PIDFILE="$RUNTIME/permissiond.pid"
BASE="$ROOT/opt/a26-audio"

# PIDs come from pidfiles that outlive reboots: identify them by /proc/PID/exe,
# which unlike /proc/PID/cmdline never blocks on a stuck process.
pid_runs() {
    case "$2" in ''|*[!0-9]*) return 1 ;; esac
    [ "$(readlink "/proc/$2/exe" 2>/dev/null)" = "$1" ]
}

# First the keeper, so it cannot start audio again behind us.
keeper="$(cat "$RUNTIME/keeper.pid" 2>/dev/null || true)"
if pid_runs /system/bin/sh "$keeper"; then
    kill -TERM "$keeper" 2>/dev/null || true
fi
rm -f "$RUNTIME/keeper.pid"

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
if pid_runs "$BASE/moon-audiod" "$pid"; then
    kill -TERM "$pid" 2>/dev/null || true
    for _ in $(seq 1 30); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 1
    done
    kill -KILL "$pid" 2>/dev/null || true
fi

# The native stand-ins ("permission", "audio") must be gone before the Java
# framework starts, so system_server's own services are the only ones.
pid="$(cat "$PERMISSION_PIDFILE" 2>/dev/null || true)"
if pid_runs "$BASE/moon-permissiond" "$pid"; then
    kill -KILL "$pid" 2>/dev/null || true
    # AudioFlinger keeps the first "audio" service it found for good; restart
    # audioserver (not a crash: init stop/start) so it binds to the
    # framework's real AudioService instead of the stand-in's dead binder.
    if [ "$(getprop init.svc.zygote)" != running ]; then
        /system/bin/stop audioserver 2>/dev/null || true
        /system/bin/start audioserver 2>/dev/null || true
    fi
fi

rm -f "$PIDFILE" "$PERMISSION_PIDFILE" "$RUNTIME/pcm" "$RUNTIME/browser-media-active"
echo 'moon audio bridge stopped'
