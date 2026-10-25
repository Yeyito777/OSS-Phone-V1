#!/usr/bin/env bash
# Cross-builds the a26-shell test binaries and runs them inside the phone's
# Alpine chroot. Usage: device-test.sh CARGO_BUILD_COMMAND...
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
TARGET=aarch64-unknown-linux-musl
ROOT=/data/local/a26-linux
DIR=/root/.a26-shell-tests

cd "$PROJECT_ROOT/src/a26-shell"
mapfile -t tests < <("$@" --locked --tests --target "$TARGET" --message-format=json |
    python3 -c 'import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("reason") == "compiler-artifact" and m["profile"]["test"] and m.get("executable"):
        print(m["executable"])')
(( ${#tests[@]} )) || { echo "no test binaries were built" >&2; exit 1; }

if ! SERIAL="$(a26_resolve_serial 2>/dev/null)"; then
    echo "WARNING: no phone attached; ${#tests[@]} test binaries built but not run" >&2
    exit 0
fi
adb -s "$SERIAL" shell "rm -rf /data/local/tmp/a26-shell-tests && mkdir /data/local/tmp/a26-shell-tests"
adb -s "$SERIAL" push "${tests[@]}" /data/local/tmp/a26-shell-tests/ >/dev/null
names=()
for test in "${tests[@]}"; do names+=("${test##*/}"); done
adb -s "$SERIAL" shell "/data/local/tmp/su -c 'set -e; rm -rf $ROOT$DIR; mkdir -p $ROOT$DIR; cp /data/local/tmp/a26-shell-tests/* $ROOT$DIR/; chmod 0755 $ROOT$DIR/*; rc=0; for t in ${names[*]}; do $ROOT/busybox.static chroot $ROOT /usr/bin/env PATH=/usr/sbin:/usr/bin:/sbin:/bin $DIR/\$t -q || rc=1; done; rm -rf $ROOT$DIR /data/local/tmp/a26-shell-tests; exit \$rc'"
