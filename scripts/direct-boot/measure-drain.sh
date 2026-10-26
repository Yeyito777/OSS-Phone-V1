#!/usr/bin/env bash
# Average battery current over SECONDS with the charger input suspended
# (batt_slate_mode), so the battery supplies the whole phone while it stays on
# USB. A detached phone-side timer restores charging even if adb drops.
# Set up the scenario first (screen state, app, audio), then:
#
#   scripts/direct-boot/measure-drain.sh SECONDS LABEL
#
# Prints: RESULT LABEL avg_mA=… samples=… range_mA=… cpu_busy_pct=… (docs/battery.md)
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
SECONDS_TO_MEASURE="${1:-120}"
LABEL="${2:-run}"
[[ "$SECONDS_TO_MEASURE" =~ ^[0-9]+$ && "$LABEL" =~ ^[A-Za-z0-9_.-]+$ ]] || {
    echo "usage: $0 SECONDS LABEL" >&2
    exit 2
}

SCRIPT="$(mktemp)"
trap 'rm -f "$SCRIPT"' EXIT
cat >"$SCRIPT" <<'PHONE'
# drain SECONDS LABEL: average battery current with the charger input suspended (slate mode);
# a detached guard restores charging even if this shell or adb dies.
S=/sys/class/power_supply/battery
secs=${1:-120}; label=${2:-run}
BB=/data/local/a26-linux/busybox.static
$BB nohup $BB setsid sh -c "sleep $((secs + 40)); echo 0 > $S/batt_slate_mode" >/dev/null 2>&1 </dev/null &
guard=$!
echo 1 > $S/batt_slate_mode
sleep 15
n=0; sum=0; min=0; max=-999999999; t=0
read c0 < /proc/stat; set -- $c0; t1=$(($2+$3+$4+$5+$6+$7+$8)); i1=$(($5+$6))
while [ $t -lt $secs ]; do
  c=$(cat $S/current_now); n=$((n+1)); sum=$((sum+c))
  [ $c -lt $min ] && min=$c; [ $c -gt $max ] && max=$c
  sleep 5; t=$((t+5))
done
read c0 < /proc/stat; set -- $c0; t2=$(($2+$3+$4+$5+$6+$7+$8)); i2=$(($5+$6))
echo 0 > $S/batt_slate_mode
kill $guard 2>/dev/null; pkill -f "sleep $((secs + 40))" 2>/dev/null
avg=$((sum/n/1000))
echo "RESULT $label avg_mA=$((-avg)) samples=$n range_mA=$((-max/1000))..$((-min/1000)) cpu_busy_pct=$(( 100*((t2-t1)-(i2-i1))/(t2-t1) )) voltage_mV=$(($(cat $S/voltage_now)/1000)) cap=$(cat $S/capacity)"
PHONE
adb -s "$SERIAL" push "$SCRIPT" /data/local/tmp/moon-measure-drain.sh >/dev/null 2>&1
adb -s "$SERIAL" shell "/data/local/tmp/su -c 'sh /data/local/tmp/moon-measure-drain.sh $SECONDS_TO_MEASURE $LABEL; rm -f /data/local/tmp/moon-measure-drain.sh'" |
    tr -d '\r' | grep '^RESULT'
