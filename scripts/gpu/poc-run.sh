#!/usr/bin/env bash
# Run the KMS proof of concept on the attached phone and reboot it back into
# a clean Moon afterwards. Needs exclusive use of the phone: Moon is stopped
# for about a minute. Results land in build/moon-gpu/poc-<time>/.
#   scripts/gpu/poc-run.sh [--runs FILE] [path/to/moon-gpu-poc]
# FILE holds one line of moon-gpu-poc arguments per run ($D expands to the
# phone's work directory, where --csv files are pulled from).
set -euo pipefail
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
RUNS=''
if [[ "${1:-}" == --runs ]]; then RUNS="$2"; shift 2; fi
BIN="${1:-$PROJECT_ROOT/images/moon-gpu/moon-gpu-poc}"
OUT="$PROJECT_ROOT/build/moon-gpu/poc-$(date +%Y%m%d-%H%M%S)"
D=/data/local/tmp/gpu
mkdir -p "$OUT"

adb -s "$SERIAL" shell su -c "'mkdir -p $D && rm -f $D/*.csv $D/*.ppm $D/*.log $D/runs.txt'"
[[ -n "$RUNS" ]] && adb -s "$SERIAL" push "$RUNS" "$D/runs.txt" >/dev/null
adb -s "$SERIAL" push "$BIN" "$D/moon-gpu-poc" >/dev/null
COMPOSITOR="$(dirname "$BIN")/moon-compositor"
[[ -x "$COMPOSITOR" ]] && adb -s "$SERIAL" push "$COMPOSITOR" "$D/moon-compositor" >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/gpu/poc-session.sh" "$D/poc-session.sh" >/dev/null
adb -s "$SERIAL" shell su -c "'chmod 755 $D/moon-gpu-poc $D/poc-session.sh; chmod 755 $D/moon-compositor 2>/dev/null'"
BB=/data/local/a26-linux/busybox.static
adb -s "$SERIAL" shell su -c "'$BB nohup $BB setsid /system/bin/sh $D/poc-session.sh \
    >/dev/null 2>&1 </dev/null &'"
echo "session started; waiting for it to finish"
for _ in $(seq 1 120); do
    sleep 2
    if adb -s "$SERIAL" shell su -c "'grep -q \"^== done\" $D/session.log'" 2>/dev/null; then
        break
    fi
done
for f in $(adb -s "$SERIAL" shell su -c "'cd $D && ls *.log *.csv *.ppm 2>/dev/null'" | tr -d '\r'); do
    adb -s "$SERIAL" pull "$D/$f" "$OUT/$f" >/dev/null 2>&1 || true
done
cat "$OUT/session.log" 2>/dev/null || echo "no session log (did the phone reset? see /proc/last_kmsg)"
# Back to a pristine Moon: the deferral flag must be gone before the reboot.
adb -s "$SERIAL" shell su -c "'rm -f /data/local/tmp/moon-suspend/active'"
adb -s "$SERIAL" reboot
echo "results in $OUT; phone rebooting"
