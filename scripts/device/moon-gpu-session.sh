#!/system/bin/sh
# Moon's GPU display path (notes/gpu/DESIGN.md): starts, keeps and stops
# moon-compositor, the native process that owns DRM master and composites
# with the Mali GPU while Xorg runs headless.
#
#   moon-gpu-session.sh start   from a26-xorg-takeover.sh, with card0 free:
#                               exit 0 = GPU mode (start Xorg with --gpu),
#                               anything else = the software path
#   moon-gpu-session.sh keep    (internal) runs the compositor and waits
#                               for it; restarts it when it exits, and after
#                               repeated failures falls back to the software
#                               path in place
#   moon-gpu-session.sh stop    before Android takes the display back
#
# The default; GPU_COMPOSITOR=0 in /data/adb/moon/config (scripts/gpu/install.sh
# --disable) opts out. A missing piece, or a failure during this
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

# GPU_FLOOR_KHZ (config): the GPU clock floor while frames flow.
floor="$(sed -n 's/^GPU_FLOOR_KHZ=\([0-9]*\)$/\1/p' "$CONFIG" 2>/dev/null | tail -1)"
# GPU_PLANES=0 (config, scripts/gpu/install.sh --no-planes): the GPU composes
# every frame; Moon's own buffers never go to display planes.
no_planes=
grep -q '^GPU_PLANES=0' "$CONFIG" 2>/dev/null && no_planes=--no-planes

stop_compositor() {
    pid="$(live_pid "$RUN/compositor.pid" moon-compositor)" || {
        rm -f "$RUN/compositor.pid" "$RUN/ready"
        return 0
    }
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
    # Whatever happens next, nothing from an earlier boot may claim GPU mode
    # (the supervisor gives Moon the compositor's socket only while `active`
    # names this boot).
    rm -f "$RUN/active" "$RUN/fallback" "$RUN/ready"
    grep -q '^GPU_COMPOSITOR=0' "$CONFIG" 2>/dev/null && exit 1
    # Everything GPU mode needs, or the software path: a headless Xorg that
    # cannot start would end the session.
    for piece in "$BIN" "$ROOT/usr/lib/xorg/modules/drivers/dummy_drv.so" \
        "$ROOT/etc/X11/xorg-gpu.conf.d/20-a26-gpu.conf" \
        "$ROOT/etc/X11/xorg-gpu.conf.d/30-a26-input.conf"; do
        if [ ! -e "$piece" ]; then
            log "$piece is missing; software path"
            exit 1
        fi
    done
    # Deep suspend hands the display off through Xorg (xrandr --off), which
    # a headless Xorg cannot do.
    if grep -q '"deep_suspend": *true' "$ROOT/etc/a26-shell/config.json" 2>/dev/null; then
        log "deep suspend is enabled; software path"
        exit 1
    fi
    if [ "$(cat "$RUN/failed-boot" 2>/dev/null)" = "$boot_id" ]; then
        log "failed earlier this boot; software path"
        exit 1
    fi
    : >"$RUN/compositor.log"
    echo "$boot_id" >"$RUN/active"
    # The keeper runs the compositor as its child, so it can simply wait
    # for it: nothing polls while the phone idles.
    "$BB" nohup "$BB" setsid /system/bin/sh "$0" keep >/dev/null 2>&1 </dev/null &
    echo $! >"$RUN/keeper.pid"
    for _ in $("$BB" seq 1 40); do
        if [ -e "$RUN/ready" ]; then
            log "compositor ready pid=$(cat "$RUN/compositor.pid" 2>/dev/null)"
            exit 0
        fi
        [ -e "$RUN/active" ] || break
        "$BB" sleep 0.1
    done
    log "compositor did not become ready; software path"
    tail -5 "$RUN/compositor.log" >>"$LOG" 2>/dev/null
    rm -f "$RUN/active"
    stop_compositor
    echo "$boot_id" >"$RUN/failed-boot"
    exit 1
    ;;
keep)
    restarts=0
    while [ -e "$RUN/active" ]; do
        rm -f "$RUN/ready"
        "$BIN" --socket "$RUN/compositor.sock" --ready-file "$RUN/ready" \
            ${floor:+--gpu-floor "$floor"} $no_planes >>"$RUN/compositor.log" 2>&1 </dev/null &
        pid=$!
        echo "$pid" >"$RUN/compositor.pid"
        # The compositor watches itself: it exits if its loop stops turning
        # (a GPU or display hang) or the display stops flipping.
        wait "$pid"
        status=$?
        [ -e "$RUN/active" ] || break
        restarts=$((restarts + 1))
        log "compositor exited with $status ($restarts/$MAX_RESTARTS): $(tail -1 "$RUN/compositor.log" 2>/dev/null)"
        if [ "$restarts" -le "$MAX_RESTARTS" ]; then
            # Moon reconnects by itself and re-sends its layers.
            log "restarting the compositor"
            continue
        fi
        # Fall back in place: Xorg leaves its headless session, the takeover
        # starts it again on the panel, and the supervisor restarts Moon on
        # the software path. Wi-Fi and audio are left running.
        log "giving up on GPU mode for this boot; software path"
        rm -f "$RUN/compositor.pid" "$RUN/ready"
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
