#!/usr/bin/env bash
# The Venus experiment on the phone (notes/gpu/BROWSER.md, option C):
# virglrenderer's vtest server runs on the Mali's Vulkan driver (bionic,
# beside moon-compositor), and vkfirst runs in the Browser's Debian root
# through Mesa's Venus driver, clearing and reading back a 1080x2340 image
# on the GPU. Nothing is installed: the files go to /data/local/tmp/venus and
# the Debian root's /tmp/venus. Display and compositor are not touched.
#   scripts/venus/phone-test.sh [--serve] [BUNDLE_DIR] [ROUNDS]
#       vkfirst, then everything removed again; with --serve the server and
#       the files stay for the Browser (scripts/gpu/browser-variant.sh with
#       VK_ICD_FILENAMES=/tmp/venus/venus_icd.json VN_DEBUG=vtest
#       VTEST_SOCKET_NAME=/tmp/.venus-test.sock LD_LIBRARY_PATH=/tmp/venus/lib)
#   scripts/venus/phone-test.sh --stop     stop the server, remove the files
# BUNDLE_DIR (default build/venus) comes from scripts/venus/build.sh.
set -euo pipefail
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
ROOTFS=/data/local/a26-linux/opt/vimbrowser-a26/rootfs
CLEANUP="
for p in \$(pidof virgl_test_server) \$(pidof virgl_render_server); do kill -9 \$p; done
rm -rf $ROOTFS/tmp/venus $ROOTFS/tmp/.venus-test.sock /data/local/tmp/venus
echo 'venus server stopped, files removed'"
MODE=test
case "${1:-}" in
--serve) MODE=serve; shift ;;
--stop) adb -s "$SERIAL" shell su <<<"$CLEANUP"; exit 0 ;;
esac
BUNDLE="${1:-$PROJECT_ROOT/build/venus}"
ROUNDS="${2:-20}"
[[ -x "$BUNDLE/android/virgl_test_server" && -x "$BUNDLE/debian/vkfirst" ]] ||
    { echo "no bundle in $BUNDLE (scripts/venus/build.sh)" >&2; exit 1; }
adb -s "$SERIAL" shell su <<<"$CLEANUP" >/dev/null
adb -s "$SERIAL" shell "mkdir -p /data/local/tmp/venus"
adb -s "$SERIAL" push "$BUNDLE/android/." /data/local/tmp/venus/android/ >/dev/null
adb -s "$SERIAL" push "$BUNDLE/debian/." /data/local/tmp/venus/debian/ >/dev/null
adb -s "$SERIAL" shell su <<SCRIPT
A=/data/local/tmp/venus/android
R=$ROOTFS
SOCK=\$R/tmp/.venus-test.sock
chmod 0755 \$A/* /data/local/tmp/venus/debian/vkfirst
cp -r /data/local/tmp/venus/debian \$R/tmp/venus
LD_LIBRARY_PATH=\$A RENDER_SERVER_EXEC_PATH=\$A/virgl_render_server \
    nohup \$A/virgl_test_server --venus --no-virgl --socket-path \$SOCK \
    >/data/local/tmp/venus/server.log 2>&1 </dev/null &
for i in 1 2 3 4 5 6 7 8 9 10; do [ -S \$SOCK ] && break; sleep 0.2; done
chmod 0666 \$SOCK 2>/dev/null
echo "server: \$([ -S \$SOCK ] && echo listening || echo NOT listening)"
# In Moon's mount namespace, where the Browser's root has its /proc (bound
# here for the test if the Browser has not done it). Never the full /dev:
# the Browser's root must not see /dev/video* (vimbrowser's
# docs/a26-phone-shell.md); Venus over vtest needs no device node.
nsenter -t \$(pidof a26-shell) -m -- /system/bin/sh -c '
    R=$ROOTFS; mounted=
    if ! grep -q " \$R/proc " /proc/mounts; then mount --bind /proc \$R/proc && mounted=1; fi
    chroot \$R /usr/bin/env -i PATH=/usr/bin:/bin LD_LIBRARY_PATH=/tmp/venus/lib \
        VK_ICD_FILENAMES=/tmp/venus/venus_icd.json VN_DEBUG=vtest \
        VTEST_SOCKET_NAME=/tmp/.venus-test.sock timeout 120 /tmp/venus/vkfirst $ROUNDS
    echo "vkfirst exit \$?"
    [ -n "\$mounted" ] && umount \$R/proc
'
echo "--- server log"; tail -15 /data/local/tmp/venus/server.log
SCRIPT
if [[ "$MODE" == test ]]; then
    adb -s "$SERIAL" shell su <<<"$CLEANUP"
else
    echo "server left running (--stop to end it)"
fi
