#!/usr/bin/env bash
# Install Moon's GPU display path on the attached phone (notes/gpu/DESIGN.md):
# moon-compositor, its session script, the session scripts that know about
# it, Xorg's headless (dummy driver) configuration and the dummy driver
# itself. GPU mode stays off until enabled:
#   scripts/gpu/install.sh [--enable | --disable]
# Takes effect at the next boot (or session start). --disable only edits the
# config. The software path is untouched and remains the fallback.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
MODE="${1:-}"
case "$MODE" in ''|--enable|--disable) ;; *) echo "usage: $0 [--enable|--disable]" >&2; exit 2 ;; esac

su_sh() { adb -s "$SERIAL" shell su; }

if [[ "$MODE" == --disable ]]; then
    su_sh <<'EOF'
sed -i '/^GPU_COMPOSITOR=/d' /data/adb/moon/config
echo "GPU_COMPOSITOR=0" >>/data/adb/moon/config
grep GPU_COMPOSITOR /data/adb/moon/config
EOF
    echo "GPU mode disabled from the next session start."
    exit 0
fi

GPU_DIR="$PROJECT_ROOT/images/moon-gpu"
[[ -x "$GPU_DIR/moon-compositor" ]] || "$PROJECT_ROOT/scripts/gpu/build.sh"

# Alpine's xf86-video-dummy for the chroot's release, verified by apk
# against the rootfs's Alpine signing keys when installed.
DUMMY_APK=xf86-video-dummy-0.4.1-r3.apk
ALPINE=v3.24
CACHE="$PROJECT_ROOT/build/moon-gpu"
mkdir -p "$CACHE"
if [[ ! -s "$CACHE/$DUMMY_APK" ]]; then
    curl -fsSL -o "$CACHE/$DUMMY_APK.part" \
        "https://dl-cdn.alpinelinux.org/alpine/$ALPINE/main/aarch64/$DUMMY_APK"
    mv "$CACHE/$DUMMY_APK.part" "$CACHE/$DUMMY_APK"
fi

for script in moon-gpu-session.sh moon-boot-supervisor.sh a26-xorg-takeover.sh \
    a26-xorg-persistent-session.sh a26-enter-chroot.sh a26-android-graphics-restore.sh; do
    sh -n "$PROJECT_ROOT/scripts/device/$script"
done

T=/data/local/tmp/moon-gpu-install
adb -s "$SERIAL" shell "rm -rf $T && mkdir -p $T"
adb -s "$SERIAL" push "$GPU_DIR/moon-compositor" "$T/" >/dev/null
adb -s "$SERIAL" push "$CACHE/$DUMMY_APK" "$T/" >/dev/null
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/gpu/xorg/20-a26-gpu.conf" "$T/" >/dev/null
for script in moon-gpu-session.sh moon-boot-supervisor.sh a26-xorg-takeover.sh \
    a26-xorg-persistent-session.sh a26-enter-chroot.sh a26-android-graphics-restore.sh; do
    adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/$script" "$T/" >/dev/null
done

su_sh <<EOF
set -eu
T=$T
ROOT=/data/local/a26-linux
mkdir -p \$ROOT/opt/moon-gpu \$ROOT/etc/X11/xorg-gpu.conf.d /data/local/tmp/moon-gpu
chmod 0700 /data/local/tmp/moon-gpu

# Xorg's dummy driver, offline from the pushed package.
cp \$T/$DUMMY_APK \$ROOT/tmp/$DUMMY_APK
chroot \$ROOT /sbin/apk add --no-network --quiet /tmp/$DUMMY_APK
rm -f \$ROOT/tmp/$DUMMY_APK
ls \$ROOT/usr/lib/xorg/modules/drivers/dummy_drv.so

install_file() { # src dst mode
    cp "\$1" "\$2.new" && chown 0:0 "\$2.new" && chmod "\$3" "\$2.new" && mv -f "\$2.new" "\$2"
}
install_file \$T/moon-compositor \$ROOT/opt/moon-gpu/moon-compositor 0755
install_file \$T/20-a26-gpu.conf \$ROOT/etc/X11/xorg-gpu.conf.d/20-a26-gpu.conf 0644
# Input is the same device setup as the panel session.
install_file \$ROOT/etc/X11/xorg.conf.d/30-a26-input.conf \
    \$ROOT/etc/X11/xorg-gpu.conf.d/30-a26-input.conf 0644
install_file \$T/moon-gpu-session.sh /data/adb/moon/moon-gpu-session.sh 0700
install_file \$T/moon-boot-supervisor.sh /data/adb/moon/moon-boot-supervisor.sh 0700
install_file \$T/a26-xorg-takeover.sh /data/local/tmp/a26-xorg-takeover.sh 0755
install_file \$T/a26-android-graphics-restore.sh /data/local/tmp/a26-android-graphics-restore.sh 0755
install_file \$T/a26-xorg-persistent-session.sh \$ROOT/usr/local/sbin/a26-xorg-persistent-session 0755
install_file \$T/a26-enter-chroot.sh \$ROOT/a26-enter-chroot.sh 0755

if [ "$MODE" = --enable ]; then
    sed -i '/^GPU_COMPOSITOR=/d' /data/adb/moon/config
    echo "GPU_COMPOSITOR=1" >>/data/adb/moon/config
fi
rm -f /data/local/tmp/moon-gpu/failed-boot
rm -rf \$T
grep GPU_COMPOSITOR /data/adb/moon/config || echo "GPU_COMPOSITOR unset (off)"
sha256sum \$ROOT/opt/moon-gpu/moon-compositor
EOF
echo "Installed the GPU display path${MODE:+ ($MODE)}; it takes effect at the next boot."
