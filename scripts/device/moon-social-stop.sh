#!/system/bin/sh
# Stops the social bridges and their keeper (moon-social-start.sh): at
# power-off, when Android takes the phone back, and to update them. A bridge
# saves its chat list and closes its sessions on SIGTERM.
set -u

RUNTIME=/data/local/tmp/moon-social

mkdir -p "$RUNTIME"
: >"$RUNTIME/stop"

# By recorded PID and start time, never pidof or a command line.
recorded() {
    set -- $(cat "$1" 2>/dev/null)
    case "${1:-}" in ''|*[!0-9]*) return 1 ;; esac
    [ "$(cut -d' ' -f22 "/proc/$1/stat" 2>/dev/null)" = "${2:-}" ] && echo "$1"
}

pids=''
for file in "$RUNTIME"/*.pid; do
    [ "$file" = "$RUNTIME/keeper.pid" ] && continue
    pid="$(recorded "$file")" && pids="$pids $pid"
done
[ -z "$pids" ] || kill -TERM $pids 2>/dev/null
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do
    left=''
    for pid in $pids; do
        [ -d "/proc/$pid" ] && left="$left $pid"
    done
    [ -z "$left" ] && break
    sleep 0.2
done
[ -z "${left:-}" ] || kill -KILL $left 2>/dev/null

keeper="$(recorded "$RUNTIME/keeper.pid")" && kill -TERM "$keeper" 2>/dev/null
rm -f "$RUNTIME"/*.pid
echo "moon-social stopped"
