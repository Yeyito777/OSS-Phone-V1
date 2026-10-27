#!/usr/bin/env bash
# Locates the ambient light sensor under the panel
# (docs/auto-brightness-compensation.md §4): Moon shows a black screen with
# one white square (`light-probe`) at full backlight, the sensor is switched
# off and on so the hub logs fresh readings, and the lux per square position
# is recorded. A coarse pass (90 px squares over the top 360 rows), then a
# fine pass (30 px) around the brightest coarse square. Flashes white for
# about a minute. Restores the probe, brightness and auto setting on exit.
#   scripts/a26-shell/light-sweep.sh [OUTPUT.csv]
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
OUTPUT="${1:-$PROJECT_ROOT/build/light-sweep-$(date +%Y%m%d-%H%M%S).csv}"
mkdir -p "$(dirname "$OUTPUT")"

ROOT=/data/local/a26-linux
CTL="$ROOT/busybox.static chroot $ROOT /opt/a26-shell/bin/a26-shellctl"
ctl() { adb -s "$SERIAL" shell "/data/local/tmp/su -c '$CTL $*'" | tr -d '\r'; }

display="$(ctl brightness)"
level="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["brightness"])' <<<"$display")"
auto="$(python3 -c 'import json,sys; print("on" if json.load(sys.stdin)["result"]["auto"] else "off")' <<<"$display")"
restore() {
    adb -s "$SERIAL" shell "rm -f /data/local/tmp/light-sweep-loop.sh /data/local/tmp/light-sweep-positions" || true
    ctl light-probe off >/dev/null || true
    ctl brightness auto off >/dev/null || true
    ctl brightness set "$level" >/dev/null || true
    ctl brightness auto "$auto" >/dev/null || true
    echo "Restored brightness $level, auto $auto."
}
trap restore EXIT
ctl screen on >/dev/null
ctl brightness auto off >/dev/null
ctl brightness set 100 >/dev/null
sleep 2

LOOP=/data/local/tmp/light-sweep-loop.sh
POSITIONS=/data/local/tmp/light-sweep-positions
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/light-sweep-loop.sh" "$LOOP" >/dev/null
probe() { # stdin: "X Y SIZE" lines; stdout: "X,Y,SIZE,LUX"
    local positions
    positions="$(mktemp)"
    cat >"$positions"
    adb -s "$SERIAL" push "$positions" "$POSITIONS" >/dev/null
    rm -f "$positions"
    adb -s "$SERIAL" shell "/data/local/tmp/su -c 'sh $LOOP $POSITIONS'" | tr -d '\r'
}

echo "x,y,size,lux" >"$OUTPUT"
{
    echo "0 0 0"
    for y in 0 90 180 270; do
        for x in 0 90 180 270 360 450 540 630 720 810 900 990; do echo "$x $y 90"; done
        echo "0 0 0"
    done
} | probe | tee -a "$OUTPUT"

peak="$(python3 - "$OUTPUT" <<'EOF'
import csv, sys
rows = [r for r in csv.DictReader(open(sys.argv[1])) if r["size"] == "90" and r["lux"]]
best = max(rows, key=lambda r: int(r["lux"]))
print(int(best["x"]) + 45, int(best["y"]) + 45)
EOF
)"
read -r cx cy <<<"$peak"
echo "coarse peak around ($cx, $cy); fine pass"
{
    echo "0 0 0"
    for dy in -75 -45 -15 15 45; do
        for dx in -75 -45 -15 15 45; do
            x=$((cx + dx)); y=$((cy + dy))
            [ $x -ge 0 ] && [ $y -ge 0 ] && echo "$x $y 30"
        done
    done
    echo "0 0 0"
} | probe | tee -a "$OUTPUT"

python3 - "$OUTPUT" <<'EOF'
import csv, sys
rows = [r for r in csv.DictReader(open(sys.argv[1])) if r["lux"]]
black = [int(r["lux"]) for r in rows if r["size"] == "0"]
fine = [r for r in rows if r["size"] == "30"]
base = min(black) if black else 0
weights = [(int(r["x"]) + 15, int(r["y"]) + 15, max(0, int(r["lux"]) - base)) for r in fine]
total = sum(w for *_, w in weights)
print(f"black screen: {min(black)}..{max(black)} lx (ambient drift check)")
if total:
    x = sum(x * w for x, _, w in weights) / total
    y = sum(y * w for _, y, w in weights) / total
    print(f"sensor centre ~ ({x:.0f}, {y:.0f}) px; peak 30 px square excess {max(w for *_, w in weights)} lx")
EOF
echo "Saved $OUTPUT"
