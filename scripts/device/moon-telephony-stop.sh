#!/system/bin/sh
# Stops moon-telephonyd and its keeper (before Android's framework starts, so
# its phone process becomes rild's only client; and on uninstall). The daemon
# takes the mobile data interface down on SIGTERM.
set -u

RUNTIME=/data/local/tmp/moon-telephony
DAEMON=/data/local/a26-linux/opt/moon-telephony/moon-telephonyd

mkdir -p "$RUNTIME"
: >"$RUNTIME/stop"
set -- $(cat "$RUNTIME/keeper.pid" 2>/dev/null)
keeper="${1:-}" keeper_start="${2:-}"
# By recorded PID and /proc/PID/exe, never pidof (it reads every process's
# cmdline, which can block).
pid="$(cat "$RUNTIME/daemon.pid" 2>/dev/null || true)"
case "$pid" in ''|*[!0-9]*) pid='' ;; esac
if [ -n "$pid" ] && [ "$(readlink "/proc/$pid/exe" 2>/dev/null)" = "$DAEMON" ]; then
    kill -TERM "$pid" 2>/dev/null
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        [ "$(readlink "/proc/$pid/exe" 2>/dev/null)" = "$DAEMON" ] || break
        sleep 0.3
    done
    # Never leave it as a second rild client next to Android's phone process.
    [ "$(readlink "/proc/$pid/exe" 2>/dev/null)" = "$DAEMON" ] && kill -KILL "$pid" 2>/dev/null
fi
# The keeper exits by itself once the daemon is gone and the stop file is
# there; end it anyway if it is still the recorded process.
case "$keeper" in ''|*[!0-9]*) ;; *)
    [ "$(cut -d' ' -f22 "/proc/$keeper/stat" 2>/dev/null)" = "$keeper_start" ] &&
        kill -TERM "$keeper" 2>/dev/null ;;
esac
rm -f "$RUNTIME/keeper.pid" "$RUNTIME/daemon.pid"
echo "moon-telephonyd stopped"
