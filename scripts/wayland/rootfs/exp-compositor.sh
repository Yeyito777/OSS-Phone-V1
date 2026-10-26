#!/bin/sh
# Runs inside /data/local/a26-wayland (a26-wayland-run.sh): start one Wayland
# compositor on KMS, measure wlbench at several damage sizes, try Xwayland,
# stop the compositor. The KMS state after it exits is whatever the
# compositor leaves; the session ends with adb reboot.
#
#   exp-compositor.sh weston|cage [SECONDS] [MPX...]
set -u
which="${1:?weston|cage}"
shift
secs="${1:-8}"
[ $# -gt 0 ] && shift
mpx_list="${*:-0.25 0.5 0.75 1.07 1.5 2.0 2.53}"
out=/root/exp-$which
mkdir -p "$out"
export XDG_RUNTIME_DIR=/run/wl LIBSEAT_BACKEND=noop
mkdir -p -m 0700 "$XDG_RUNTIME_DIR"
rm -f "$XDG_RUNTIME_DIR"/wayland-* "$XDG_RUNTIME_DIR"/wl-a26*

wait_socket() {
    for _ in $(seq 1 100); do [ -S "$XDG_RUNTIME_DIR/$1" ] && return 0; sleep 0.1; done
    return 1
}

case "$which" in
weston)
    weston --backend=drm --renderer=pixman --socket=wl-a26 --idle-time=0 \
        --continue-without-input --xwayland --debug \
        --log="$out/weston.log" >"$out/weston.stdout" 2>&1 &
    comp=$!
    ;;
cage)
    # cage runs exactly one client; it is the waiting shell below, which runs
    # each wlbench in turn. Pixman renderer, no GPU.
    export WLR_RENDERER=pixman WLR_BACKENDS=drm,libinput WLR_LIBINPUT_NO_DEVICES=1
    cage -d -- sh -c 'echo "$WAYLAND_DISPLAY" >/run/wl/cage-display; while [ ! -e /run/wl/cage-stop ]; do sleep 0.2; done' \
        >"$out/cage.log" 2>&1 &
    comp=$!
    ;;
*) echo "unknown compositor $which" >&2; exit 2 ;;
esac

if [ "$which" = cage ]; then
    for _ in $(seq 1 100); do [ -s /run/wl/cage-display ] && break; sleep 0.1; done
    export WAYLAND_DISPLAY="$(cat /run/wl/cage-display 2>/dev/null)"
    wait_socket "$WAYLAND_DISPLAY" || { echo "cage did not start" >&2; tail -30 "$out/cage.log"; exit 1; }
else
    export WAYLAND_DISPLAY=wl-a26
    wait_socket wl-a26 || { echo "weston did not start" >&2; tail -40 "$out/weston.log"; exit 1; }
fi
echo "$which up on $WAYLAND_DISPLAY"
sleep 1
wayland-info >"$out/wayland-info.txt" 2>&1 || true

: >"$out/wlbench.txt"
for m in $mpx_list; do
    timeout $((secs + 10)) wlbench --mpx "$m" --seconds "$secs" >>"$out/wlbench.txt" 2>>"$out/wlbench.err"
    tail -n 1 "$out/wlbench.txt"
    sleep 1
done

# Xwayland: a rootless X11 client (weston starts Xwayland on demand).
if [ "$which" = weston ]; then
    xdisp="$(grep -o 'xserver listening on display :[0-9]*' "$out/weston.log" | tail -n 1 | sed 's/.*://')"
    [ -n "$xdisp" ] || xdisp=0
    DISPLAY=":$xdisp" timeout 20 xdpyinfo >"$out/xdpyinfo.txt" 2>&1 && echo "Xwayland :$xdisp answers"
    DISPLAY=":$xdisp" timeout 8 xeyes -geometry 600x600+0+0 >/dev/null 2>&1 &
    sleep 4
    weston-screenshooter >/dev/null 2>&1 && mv -f wayland-screenshot-*.png "$out/" 2>/dev/null
    echo "$xdisp" >"$out/xdisplay"
    wait
fi

if [ -n "${HOLD_SECONDS:-}" ]; then
    echo "holding the compositor for $HOLD_SECONDS s (Xwayland/app tests from outside)"
    sleep "$HOLD_SECONDS"
fi

touch /run/wl/cage-stop
kill -TERM "$comp" 2>/dev/null
for _ in $(seq 1 30); do kill -0 "$comp" 2>/dev/null || break; sleep 0.2; done
kill -KILL "$comp" 2>/dev/null
rm -f /run/wl/cage-stop /run/wl/cage-display
echo "$which stopped"
