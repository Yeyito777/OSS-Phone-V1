#!/system/bin/sh
# Phone side of the KMS proof of concept (run detached as root by
# poc-run.sh). Stops the running Moon session without its Android-restore
# traps, runs moon-gpu-poc as DRM master, then tests handing the display back
# to Xorg in place and restarts Moon. The host reboots the phone afterwards,
# so nothing here has to survive.
set -u
D=/data/local/tmp/gpu
ROOT=/data/local/a26-linux
BB=$ROOT/busybox.static
POC=$D/moon-gpu-poc
exec </dev/null >"$D/session.log" 2>&1

up() { cut -d' ' -f1 /proc/uptime; }
log() { echo "== $* (up $(up))"; }
card_users() { /system/bin/toybox lsof /dev/dri/card0 2>/dev/null | grep /dev/dri/card0; }

log "begin $(date)"
# The supervisor defers its liveness checks while this flag exists (it is
# what a planned deep-suspend cycle sets); without it, a missing Xorg would
# end the supervisor and start Android's framework.
mkdir -p /data/local/tmp/moon-suspend
: >/data/local/tmp/moon-suspend/active

# KILL (not TERM) the shells whose EXIT traps restore Android, then stop
# Xorg; Moon exits with its X connection.
for pid in $(ps -A -o PID,ARGS | grep -E 'a26-xorg-takeover.sh|a26-xorg-persistent-session' |
        grep -v grep | awk '{print $1}'); do
    log "kill -9 $pid $(tr '\0' ' ' </proc/$pid/cmdline 2>/dev/null)"
    kill -KILL "$pid" 2>/dev/null
done
for pid in $(pidof Xorg); do
    log "TERM Xorg $pid"
    kill -TERM "$pid"
done
for _ in $(seq 1 50); do
    [ -z "$(card_users)" ] && break
    sleep 0.1
done
card_users && { log "card0 still open, giving up"; exit 1; }
log "card0 released"

run() {
    log "moon-gpu-poc $*"
    "$POC" "$@" 2>&1
    log "exit $?"
}
# One moon-gpu-poc run per line of runs.txt (pushed by poc-run.sh --runs),
# or the default set.
if [ -s "$D/runs.txt" ]; then
    while read -r args; do
        [ -n "$args" ] && eval run $args
    done <"$D/runs.txt"
else
    run --seconds 10 --brightness 200 --csv "$D/switcher.csv" --dump "$D/switcher.ppm"
    run --seconds 10 --scene zoom --brightness 200 --csv "$D/zoom.csv"
    run --seconds 4 --scene bg --brightness 200 --csv "$D/bg.csv"
    run --seconds 3 --active-cycles 3 --brightness 200 --csv "$D/cycles.csv"
fi

# The compositor daemon's startup self-test (KMS, EGL, targets, X mirror).
if [ -x "$D/moon-compositor" ]; then
    log "moon-compositor startup"
    "$D/moon-compositor" --socket "$D/compositor.sock" --ready-file "$D/compositor.ready" \
        >"$D/compositor.log" 2>&1 &
    cpid=$!
    sleep 3
    cat "$D/compositor.ready" 2>/dev/null
    kill "$cpid"
    wait "$cpid"
    log "exit $?"
    cat "$D/compositor.log"
fi

# In-place handoff to Xorg: keep the last GPU frame on screen with master
# dropped, start the normal Xorg session, which becomes master and modesets.
log "handoff: poc holds the frame, Xorg starts"
"$POC" --seconds 2 --hold 30 --brightness 200 >"$D/hold.log" 2>&1 &
holder=$!
sleep 4
rm -f "$ROOT/root/xorg-session.status"
A26_ROOT="$ROOT" A26_BUSYBOX="$BB" "$BB" nohup "$BB" setsid /system/bin/sh \
    "$ROOT/a26-enter-chroot.sh" /usr/local/sbin/a26-xorg-persistent-session \
    >/dev/null 2>&1 </dev/null &
for _ in $(seq 1 100); do
    grep -q '^XORG_READY=' "$ROOT/root/xorg-session.status" 2>/dev/null && break
    sleep 0.2
done
log "xorg status: $(head -1 "$ROOT/root/xorg-session.status" 2>/dev/null)"
chroot "$ROOT" /usr/bin/drm_info 2>/dev/null | grep -E '"ACTIVE"|"FB_ID"|"DPMS"' | head -6
kill "$holder" 2>/dev/null
wait "$holder"
cat "$D/hold.log"
sleep 1
log "after the holder exited"
chroot "$ROOT" /usr/bin/drm_info 2>/dev/null | grep -E '"ACTIVE"|"FB_ID"' | head -4

# Moon, as the supervisor starts it.
rm -f "$ROOT/run/a26-shell/control.sock"
"$BB" nohup "$BB" setsid /system/bin/sh "$ROOT/a26-enter-chroot.sh" \
    /usr/bin/env DISPLAY=:0 A26_SHELL_CONFIG=/etc/a26-shell/config.json \
    /opt/a26-shell/bin/a26-shell >>"$ROOT/root/a26-shell.log" 2>&1 </dev/null &
for _ in $(seq 1 50); do
    "$BB" timeout 1 "$BB" chroot "$ROOT" /opt/a26-shell/bin/a26-shellctl ping \
        >/dev/null 2>&1 && break
    sleep 0.2
done
log "moon ping: $("$BB" timeout 2 "$BB" chroot "$ROOT" /opt/a26-shell/bin/a26-shellctl ping 2>&1 | head -c 200)"
log "done"
