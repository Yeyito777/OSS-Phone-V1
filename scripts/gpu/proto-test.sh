#!/usr/bin/env bash
# Runs moon-gpu-proto-test (src/moon-gpu/proto-test.c) against the phone's
# running moon-compositor: Moon is stopped (SIGSTOP) for the few seconds the
# test owns the compositor's socket, then continued; it reconnects and
# re-sends its layers by itself. The panel must be on (the test waits for
# flips); the screen shows a grey test frame meanwhile.
#   scripts/gpu/proto-test.sh
set -euo pipefail
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
BIN="$PROJECT_ROOT/images/moon-gpu/moon-gpu-proto-test"
[[ -x "$BIN" ]] || "$PROJECT_ROOT/scripts/gpu/build.sh"
adb -s "$SERIAL" push "$BIN" /data/local/tmp/moon-gpu-proto-test >/dev/null
adb -s "$SERIAL" shell su <<'EOF'
RUN=/data/local/tmp/moon-gpu
T=/data/local/tmp/moon-gpu-proto-test
chmod 0755 $T
moon="$(pidof a26-shell)"
[ -n "$moon" ] && kill -STOP $moon
$T $RUN/compositor.sock
status=$?
[ -n "$moon" ] && kill -CONT $moon
rm -f $T
sleep 2
tail -3 $RUN/compositor.log
exit $status
EOF
