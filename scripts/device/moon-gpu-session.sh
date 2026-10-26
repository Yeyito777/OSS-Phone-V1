#!/system/bin/sh
# Moon's GPU display path (notes/gpu/DESIGN.md): starts, keeps and stops
# moon-compositor, the native process that owns DRM master and composites
# with the Mali GPU while Xorg runs headless.
#
#   moon-gpu-session.sh start   from a26-xorg-takeover.sh, with card0 free:
#                               exit 0 = GPU mode (start Xorg with --gpu),
#                               anything else = the software path
#   moon-gpu-session.sh keep    (internal) restarts a dead compositor; after
#                               repeated failures falls back to the software
#                               path in place
#   moon-gpu-session.sh stop    before Android takes the display back
#
# Opt-in: GPU_COMPOSITOR=1 in /data/adb/moon/config. A failure during this
# boot's start or keep disables GPU mode until the next boot.
set -u

ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
RUN=/data/local/tmp/moon-gpu
BIN="$ROOT/opt/moon-gpu/moon-compositor"
CONFIG=/data/adb/moon/config
XORG_STATE=/data/local/tmp/a26-xorg-control
LOG="$RUN/session.log"
MAX_RESTARTS=2

mkdir -p "$RUN"
chmod 0700 "$RUN"
log() { echo "moon-gpu: $* [up=$(cut -d' ' -f1 /proc/uptime)]" >>"$LOG"; }
boot_id="$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || echo unknown)"

# The PID in a pidfile, if it is still that program (never trust cmdline:
# reading it can block on a stuck process).
live_pid() {
    pid="$(cat "$1" 2>/dev/null || true)"
    case "$pid" in ''|*[!0-9]*) return 1 ;; esac
    exe="$("$BB" readlink "/proc/$pid/exe" 2>/dev/null || true)"
    case "$exe" in *"$2"*) echo "$pid" ;; *) return 1 ;; esac
}

start_compositor() {
    rm -f "$RUN/ready"
    "$BB" nohup "$BB" setsid "$BIN" --socket "$RUN/compositor.sock" \
        --ready-file "$RUN/ready" >>"$RUN/compositor.log" 2>&1 </dev/null &
    echo $! >"$RUN/compositor.pid"
    for _ in $("$BB" seq 1 40); do
        [ -e "$RUN/ready" ] && return 0
        live_pid "$RUN/compositor.pid" moon-compositor >/dev/null || return 1
        "$BB" sleep 0.1
    done
    return 1
}

stop_compositor() {
    pid="$(live_pid "$RUN/compositor.pid" moon-compositor)" || return 0
    kill -TERM "$pid" 2>/dev/null
    for _ in $("$BB" seq 1 20); do
        kill -0 "$pid" 2>/dev/null || break
        "$BB" sleep 0.1
    done
    kill -KILL "$pid" 2>/dev/null || true
    rm -f "$RUN/compositor.pid" "$RUN/ready"
}

case "${1:-}" in
start)
    grep -q '^GPU_COMPOSITOR=1' "$CONFIG" 2>/dev/null || exit 1
    if [ ! -x "$BIN" ]; then
        log "enabled but $BIN is missing; software path"
        exit 1
    fi
    if [ "$(cat "$RUN/failed-boot" 2>/dev/null)" = "$boot_id" ]; then
        log "failed earlier this boot; software path"
        exit 1
    fi
    rm -f "$RUN/active" "$RUN/fallback"
    : >"$RUN/compositor.log"
    if ! start_compositor; then
        log "compositor did not become ready; software path"
        tail -5 "$RUN/compositor.log" >>"$LOG" 2>/dev/null
        stop_compositor
        echo "$boot_id" >"$RUN/failed-boot"
        exit 1
    fi
    : >"$RUN/active"
    log "compositor ready pid=$(cat "$RUN/compositor.pid")"
    "$BB" nohup "$BB" setsid /system/bin/sh "$0" keep >/dev/null 2>&1 </dev/null &
    echo $! >"$RUN/keeper.pid"
    exit 0
    ;;
keep)
    restarts=0
    while [ -e "$RUN/active" ]; do
        "$BB" sleep 1
        [ -e "$RUN/active" ] || break
        live_pid "$RUN/compositor.pid" moon-compositor >/dev/null && continue
        restarts=$((restarts + 1))
        log "compositor exited ($restarts/$MAX_RESTARTS): $(tail -1 "$RUN/compositor.log" 2>/dev/null)"
        if [ "$restarts" -le "$MAX_RESTARTS" ] && start_compositor; then
            # Moon reconnects by itself and re-sends its layers.
            log "compositor restarted pid=$(cat "$RUN/compositor.pid")"
            continue
        fi
        # Fall back in place: Xorg leaves its headless session, the takeover
        # starts it again on the panel, and the supervisor restarts Moon on
        # the software path. Wi-Fi and audio are left running.
        log "giving up on GPU mode for this boot; software path"
        stop_compositor
        echo "$boot_id" >"$RUN/failed-boot"
        rm -f "$RUN/active"
        : >"$RUN/fallback"
        : >"$XORG_STATE/gpu-fallback"
        xpid="$(cat "$ROOT/run/a26-xorg.pid" 2>/dev/null || true)"
        case "$xpid" in ''|*[!0-9]*) ;; *) kill -TERM "$xpid" 2>/dev/null ;; esac
        break
    done
    ;;
stop)
    # The keeper ends within a second once `active` is gone.
    rm -f "$RUN/active" "$RUN/keeper.pid"
    stop_compositor
    log "stopped"
    ;;
*)
    echo "usage: $0 start|keep|stop" >&2
    exit 2
    ;;
esac
