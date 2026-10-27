#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"

AUDIOD_DIR="$PROJECT_ROOT/images/moon-audiod"
[[ -x "$AUDIOD_DIR/moon-audiod" && -x "$AUDIOD_DIR/moon-permissiond" ]] ||
    "$PROJECT_ROOT/scripts/a26-shell/build-audiod.sh"

for script in \
    "$PROJECT_ROOT/scripts/device/moon-boot-supervisor.sh" \
    "$PROJECT_ROOT/scripts/device/moon-direct-policy.sh" \
    "$PROJECT_ROOT/scripts/device/moon-post-ready.sh" \
    "$PROJECT_ROOT/scripts/device/moon-boot-confirm.sh" \
    "$PROJECT_ROOT/scripts/device/moon-audio-start.sh" \
    "$PROJECT_ROOT/scripts/device/moon-audio-stop.sh" \
    "$PROJECT_ROOT/scripts/device/moon-audio-keeper.sh" \
    "$PROJECT_ROOT/scripts/device/moon-gpu-session.sh" \
    "$PROJECT_ROOT/scripts/device/moon-suspend-cycle.sh" \
    "$PROJECT_ROOT/scripts/device/a26-enter-chroot.sh" \
    "$PROJECT_ROOT/scripts/device/90-moon-autostart.sh" \
    "$PROJECT_ROOT/scripts/device/a26-xorg-takeover.sh" \
    "$PROJECT_ROOT/scripts/device/a26-xorg-safety-watchdog.sh" \
    "$PROJECT_ROOT/scripts/device/a26-android-graphics-restore.sh" \
    "$PROJECT_ROOT/scripts/device/a26-xorg-persistent-session.sh" \
    "$PROJECT_ROOT/scripts/device/moon-power.sh" \
    "$PROJECT_ROOT/scripts/device/moon-power"
do
    sh -n "$script"
done

adb -s "$SERIAL" get-state >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-boot-supervisor.sh" \
    /data/local/tmp/moon-boot-supervisor.sh >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/90-moon-autostart.sh" \
    /data/local/tmp/90-moon-autostart.sh >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-audio-start.sh" \
    /data/local/tmp/moon-audio-start.sh >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-audio-stop.sh" \
    /data/local/tmp/moon-audio-stop.sh >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-audio-keeper.sh" \
    /data/local/tmp/moon-audio-keeper.sh >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-gpu-session.sh" \
    /data/local/tmp/moon-gpu-session.sh >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-suspend-cycle.sh" \
    /data/local/tmp/moon-suspend-cycle.sh >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-direct-policy.sh" \
    /data/local/tmp/moon-direct-policy.sh >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-post-ready.sh" \
    /data/local/tmp/moon-post-ready.sh >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-boot-confirm.sh" \
    /data/local/tmp/moon-boot-confirm.sh >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-mode-stop.list" \
    /data/local/tmp/moon-mode-stop.list >/dev/null
adb -s "$SERIAL" push "$AUDIOD_DIR/moon-audiod" \
    /data/local/tmp/moon-audiod >/dev/null
adb -s "$SERIAL" push "$AUDIOD_DIR/moon-permissiond" \
    /data/local/tmp/moon-permissiond >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/a26-enter-chroot.sh" \
    /data/local/tmp/a26-enter-chroot.sh.new >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/a26-xorg-takeover.sh" \
    /data/local/tmp/a26-xorg-takeover.sh.new >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/a26-xorg-safety-watchdog.sh" \
    /data/local/tmp/a26-xorg-safety-watchdog.sh.new >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/a26-android-graphics-restore.sh" \
    /data/local/tmp/a26-android-graphics-restore.sh.new >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/a26-xorg-persistent-session.sh" \
    /data/local/tmp/a26-xorg-persistent-session.sh >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-power.sh" \
    /data/local/tmp/moon-power.sh >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/moon-power" \
    /data/local/tmp/moon-power >/dev/null

adb -s "$SERIAL" shell '/data/local/tmp/su -c '\''
set -eu
mkdir -p /data/adb/moon /data/adb/service.d \
    /data/local/a26-linux/usr/local/sbin \
    /data/local/a26-linux/opt/a26-audio \
    /data/local/tmp/moon-suspend
