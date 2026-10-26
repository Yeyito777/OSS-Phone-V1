#!/system/bin/sh
# Stops moon-telephonyd and its keeper (before Android's framework starts, so
# its phone process becomes rild's only client; and on uninstall). The daemon
# takes the mobile data interface down on SIGTERM.
set -u

RUNTIME=/data/local/tmp/moon-telephony
DAEMON=/data/local/a26-linux/opt/moon-telephony/moon-telephonyd

mkdir -p "$RUNTIME"
: >"$RUNTIME/stop"
keeper="$(cat "$RUNTIME/keeper.pid" 2>/dev/null || true)"
case "$keeper" in ''|*[!0-9]*) keeper='' ;; esac
for pid in $(pidof moon-telephonyd 2>/dev/null); do
    [ "$(readlink "/proc/$pid/exe" 2>/dev/null)" = "$DAEMON" ] && kill -TERM "$pid" 2>/dev/null
done
for _ in 1 2 3 4 5 6 7 8 9 10; do
    pidof moon-telephonyd >/dev/null 2>&1 || break
    sleep 0.3
done
if [ -n "$keeper" ] && grep -q moon-telephony-keeper "/proc/$keeper/cmdline" 2>/dev/null; then
    kill -TERM "$keeper" 2>/dev/null
fi
rm -f "$RUNTIME/keeper.pid"
echo "moon-telephonyd stopped"
