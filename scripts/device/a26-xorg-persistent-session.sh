#!/bin/sh
set -u

LOG=/root/Xorg.persistent.log
STATUS=/root/xorg-session.status
# --gpu: moon-compositor owns the panel; Xorg runs headless (dummy driver) and
# reaches the screen through Moon's X mirror (notes/gpu/DESIGN.md).
CONFIGDIR=/etc/X11/xorg.conf.d
[ "${1:-}" != --gpu ] || CONFIGDIR=/etc/X11/xorg-gpu.conf.d
rm -f /tmp/.X0-lock /tmp/.X11-unix/X0 "$STATUS"
mkdir -p /tmp/.X11-unix /run

xpid=''
cleanup() {
    trap - EXIT HUP INT TERM
    # A GPU-to-software fallback replaces only Xorg: keep Wi-Fi up.
    if [ -x /opt/a26-system/libexec/a26-wifi-stop ] && [ ! -e /run/moon-gpu/fallback ]; then
        /opt/a26-system/libexec/a26-wifi-stop 2>/dev/null || true
    fi
    if [ -n "$xpid" ] && kill -0 "$xpid" 2>/dev/null; then
        kill -TERM "$xpid" 2>/dev/null || true
        for _ in $(seq 1 10); do
            kill -0 "$xpid" 2>/dev/null || break
            sleep 1
        done
        kill -KILL "$xpid" 2>/dev/null || true
        wait "$xpid" 2>/dev/null || true
    fi
    rm -f /run/a26-xorg.pid
}
trap cleanup EXIT HUP INT TERM

Xorg :0 \
    -configdir "$CONFIGDIR" \
    -logfile "$LOG" \
    -novtswitch -sharevts -keeptty -noreset -nolisten tcp &
xpid=$!
echo "$xpid" >/run/a26-xorg.pid

ready=0
for _ in $(seq 1 225); do
    if DISPLAY=:0 xrandr --current >"$STATUS.tmp" 2>&1; then
        ready=1
        break
    fi
    kill -0 "$xpid" 2>/dev/null || break
    sleep 0.2
done

if [ "$ready" != 1 ]; then
    {
        echo "XORG_READY=0"
        cat "$STATUS.tmp" 2>/dev/null || true
    } >"$STATUS"
    exit 20
fi

echo "Xorg answered up=$(cut -d' ' -f1 /proc/uptime)" >&2
# Native Wi-Fi is not needed for the lock screen: Moon's supervisor starts it
# after Moon is up (moon-post-ready.sh). It is still stopped with this session.

# The only client used for the initial bare-server presentation.  It exits
# immediately; no display manager, compositor, or window manager is launched.
DISPLAY=:0 xsetroot -solid '#18354f'
{
    echo "XORG_READY=1"
    echo "XORG_PID=$xpid"
    echo "DISPLAY=:0"
    echo "WINDOW_MANAGER=none"
    DISPLAY=:0 xrandr --current
} >"$STATUS"
rm -f "$STATUS.tmp"

# Keep the supervising shell alive.  When Xorg exits, the outer Android-side
# supervisor restores SurfaceFlinger and the hardware composer.
wait "$xpid"
exit $?