chmod 0700 /data/adb/moon /data/local/tmp/moon-suspend

cp /data/local/tmp/moon-boot-supervisor.sh \
    /data/adb/moon/moon-boot-supervisor.sh.new
cp /data/local/tmp/90-moon-autostart.sh \
    /data/adb/service.d/90-moon-autostart.sh.new
cp /data/local/tmp/moon-audio-start.sh \
    /data/adb/moon/moon-audio-start.sh.new
cp /data/local/tmp/moon-audio-stop.sh \
    /data/adb/moon/moon-audio-stop.sh.new
cp /data/local/tmp/moon-audio-keeper.sh \
    /data/adb/moon/moon-audio-keeper.sh.new
cp /data/local/tmp/moon-gpu-session.sh \
    /data/adb/moon/moon-gpu-session.sh.new
cp /data/local/tmp/moon-suspend-cycle.sh \
    /data/adb/moon/moon-suspend-cycle.sh.new
cp /data/local/tmp/moon-direct-policy.sh \
    /data/adb/moon/moon-direct-policy.sh.new
cp /data/local/tmp/moon-post-ready.sh \
    /data/adb/moon/moon-post-ready.sh.new
cp /data/local/tmp/moon-mode-stop.list \
    /data/adb/moon/moon-mode-stop.list.new
cp /data/local/tmp/moon-audiod \
    /data/local/a26-linux/opt/a26-audio/moon-audiod.new
cp /data/local/tmp/moon-permissiond \
    /data/local/a26-linux/opt/a26-audio/moon-permissiond.new
cp /data/local/tmp/a26-enter-chroot.sh.new \
    /data/local/a26-linux/a26-enter-chroot.sh.new
cp /data/local/tmp/a26-xorg-takeover.sh.new \
    /data/local/tmp/a26-xorg-takeover.sh
cp /data/local/tmp/a26-xorg-safety-watchdog.sh.new \
    /data/local/tmp/a26-xorg-safety-watchdog.sh
cp /data/local/tmp/a26-android-graphics-restore.sh.new \
    /data/local/tmp/a26-android-graphics-restore.sh
cp /data/local/tmp/a26-xorg-persistent-session.sh \
    /data/local/a26-linux/usr/local/sbin/a26-xorg-persistent-session

chown 0:0 \
    /data/adb/moon/moon-boot-supervisor.sh.new \
    /data/adb/service.d/90-moon-autostart.sh.new \
    /data/adb/moon/moon-audio-start.sh.new \
    /data/adb/moon/moon-audio-stop.sh.new \
    /data/adb/moon/moon-audio-keeper.sh.new \
    /data/adb/moon/moon-gpu-session.sh.new \
    /data/adb/moon/moon-suspend-cycle.sh.new \
    /data/adb/moon/moon-direct-policy.sh.new \
    /data/adb/moon/moon-post-ready.sh.new \
    /data/adb/moon/moon-mode-stop.list.new \
    /data/local/a26-linux/opt/a26-audio/moon-audiod.new \
    /data/local/a26-linux/opt/a26-audio/moon-permissiond.new \
    /data/local/a26-linux/a26-enter-chroot.sh.new \
    /data/local/tmp/a26-xorg-takeover.sh \
    /data/local/tmp/a26-xorg-safety-watchdog.sh \
    /data/local/tmp/a26-android-graphics-restore.sh \
    /data/local/a26-linux/usr/local/sbin/a26-xorg-persistent-session
chmod 0700 /data/adb/moon/moon-boot-supervisor.sh.new
chmod 0700 \
    /data/adb/moon/moon-audio-start.sh.new \
    /data/adb/moon/moon-audio-stop.sh.new \
    /data/adb/moon/moon-audio-keeper.sh.new \
    /data/adb/moon/moon-gpu-session.sh.new \
    /data/adb/moon/moon-suspend-cycle.sh.new \
    /data/adb/moon/moon-direct-policy.sh.new \
    /data/adb/moon/moon-post-ready.sh.new
