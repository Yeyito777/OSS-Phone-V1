#!/usr/bin/env bash
# Runs a fixed set of Moon animations on the attached (unlocked) phone with
# injected 120 Hz touches and prints each one's frame summary from Moon's
# `state.animation` (frames, missed vblanks, interval mean/p95/max). Works
# the same on the XRender and the GPU display paths, for comparison.
#   scripts/gpu/gestures.sh [ROUNDS]
# It ends on the lock screen.
set -euo pipefail
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
ROUNDS="${1:-3}"

adb -s "$SERIAL" shell su <<EOF
R=/data/local/a26-linux
BB=\$R/busybox.static
ctl() { \$BB chroot \$R /opt/a26-shell/bin/a26-shellctl "\$@"; }
touch_path() { \$BB chroot \$R /opt/a26-shell/bin/a26-touchpath "\$@"; }
summary() {
    sleep 0.8
    ctl state | \$BB sed -n 's/.*"animation":"\([^"]*\)".*/\1/p'
}
run() {
    label="\$1"; shift
    "\$@" >/dev/null 2>&1
    echo "\$label: \$(summary)"
}
ctl profile on >/dev/null
for round in \$(\$BB seq 1 $ROUNDS); do
    echo "== round \$round"
    ctl home >/dev/null; sleep 1
    run "launch from home" ctl launch system
    sleep 2
    run "swipe up out of an app" touch_path 540 2330 540 900 220
    sleep 1
    ctl launch system >/dev/null; sleep 2.5
    run "slow swipe up and hold into the switcher" touch_path 540 2330 540 1500 450 700
    sleep 0.5
    run "switcher scroll" touch_path 900 1300 150 1300 300
    sleep 0.5
    run "switcher back" touch_path 150 1300 900 1300 300
    sleep 0.5
done
# Last, because it ends locked: the lock screen's keypad reveal and back.
ctl home >/dev/null; sleep 1
ctl lock >/dev/null; sleep 1.5
for round in \$(\$BB seq 1 $ROUNDS); do
    run "lock screen drag to the keypad" touch_path 540 2200 540 1100 350
    sleep 1
    run "keypad back to the clock" touch_path 540 1100 540 2200 300
    sleep 1
done
ctl profile off >/dev/null
EOF
