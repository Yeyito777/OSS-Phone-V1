#!/usr/bin/env bash
# Soak test of the default Moon configuration on the attached phone. The
# harness (scripts/device/moon-soak.sh) runs on the phone, detached, so the
# Mac may sleep or disconnect meanwhile.
#
#   scripts/a26-shell/soak.sh start [HOURS] [--no-charge]   (default 24 h)
#   scripts/a26-shell/soak.sh status
#   scripts/a26-shell/soak.sh stop
#   scripts/a26-shell/soak.sh report [RUN]   pull the data and the logs, analyse
#
# --no-charge pauses charging over USB (slate mode) to measure the idle drain.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
BASE=/data/local/tmp/moon-soak
su_run() { adb -s "$SERIAL" shell "/data/local/tmp/su -c '$1'" | tr -d '\r'; }
latest() { su_run "ls -1 $BASE 2>/dev/null | tail -1"; }

case "${1:-}" in
start)
    hours="${2:-24}"
    no_charge=0
    [[ "${3:-}" == --no-charge || "${2:-}" == --no-charge ]] && no_charge=1
    [[ "$hours" =~ ^[0-9]+$ ]] || hours=24
    run="$BASE/$(date +%Y%m%d-%H%M%S)"
    adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-soak.sh" /data/local/tmp/moon-soak.sh >/dev/null
    su_run "mkdir -p $run && cp /data/local/tmp/moon-soak.sh $BASE/moon-soak.sh &&
        /data/local/a26-linux/busybox.static setsid /data/local/a26-linux/busybox.static nohup \
        /system/bin/sh $BASE/moon-soak.sh $run $hours $no_charge >/dev/null 2>&1 </dev/null &"
    sleep 2
    echo "started $run (${hours} h, no_charge=$no_charge): pid $(su_run "cat $run/pid")"
    ;;
status)
    run="$BASE/$(latest)"
    su_run "cat $run/pid 2>/dev/null && echo running || echo finished; tail -3 $run/soak.log;
        tail -1 $run/samples.csv; tail -5 $run/events.csv"
    ;;
stop)
    run="$BASE/$(latest)"
    su_run "touch $run/stop; pid=\$(cat $run/pid 2>/dev/null); [ -n \"\$pid\" ] && kill -TERM \$pid"
    echo "stopping $run"
    ;;
report)
    name="${2:-$(latest)}"
    out="$PROJECT_ROOT/build/soak/$name"
    mkdir -p "$out"
    for file in samples.csv procs.csv events.csv soak.log; do
        su_run "cat $BASE/$name/$file" >"$out/$file"
    done
    # Logs from the run's time span, for the error scan.
    su_run 'cat /data/adb/moon/boot.log /data/adb/moon/power.log /data/adb/moon/telephony.log 2>/dev/null' >"$out/moon-logs.txt"
    su_run 'cat /data/adb/moon/notifyd.log /data/local/a26-linux/root/a26-shell.log 2>/dev/null' >>"$out/moon-logs.txt"
    su_run 'dmesg' >"$out/dmesg.txt"
    su_run 'logcat -d -b crash,main,system 2>/dev/null | tail -20000' >"$out/logcat.txt"
    python3 "$PROJECT_ROOT/scripts/a26-shell/soak-report.py" "$out"
    ;;
*)
    sed -n '2,13p' "$0"
    exit 2
    ;;
esac
