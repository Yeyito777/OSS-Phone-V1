#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
ACTION="${1:-status}"

case "$ACTION" in
    status)
        adb -s "$SERIAL" shell '/data/local/tmp/su -c '\''
echo "enabled=$([ ! -e /data/adb/moon/disabled ] && echo true || echo false)"
echo "skip_once=$([ -e /data/adb/moon/skip-once ] && echo true || echo false)"
echo "start_failures=$(cat /data/adb/moon/start-failures 2>/dev/null || echo 0)"
echo "last_success_boot_id=$(cat /data/adb/moon/last-success-boot-id 2>/dev/null || true)"
echo "current_boot_id=$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || true)"
echo "Xorg=$(pidof Xorg 2>/dev/null || true)"
echo "Moon=$(pidof a26-shell 2>/dev/null || true)"
echo "boot_mode=$(getprop moon.boot.mode) session_ready=$(getprop moon.session.ready)"
tail -20 /data/adb/moon/direct-boot.log 2>/dev/null || true
tail -80 /data/adb/moon/boot.log 2>/dev/null || true
'\'''
        ;;
    enable)
        adb -s "$SERIAL" shell '/data/local/tmp/su -c "rm -f /data/adb/moon/disabled /data/adb/moon/start-failures"'
        ;;
    disable)
        adb -s "$SERIAL" shell '/data/local/tmp/su -c "mkdir -p /data/adb/moon; touch /data/adb/moon/disabled; chmod 0600 /data/adb/moon/disabled"'
        ;;
    skip-once)
        adb -s "$SERIAL" shell '/data/local/tmp/su -c "mkdir -p /data/adb/moon; touch /data/adb/moon/skip-once; chmod 0600 /data/adb/moon/skip-once"'
        ;;
    *)
        echo "usage: $0 [status|enable|disable|skip-once]" >&2
        exit 2
        ;;
esac
