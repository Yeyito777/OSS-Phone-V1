#!/system/bin/sh
# Keeps the Exocortex daemon (and its notification bridge) running on the
# phone, in Moon and Android mode alike (docs/EXOCORTEX.md):
#
#   exocortex-keeper.sh start      start the keeper and its watchdog, detached
#   exocortex-keeper.sh boot       the same, at boot (forgets `exocortexd stop`)
#   exocortex-keeper.sh shutdown   stop the daemon gracefully (moon-power.sh)
#   exocortex-keeper.sh status
#   exocortex-keeper.sh keeper|watchdog   (internal: the two loops)
#   exocortex-keeper.sh init       the keeper loop in the foreground, for the
#                                  proposed init service (proposed/exocortex.rc)
#
# Runs in Android's root namespace, started by the Magisk service.d hook
# (95-exocortex.sh), independent of Moon's supervisor and its init service.
# Two processes watch each other: the keeper (re)starts the daemon with
# backoff and restarts the watchdog; the watchdog restarts the keeper. Both
# leave the init service's cgroup and their session first, so nothing that
# ends Moon, Xorg or a chroot session takes them along. Each child runs in a
# private mount namespace with its own chroot mounts (exocortex-ns.sh), so
# Moon's session mounts coming and going never pull /dev or /proc out from
# under it. Everything stands down once the phone is shutting down.
set -u

ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
BASE=/data/adb/exocortex
STATE="$ROOT/var/lib/exocortex-keeper"   # /var/lib/exocortex-keeper in the chroot
LOGDIR="$ROOT/var/log/exocortex"
LOG="$LOGDIR/keeper.log"
SELF="$BASE/exocortex-keeper.sh"
NS="$BASE/exocortex-ns.sh"
NOTIFY_BIN=/opt/a26-system/bin/exocortex-notify

mkdir -p "$STATE" "$LOGDIR"

log() {
    echo "exocortex-keeper[$$]: $* [$(date '+%Y-%m-%dT%H:%M:%S%z' 2>/dev/null) up=$(cut -d' ' -f1 /proc/uptime)]" >>"$LOG"
}

uptime_s() { cut -d. -f1 /proc/uptime; }

shutting_down() {
    [ "$(getprop moon.boot.mode)" = shutdown ] || [ -n "$(getprop sys.powerctl)" ]
}

# The pid file's process must still be the named role: its command line
# matches the extended regex MARK (a zombie's is empty), so a reused PID never
# counts.
alive() {
    pid="$(cat "$1" 2>/dev/null)"
    case "$pid" in ''|*[!0-9]*) return 1 ;; esac
    "$BB" tr '\0' ' ' 2>/dev/null <"/proc/$pid/cmdline" | "$BB" grep -q -E -- "$2"
}

pid_of() { cat "$1" 2>/dev/null; }

leave_service_cgroup() {
    # The root cgroup is nobody's init service (see moon-power.sh).
    echo $$ >/sys/fs/cgroup/cgroup.procs 2>/dev/null
    echo -1000 >/proc/$$/oom_score_adj 2>/dev/null
}

spawn_detached() {
    # A new session, outside whatever started us; the role re-checks itself.
    "$BB" nohup "$BB" setsid /system/bin/sh "$SELF" "$1" </dev/null >>"$LOG" 2>&1 &
}

rotate() {
    for file in "$@"; do
        [ "$("$BB" stat -c %s "$file" 2>/dev/null || echo 0)" -gt 1048576 ] && mv -f "$file" "$file.1"
    done
}

# --- DNS for the daemon's namespace ----------------------------------------
# exocortex-ns.sh binds $STATE/resolv.conf over the chroot's /etc/resolv.conf.
# Moon's session keeps the live one (the Wi-Fi DHCP script writes it); without
# Moon, Android's DNS properties, then public resolvers. Rewritten in place
# (the bind follows the inode), only when it changes.
refresh_resolv() {
    new="$STATE/resolv.conf.new"
    : >"$new"
    if "$BB" grep -q '^nameserver ' "$ROOT/run/a26-resolv.conf" 2>/dev/null; then
        "$BB" grep '^nameserver ' "$ROOT/run/a26-resolv.conf" >>"$new"
    else
        for prop in net.dns1 net.dns2; do
            ns="$(getprop "$prop")"
            [ -n "$ns" ] && echo "nameserver $ns" >>"$new"
        done
    fi
    "$BB" grep -q '^nameserver ' "$new" || printf 'nameserver 1.1.1.1\nnameserver 8.8.8.8\n' >>"$new"
    echo 'options timeout:2 attempts:2' >>"$new"
    if ! "$BB" cmp -s "$new" "$STATE/resolv.conf"; then
        cat "$new" >"$STATE/resolv.conf"
        chmod 0644 "$STATE/resolv.conf"
    fi
    rm -f "$new"
}

