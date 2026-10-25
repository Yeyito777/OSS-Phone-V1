#!/usr/bin/env bash
# Reboot the phone N times (default 3) and report, for each boot, the kernel
# uptime at every stage of the direct-boot critical path, ending at
# moon.session.ready=1. Reads only the boot's own logs; changes nothing.
#
#   scripts/direct-boot/measure-boot.sh [COUNT]
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
COUNT="${1:-3}"

phone() { adb -s "$SERIAL" shell "/data/local/tmp/su -c '$1'" | tr -d '\r'; }

for run in $(seq 1 "$COUNT"); do
    old_boot="$(phone 'cat /proc/sys/kernel/random/boot_id')"
    adb -s "$SERIAL" reboot
    started=$SECONDS
    while :; do
        (( SECONDS - started < 300 )) || { echo "run $run: no ready Moon within 300 s" >&2; exit 1; }
        sleep 2
        boot="$(phone 'cat /proc/sys/kernel/random/boot_id' 2>/dev/null || true)"
        [[ -n "$boot" && "$boot" != "$old_boot" ]] || continue
        mode="$(phone 'getprop moon.boot.mode' 2>/dev/null || true)"
        ready="$(phone 'getprop moon.session.ready' 2>/dev/null || true)"
        [[ "$mode" == moon && "$ready" == 1 ]] && break
        [[ "$mode" == android ]] && { echo "run $run: boot went to Android" >&2; exit 1; }
    done
    echo "== run $run: ready $((SECONDS - started)) s after adb reboot"
    phone 'b=$(cat /proc/sys/kernel/random/boot_id); awk -v b="$b" "index(\$0, \"boot_id=\" b) { on = 1 } on" /data/adb/moon/boot.log |
        grep -E "supervisor start|display bootstrap|takeover|Xorg ready|audio ready|active default" |
        sed -E "s/^moon-boot: //; s/ \[[^]]* (up=[0-9.]+)\]$/ \1/"
        grep -E "^(takeover begin|graphics services|DRM card|Xorg answered|native Wi-Fi)" /data/local/tmp/a26-xorg-control/takeover.log | sed -E "s/ token=[^ ]*//; s/ [0-9-]+T[^ ]* / /"'
done