chmod 0600 /data/adb/moon/moon-mode-stop.list.new
chmod 0755 \
    /data/local/a26-linux/opt/a26-audio/moon-audiod.new \
    /data/local/a26-linux/opt/a26-audio/moon-permissiond.new
chmod 0755 /data/local/a26-linux/a26-enter-chroot.sh.new
chmod 0755 \
    /data/adb/service.d/90-moon-autostart.sh.new \
    /data/local/tmp/a26-xorg-takeover.sh \
    /data/local/tmp/a26-xorg-safety-watchdog.sh \
    /data/local/tmp/a26-android-graphics-restore.sh \
    /data/local/a26-linux/usr/local/sbin/a26-xorg-persistent-session

mv -f /data/adb/moon/moon-boot-supervisor.sh.new \
    /data/adb/moon/moon-boot-supervisor.sh
mv -f /data/adb/service.d/90-moon-autostart.sh.new \
    /data/adb/service.d/90-moon-autostart.sh
mv -f /data/adb/moon/moon-audio-start.sh.new \
    /data/adb/moon/moon-audio-start.sh
mv -f /data/adb/moon/moon-audio-stop.sh.new \
    /data/adb/moon/moon-audio-stop.sh
mv -f /data/adb/moon/moon-audio-keeper.sh.new \
    /data/adb/moon/moon-audio-keeper.sh
mv -f /data/adb/moon/moon-gpu-session.sh.new \
    /data/adb/moon/moon-gpu-session.sh
mv -f /data/adb/moon/moon-suspend-cycle.sh.new \
    /data/adb/moon/moon-suspend-cycle.sh
mv -f /data/adb/moon/moon-direct-policy.sh.new \
    /data/adb/moon/moon-direct-policy.sh
mv -f /data/adb/moon/moon-post-ready.sh.new \
    /data/adb/moon/moon-post-ready.sh
# Stage-2 boot-count clear (notes/stage2/PROMOTION-PLAN.md section 2). Inert on
# every ordinary boot: moon-post-ready.sh starts it only when
# /debug_ramdisk/oss-contained exists (a boot oss-init contained), and it
# exits at once without that marker.
cp /data/local/tmp/moon-boot-confirm.sh /data/adb/moon/moon-boot-confirm.sh.new
chown 0:0 /data/adb/moon/moon-boot-confirm.sh.new
chmod 0700 /data/adb/moon/moon-boot-confirm.sh.new
mv -f /data/adb/moon/moon-boot-confirm.sh.new /data/adb/moon/moon-boot-confirm.sh
mv -f /data/adb/moon/moon-mode-stop.list.new \
    /data/adb/moon/moon-mode-stop.list
mv -f /data/local/a26-linux/opt/a26-audio/moon-audiod.new \
    /data/local/a26-linux/opt/a26-audio/moon-audiod
mv -f /data/local/a26-linux/opt/a26-audio/moon-permissiond.new \
    /data/local/a26-linux/opt/a26-audio/moon-permissiond
mv -f /data/local/a26-linux/a26-enter-chroot.sh.new \
    /data/local/a26-linux/a26-enter-chroot.sh

# Power entry point (docs/power.md): device side and Moon's chroot helper.
cp /data/local/tmp/moon-power.sh /data/adb/moon/moon-power.sh.new
chown 0:0 /data/adb/moon/moon-power.sh.new
chmod 0700 /data/adb/moon/moon-power.sh.new
mv -f /data/adb/moon/moon-power.sh.new /data/adb/moon/moon-power.sh
mkdir -p /data/local/a26-linux/opt/a26-shell/bin
cp /data/local/tmp/moon-power /data/local/a26-linux/opt/a26-shell/bin/moon-power.new
chown 0:0 /data/local/a26-linux/opt/a26-shell/bin/moon-power.new
chmod 0755 /data/local/a26-linux/opt/a26-shell/bin/moon-power.new
mv -f /data/local/a26-linux/opt/a26-shell/bin/moon-power.new \
    /data/local/a26-linux/opt/a26-shell/bin/moon-power

if [ ! -f /data/adb/moon/config ]; then
    cat >/data/adb/moon/config <<EOF