# --- children -----------------------------------------------------------------
# Each stage of a start keeps the PID (unshare and every later step exec):
# the namespace script, run-daemon.sh's dependency check, the daemon itself.
# exocortex-ns.sh writes its own PID too, in case setsid had to fork.
DAEMON_MARK='exocortex-ns\.sh daemon|run-daemon\.sh|exocortexd-main'
NOTIFY_MARK='exocortex-ns\.sh notify|exocortex-notify'
SSHD_MARK='exocortex-ns\.sh sshd|sshd: /usr/sbin/sshd'
TUNNEL_MARK='exocortex-ns\.sh tunnel|oss-phone-tunnel|ssh -N .*-R 127\.0\.0\.1:'
KEEPER_MARK='exocortex-keeper\.sh (keeper|init)'
WATCHDOG_MARK='exocortex-keeper\.sh watchdog'

start_daemon() {
    rotate "$LOGDIR/daemon.out"
    "$BB" setsid "$BB" unshare -m --propagation private /system/bin/sh "$NS" daemon \
        </dev/null >>"$LOGDIR/daemon.out" 2>&1 &
    echo $! >"$STATE/daemon.pid"
    log "daemon started pid=$!"
}

# The other children (exocortex-notify, sshd, the reverse tunnel) each in a
# namespace of its own, restarted with the same backoff as the daemon.
start_child() {
    rotate "$LOGDIR/$1.out"
    "$BB" setsid "$BB" unshare -m --propagation private /system/bin/sh "$NS" "$1" \
        </dev/null >>"$LOGDIR/$1.out" 2>&1 &
    echo $! >"$STATE/$1.pid"
    log "$1 started pid=$!"
}

# keep_child NAME MARK: start NAME when it is not running, at most once per
# backoff delay (2 s doubling to 60 s; reset after two minutes up).
keep_child() {
    alive "$STATE/$1.pid" "$2" && return 0
    eval "started=\${${1}_started:-0} delay=\${${1}_delay:-2}"
    if [ "$started" != 0 ]; then
        [ $((now - started)) -lt 120 ] || delay=2
        [ $((now - started)) -ge "$delay" ] || return 0
        log "$1 exited; restarting (backoff ${delay}s)"
        [ "$delay" -ge 60 ] || delay=$((delay * 2))
    fi
    start_child "$1"
    eval "${1}_started=$(uptime_s) ${1}_delay=$delay"
}

stop_children() {
    stop_child "$STATE/notify.pid" "$NOTIFY_MARK" 3
    stop_child "$STATE/tunnel.pid" "$TUNNEL_MARK" 3
    stop_child "$STATE/sshd.pid" "$SSHD_MARK" 3
}

# SIGTERM is the daemon's graceful shutdown: it settles running turns and
# schedules them to replay on its next start (Exocortex README).
stop_child() {
    pidfile=$1 mark=$2 wait=$3
    alive "$pidfile" "$mark" || return 0
    pid="$(pid_of "$pidfile")"
    log "stopping pid=$pid ($mark)"
    kill -TERM "$pid" 2>/dev/null
    for _ in $("$BB" seq 1 $((wait * 5))); do
        alive "$pidfile" "$mark" || { log "pid=$pid stopped"; return 0; }
        "$BB" sleep 0.2
    done
    kill -KILL "$pid" 2>/dev/null
    log "pid=$pid killed after ${wait}s"
}

# --- the two loops ---------------------------------------------------------------
# Under init (the proposed rc service) init is the supervisor: no watchdog,
# and the service's own cgroup is the right one.
UNDER_INIT=0

