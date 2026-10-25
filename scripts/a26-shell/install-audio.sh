#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"

AUDIOD_DIR="$PROJECT_ROOT/images/moon-audiod"
[[ -x "$AUDIOD_DIR/moon-audiod" && -x "$AUDIOD_DIR/moon-permissiond" ]] ||
    "$PROJECT_ROOT/scripts/a26-shell/build-audiod.sh"
START="$PROJECT_ROOT/scripts/device/moon-audio-start.sh"
STOP="$PROJECT_ROOT/scripts/device/moon-audio-stop.sh"
SUSPEND="$PROJECT_ROOT/scripts/device/moon-suspend-cycle.sh"
CHROOT="$PROJECT_ROOT/scripts/device/a26-enter-chroot.sh"

for script in "$START" "$STOP" "$SUSPEND" "$CHROOT"; do
    sh -n "$script"
done

adb -s "$SERIAL" get-state >/dev/null
adb -s "$SERIAL" push "$AUDIOD_DIR/moon-audiod" /data/local/tmp/moon-audiod >/dev/null
adb -s "$SERIAL" push "$AUDIOD_DIR/moon-permissiond" /data/local/tmp/moon-permissiond >/dev/null
adb -s "$SERIAL" push "$START" /data/local/tmp/moon-audio-start.sh >/dev/null
adb -s "$SERIAL" push "$STOP" /data/local/tmp/moon-audio-stop.sh >/dev/null
adb -s "$SERIAL" push "$SUSPEND" /data/local/tmp/moon-suspend-cycle.sh >/dev/null
adb -s "$SERIAL" push "$CHROOT" /data/local/tmp/a26-enter-chroot.sh >/dev/null

adb -s "$SERIAL" shell '/data/local/tmp/su -c '\''
set -eu
mkdir -p /data/adb/moon /data/local/a26-linux/opt/a26-audio \
    /data/local/tmp/moon-suspend
chmod 0700 /data/adb/moon /data/local/tmp/moon-suspend

cp /data/local/tmp/moon-audio-start.sh /data/adb/moon/moon-audio-start.sh.new
cp /data/local/tmp/moon-audio-stop.sh /data/adb/moon/moon-audio-stop.sh.new
cp /data/local/tmp/moon-suspend-cycle.sh /data/adb/moon/moon-suspend-cycle.sh.new
cp /data/local/tmp/moon-audiod /data/local/a26-linux/opt/a26-audio/moon-audiod.new
cp /data/local/tmp/moon-permissiond /data/local/a26-linux/opt/a26-audio/moon-permissiond.new
cp /data/local/tmp/a26-enter-chroot.sh \
    /data/local/a26-linux/a26-enter-chroot.sh.new
chown 0:0 \
    /data/adb/moon/moon-audio-start.sh.new \
    /data/adb/moon/moon-audio-stop.sh.new \
    /data/adb/moon/moon-suspend-cycle.sh.new \
    /data/local/a26-linux/opt/a26-audio/moon-audiod.new \
    /data/local/a26-linux/opt/a26-audio/moon-permissiond.new \
    /data/local/a26-linux/a26-enter-chroot.sh.new
chmod 0700 \
    /data/adb/moon/moon-audio-start.sh.new \
    /data/adb/moon/moon-audio-stop.sh.new \
    /data/adb/moon/moon-suspend-cycle.sh.new
chmod 0755 /data/local/a26-linux/opt/a26-audio/moon-audiod.new \
    /data/local/a26-linux/opt/a26-audio/moon-permissiond.new
chmod 0755 /data/local/a26-linux/a26-enter-chroot.sh.new

mv -f /data/adb/moon/moon-audio-start.sh.new \
    /data/adb/moon/moon-audio-start.sh
mv -f /data/adb/moon/moon-audio-stop.sh.new \
    /data/adb/moon/moon-audio-stop.sh
mv -f /data/adb/moon/moon-suspend-cycle.sh.new \
    /data/adb/moon/moon-suspend-cycle.sh
mv -f /data/local/a26-linux/opt/a26-audio/moon-audiod.new \
    /data/local/a26-linux/opt/a26-audio/moon-audiod
mv -f /data/local/a26-linux/opt/a26-audio/moon-permissiond.new \
    /data/local/a26-linux/opt/a26-audio/moon-permissiond
mv -f /data/local/a26-linux/a26-enter-chroot.sh.new \
    /data/local/a26-linux/a26-enter-chroot.sh

rm -f \
    /data/local/tmp/moon-audio-start.sh \
    /data/local/tmp/moon-audio-stop.sh \
    /data/local/tmp/moon-suspend-cycle.sh \
    /data/local/tmp/moon-audiod \
    /data/local/tmp/moon-permissiond \
    /data/local/tmp/a26-enter-chroot.sh
'\'''

expected="$(shasum -a 256 "$AUDIOD_DIR/moon-audiod" | awk '{print $1}')"
actual="$(adb -s "$SERIAL" shell '/data/local/tmp/su -c '\''sha256sum /data/local/a26-linux/opt/a26-audio/moon-audiod | cut -d" " -f1'\''' | tr -d '\r')"
[[ "$actual" == "$expected" ]]
echo 'Installed Moon native audio and native-session runtime helpers.'
