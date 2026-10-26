#!/usr/bin/env bash
# Install Moon's telephony service on the attached phone: moon-telephonyd,
# moon-telephonyctl, moon-call-audio and the mobile-data hook under the
# chroot's /opt/moon-telephony, and the start/stop scripts in /data/adb/moon.
# The boot hooks that call them (moon-post-ready.sh, a26-enter-chroot.sh,
# a26-android-graphics-restore.sh) are installed by install-autostart.sh.
# With --start, (re)starts the daemon now.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
IMAGE="$PROJECT_ROOT/images/moon-telephony"

[[ -x "$IMAGE/moon-telephonyd" && -x "$IMAGE/moon-call-audio" ]] ||
    "$PROJECT_ROOT/scripts/a26-shell/build-telephony.sh"
for script in moon-telephony-start.sh moon-telephony-stop.sh; do
    sh -n "$PROJECT_ROOT/scripts/device/$script"
done

STAGE=/data/local/tmp/moon-telephony-install
adb -s "$SERIAL" shell "rm -rf $STAGE && mkdir -p $STAGE"
adb -s "$SERIAL" push "$IMAGE/moon-telephonyd" "$IMAGE/moon-telephonyctl" "$IMAGE/moon-call-audio" \
    "$IMAGE/mobile-data" "$PROJECT_ROOT/scripts/device/moon-telephony-start.sh" \
    "$PROJECT_ROOT/scripts/device/moon-telephony-stop.sh" "$STAGE/" >/dev/null

START=0
[[ "${1:-}" == --start ]] && START=1
adb -s "$SERIAL" shell "/data/local/tmp/su -c '
set -eu
ROOT=/data/local/a26-linux
OPT=\$ROOT/opt/moon-telephony
[ -x /data/adb/moon/moon-telephony-stop.sh ] && sh /data/adb/moon/moon-telephony-stop.sh >/dev/null 2>&1 || true
mkdir -p \$OPT /data/adb/moon
for f in moon-telephonyd moon-telephonyctl moon-call-audio mobile-data; do
    cp $STAGE/\$f \$OPT/\$f.new && chmod 0755 \$OPT/\$f.new && mv -f \$OPT/\$f.new \$OPT/\$f
done
ln -sf /opt/moon-telephony/moon-telephonyctl \$ROOT/usr/local/bin/moon-telephonyctl
for f in moon-telephony-start.sh moon-telephony-stop.sh; do
    cp $STAGE/\$f /data/adb/moon/\$f.new && chmod 0700 /data/adb/moon/\$f.new && mv -f /data/adb/moon/\$f.new /data/adb/moon/\$f
done
grep -q \"^moon-telephony:\" \$ROOT/etc/group || \$ROOT/busybox.static chroot \$ROOT /usr/sbin/addgroup -S moon-telephony
rm -rf $STAGE
if [ $START = 1 ]; then sh /data/adb/moon/moon-telephony-start.sh; fi
'"
echo "Installed moon-telephony (start: $([[ $START = 1 ]] && echo now || echo at the next Moon boot))"