MIN_START_BATTERY=5
BATTERY_WARN=20
BATTERY_LOW=10
BATTERY_CRITICAL=3
MAX_START_FAILURES=3
OVERRIDE_WINDOW_SECONDS=8
DIRECT_BOOT=1
DIRECT_OVERRIDE_SECONDS=3
AUDIO_STREAM_INDEX=5
GPU_COMPOSITOR=1
EOF
    chown 0:0 /data/adb/moon/config
    chmod 0600 /data/adb/moon/config
fi

# docs/power.md: Moon no longer falls back to Android on low battery, and
# starts from 5 % (above the 3 % critical power-off). Migrate the old
# defaults; values someone chose on purpose stay.
sed -i -e "/^LOW_BATTERY_FALLBACK=/d" -e "s/^MIN_START_BATTERY=20\$/MIN_START_BATTERY=5/" \
    /data/adb/moon/config

# Moon powers the panel down through DPMS on screen-off; an Xorg config that
# disables DPMS would hide the extension and leave the panel refreshing.
XCONF=/data/local/a26-linux/etc/X11/xorg.conf.d/20-a26.conf
if [ -f "$XCONF" ] && grep -q "\"DPMS\" \"false\"" "$XCONF"; then
    cp -p "$XCONF" /data/adb/moon/20-a26.conf.dpms-false
    sed -i "s/\"DPMS\" \"false\"/\"DPMS\" \"true\"/" "$XCONF"
fi

# Non-root clients of moon-notifyd (docs/notifications-protocol.md) join this
# group; root needs nothing.
grep -q "^moon-notify:" /data/local/a26-linux/etc/group ||
    echo "moon-notify:x:1801:" >>/data/local/a26-linux/etc/group

rm -f /data/adb/moon/disabled
rm -f \
    /data/local/tmp/moon-boot-supervisor.sh \
    /data/local/tmp/90-moon-autostart.sh \
    /data/local/tmp/moon-audio-start.sh \
    /data/local/tmp/moon-audio-stop.sh \
    /data/local/tmp/moon-audio-keeper.sh \
    /data/local/tmp/moon-gpu-session.sh \
    /data/local/tmp/moon-suspend-cycle.sh \
    /data/local/tmp/moon-direct-policy.sh \
    /data/local/tmp/moon-post-ready.sh \
    /data/local/tmp/moon-boot-confirm.sh \
    /data/local/tmp/moon-mode-stop.list \
    /data/local/tmp/moon-audiod \
    /data/local/tmp/moon-permissiond \
    /data/local/tmp/a26-enter-chroot.sh.new \
    /data/local/tmp/a26-xorg-takeover.sh.new \
    /data/local/tmp/a26-xorg-safety-watchdog.sh.new \
    /data/local/tmp/a26-android-graphics-restore.sh.new \
    /data/local/tmp/a26-xorg-persistent-session.sh

ls -lZ \
    /data/adb/service.d/90-moon-autostart.sh \
    /data/adb/moon/moon-boot-supervisor.sh \
    /data/adb/moon/moon-audio-start.sh \
    /data/adb/moon/moon-audio-stop.sh \
    /data/adb/moon/moon-suspend-cycle.sh \
    /data/adb/moon/moon-direct-policy.sh \
    /data/adb/moon/moon-post-ready.sh \
    /data/adb/moon/moon-boot-confirm.sh \
    /data/adb/moon/moon-mode-stop.list \
    /data/local/a26-linux/opt/a26-audio/moon-audiod \
    /data/local/a26-linux/opt/a26-audio/moon-permissiond \
    /data/adb/moon/config
'\'''

echo 'Installed and enabled autonomous Moon boot: direct boot when the boot image'
echo 'carries scripts/direct-boot, otherwise the Magisk service.d takeover.'

# Moon's GPU display path (the default; notes/gpu/DESIGN.md): moon-compositor,
# headless Xorg's configuration and the dummy driver. It keeps the user's
# GPU_COMPOSITOR choice; `scripts/gpu/install.sh --disable` opts out.
"$PROJECT_ROOT/scripts/gpu/install.sh"