keeper() {
    if [ "$UNDER_INIT" = 0 ]; then
        leave_service_cgroup
        trap '' HUP
        if alive "$STATE/keeper.pid" "$KEEPER_MARK" && [ "$(pid_of "$STATE/keeper.pid")" != $$ ]; then
            exit 0
        fi
    fi
    echo $$ >"$STATE/keeper.pid"
    log "keeper running"

    daemon_delay=2 daemon_started=0
    tick=0
    while :; do
        if shutting_down; then
            # moon-power.sh stops the daemon in its order; an Android
            # shutdown gets the same graceful stop from here.
            stop_child "$STATE/daemon.pid" "$DAEMON_MARK" 25
            stop_children
            log "phone shutting down; keeper exiting"
            # Under init an exit would only be restarted: wait for init's
            # own shutdown to end the service instead.
            [ "$UNDER_INIT" = 0 ] || while :; do "$BB" sleep 3600; done
            exit 0
        fi
        [ "$UNDER_INIT" = 1 ] || alive "$STATE/watchdog.pid" "$WATCHDOG_MARK" || spawn_detached watchdog

        if [ -e "$BASE/disabled" ]; then
            stop_child "$STATE/daemon.pid" "$DAEMON_MARK" 25
            stop_children
            "$BB" sleep 5
            continue
        fi
        [ $((tick % 15)) = 0 ] && refresh_resolv
        tick=$((tick + 1))
        now=$(uptime_s)

        if ! alive "$STATE/daemon.pid" "$DAEMON_MARK" && [ ! -e "$STATE/stopped" ]; then
            if [ -e "$STATE/restart-now" ]; then
                rm -f "$STATE/restart-now"
                daemon_delay=2
            elif [ "$daemon_started" != 0 ]; then
                # A run that lasted a while starts the backoff over.
                if [ $((now - daemon_started)) -ge 120 ]; then daemon_delay=2; fi
                log "daemon exited; restarting in ${daemon_delay}s"
                "$BB" sleep "$daemon_delay"
                [ "$daemon_delay" -ge 60 ] || daemon_delay=$((daemon_delay * 2))
            fi
            shutting_down || [ -e "$STATE/stopped" ] || { start_daemon; daemon_started=$(uptime_s); }
        fi

        now=$(uptime_s)
        [ ! -x "$ROOT$NOTIFY_BIN" ] || keep_child notify "$NOTIFY_MARK"
        # SSH (docs/SSH.md): the server once installed, the tunnel once configured.
        [ ! -x "$ROOT/usr/sbin/sshd" ] || [ ! -s "$ROOT/etc/ssh/oss-phone-sshd.conf" ] || keep_child sshd "$SSHD_MARK"
        [ ! -s "$ROOT/etc/oss-phone/tunnel.conf" ] || keep_child tunnel "$TUNNEL_MARK"
        "$BB" sleep 2
    done
}

watchdog() {
    leave_service_cgroup
    trap '' HUP
    if alive "$STATE/watchdog.pid" "$WATCHDOG_MARK" && [ "$(pid_of "$STATE/watchdog.pid")" != $$ ]; then
        exit 0
    fi
    echo $$ >"$STATE/watchdog.pid"
    while ! shutting_down; do
        if ! alive "$STATE/keeper.pid" "$KEEPER_MARK"; then
            log "watchdog: keeper not running; starting it"
            spawn_detached keeper
            "$BB" sleep 3
        fi
        "$BB" sleep 5
    done
    exit 0
}

case "${1:-}" in
    boot|start)
        # `exocortexd stop` lasts until the next boot, as under systemd.
        [ "$1" = start ] || rm -f "$STATE/stopped"
        rotate "$LOG"
        alive "$STATE/keeper.pid" "$KEEPER_MARK" || spawn_detached keeper
        alive "$STATE/watchdog.pid" "$WATCHDOG_MARK" || spawn_detached watchdog
        ;;
    keeper) keeper ;;
    init) UNDER_INIT=1; keeper ;;
    watchdog) watchdog ;;
    shutdown)
        # Called by moon-power.sh after it set moon.boot.mode=shutdown, so the
        # loops stand down instead of restarting what this stops.
        stop_child "$STATE/daemon.pid" "$DAEMON_MARK" "${2:-25}"
        stop_children
        ;;
    resolv) refresh_resolv ;;
    status)
        report() {
            if alive "$STATE/$1.pid" "$2"; then echo "$1: running pid $(pid_of "$STATE/$1.pid")"; else echo "$1: not running"; fi
        }
        report keeper "$KEEPER_MARK"
        report watchdog "$WATCHDOG_MARK"
        if alive "$STATE/daemon.pid" "$DAEMON_MARK"; then echo "daemon: running pid $(pid_of "$STATE/daemon.pid")"
        elif [ -e "$STATE/stopped" ]; then echo "daemon: stopped (exocortexd start)"
        else echo "daemon: not running"; fi
        report notify "$NOTIFY_MARK"
        report sshd "$SSHD_MARK"
        report tunnel "$TUNNEL_MARK"
        [ ! -e "$BASE/disabled" ] || echo "disabled ($BASE/disabled)"
        ;;
    *) echo "usage: $0 boot|start|shutdown|status" >&2; exit 2 ;;
esac
