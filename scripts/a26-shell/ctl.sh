#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
[[ $# -gt 0 ]] || { echo "usage: $0 COMMAND [ARGS...]" >&2; exit 2; }

for argument in "$@"; do
    [[ "$argument" =~ ^[A-Za-z0-9_.,/:+-]+$ ]] || {
        echo "unsupported control argument: $argument" >&2
        exit 2
    }
done
command_line="$(printf '%s ' "$@")"
adb -s "$SERIAL" shell "/data/local/tmp/su -c \"A26_ROOT=/data/local/a26-linux A26_BUSYBOX=/data/local/a26-linux/busybox.static /system/bin/sh /data/local/a26-linux/a26-enter-chroot.sh /opt/a26-shell/bin/a26-shellctl $command_line\""
