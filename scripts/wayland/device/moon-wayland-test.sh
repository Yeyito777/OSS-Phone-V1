#!/system/bin/sh
# Experiment only (milestone 1 on the phone): run Moon with its Wayland
# server on, without touching /data/adb/moon or the boot path.
#
#   su -c 'sh /data/local/tmp/a26-wayland/moon-wayland-test.sh start [demo|system]'
#   su -c 'sh /data/local/tmp/a26-wayland/moon-wayland-test.sh stop'
#
# start: stops the running Moon session and starts the installed Moon again
# exactly as the supervisor does (same chroot wrapper, same GPU socket when
# the GPU session is up), plus MOON_WAYLAND_DIR; `demo` puts the native demo
# app in the System slot, `system` the System app's Wayland build. The supervisor's liveness probe keeps
# working (same control socket) and its pidfile names the new session, so
# a failure still takes the normal recovery and fallback paths.
# stop: the same restart without the Wayland environment. A reboot also
# returns to the normal session.
set -u
ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
RUNTIME=/data/local/tmp/moon-boot
LOG=/data/local/tmp/a26-wayland/moon-wayland-test.log
mkdir -p /data/local/tmp/a26-wayland
echo "== $* $(date)" >>"$LOG"

[ "$(getprop moon.session.ready)" = 1 ] || { echo "Moon is not the active session" >&2; exit 1; }

stop_session() {
    session="$(cat "$RUNTIME/moon-session.pid" 2>/dev/null)"
    case "$session" in ''|*[!0-9]*) return 0 ;; esac
    kill -TERM "-$session" 2>/dev/null
    for _ in $(seq 1 25); do kill -0 "-$session" 2>/dev/null || return 0; sleep 0.2; done
    kill -KILL "-$session" 2>/dev/null
}

start_session() {
    extra="$1"
    boot_id="$(cat /proc/sys/kernel/random/boot_id)"
    gpu_env=''
    [ "$(cat /data/local/tmp/moon-gpu/active 2>/dev/null)" != "$boot_id" ] ||
        gpu_env=MOON_GPU_SOCKET=/run/moon-gpu/compositor.sock
    rm -f "$ROOT/run/a26-shell/control.sock"
    "$BB" nohup "$BB" setsid /system/bin/sh "$ROOT/a26-enter-chroot.sh" \
        /usr/bin/env DISPLAY=:0 A26_SHELL_CONFIG=/etc/a26-shell/config.json $gpu_env $extra \
        /opt/a26-shell/bin/a26-shell \
        >>"$ROOT/root/a26-shell.log" 2>&1 </dev/null &
    echo "$!" >"$RUNTIME/moon-session.pid.new"
    chmod 0600 "$RUNTIME/moon-session.pid.new"
    mv -f "$RUNTIME/moon-session.pid.new" "$RUNTIME/moon-session.pid"
    for _ in $(seq 1 50); do
        "$BB" timeout -s KILL 2 "$BB" chroot "$ROOT" /opt/a26-shell/bin/a26-shellctl ping \
            >/dev/null 2>&1 && { echo "Moon answers (session $(cat "$RUNTIME/moon-session.pid")) $gpu_env $extra"; return 0; }
        sleep 0.2
    done
    echo "Moon did not answer within 10 s" >&2
    return 1
}

case "${1:-}" in
    start)
        extra=MOON_WAYLAND_DIR=/run/moon-wayland
        case "${2:-}" in
            demo) extra="$extra A26_SYSTEM_APP=/opt/moon-wl-demo/bin/moon-wl-demo" ;;
            system) extra="$extra A26_SYSTEM_APP=/opt/a26-system/bin/a26-system-wayland" ;;
        esac
        stop_session
        start_session "$extra" 2>&1 | tee -a "$LOG"
        ;;
    stop)
        stop_session
        start_session '' 2>&1 | tee -a "$LOG"
        ;;
    *) echo "usage: $0 start [demo|system] | stop" >&2; exit 2 ;;
esac
