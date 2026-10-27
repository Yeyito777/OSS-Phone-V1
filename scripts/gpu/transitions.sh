#!/usr/bin/env bash
# The notification transitions, measured like scripts/gpu/gestures.sh: a
# banner over home and over an app, the notification centre in and out, and
# a timer's ringing screen rising from screen-off. Needs an unlocked phone;
# ends on the lock screen.
#   scripts/gpu/transitions.sh [ROUNDS]
set -euo pipefail
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
ROUNDS="${1:-2}"

adb -s "$SERIAL" shell su <<EOF
R=/data/local/a26-linux
BB=\$R/busybox.static
ctl() { \$BB chroot \$R /opt/a26-shell/bin/a26-shellctl "\$@"; }
notify() { \$BB chroot \$R /opt/a26-shell/bin/moon-notifyctl "\$@"; }
touch_path() { \$BB chroot \$R /opt/a26-shell/bin/a26-touchpath "\$@"; }
summary() {
    sleep "\${1:-0.8}"
    ctl state | \$BB sed -n 's/.*"animation":"\([^"]*\)".*/\1/p'
}
ctl profile on >/dev/null
for round in \$(\$BB seq 1 $ROUNDS); do
    echo "== round \$round"
    ctl home >/dev/null; sleep 1
    notify notify.post app=system title=Benchmark body="Banner over home" silent=true >/dev/null
    echo "banner in, over home: \$(summary 1.2)"
    echo "banner out (5 s timeout): \$(summary 5.5)"
    touch_path 540 4 540 1500 320 >/dev/null 2>&1
    echo "notification centre in: \$(summary)"
    touch_path 540 1500 540 120 260 >/dev/null 2>&1
    echo "notification centre out: \$(summary)"
    ctl launch system >/dev/null; sleep 2.5
    notify notify.post app=system title=Benchmark body="Banner over an app" silent=true >/dev/null
    echo "banner in, over an app: \$(summary 1.2)"
    sleep 5
done
# Last, because screen-off locks: a timer's ringing screen from screen-off.
ctl home >/dev/null; sleep 1.5
for round in \$(\$BB seq 1 $ROUNDS); do
    ctl screen off >/dev/null; sleep 2
    timer=\$(notify timer.create duration_s=2 label=Benchmark | \$BB grep -o '"id": *"[^"]*"' | \$BB head -1 | \$BB sed 's/.*"\([^"]*\)"\$/\1/')
    echo "ringing screen from screen-off: \$(summary 4)"
    notify timer.stop timer=\$timer >/dev/null
    echo "ringing screen dismissed: \$(summary 1.2)"
    sleep 1
done
ctl profile off >/dev/null
EOF
