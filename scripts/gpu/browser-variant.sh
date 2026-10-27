#!/usr/bin/env bash
# Runs the phone's Browser with one extra Chromium flag, for a measurement
# (notes/gpu/BROWSER.md), without changing any installed file: a copy of
# vimbrowser's launcher with the flag added is bind-mounted over it in Moon's
# mount namespace, and the Browser is stopped.
#   scripts/gpu/browser-variant.sh on FLAG    e.g. --disable-gpu-compositing
#   scripts/gpu/browser-variant.sh off        unmount; the next launch is stock
# Open the Browser again afterwards (a26-shellctl launch browser).
set -euo pipefail
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
case "${1:-}" in
on) FLAG="${2:?flag required}" ;;
off) FLAG= ;;
*) echo "usage: $0 on FLAG | off" >&2; exit 2 ;;
esac
case "$FLAG" in *[!A-Za-z0-9=,._-]*) echo "unexpected characters in $FLAG" >&2; exit 2 ;; esac
adb -s "$SERIAL" shell su <<SCRIPT
set -e
L=/data/local/a26-linux/opt/vimbrowser-a26/bin/vimbrowser-a26
V=/data/local/tmp/vimbrowser-a26.variant
ns() { nsenter -t \$(pidof a26-shell) -m -- "\$@"; }
ns umount \$L 2>/dev/null || true
for p in \$(pidof vimbrowser); do kill -TERM \$p; done
sleep 2
for p in \$(pidof vimbrowser); do kill -KILL \$p; done
if [ -n "$FLAG" ]; then
    awk '{ print } /^ *--disable-gpu-sandbox \\\\\$/ { print "      $FLAG \\\\" }' \$L >\$V
    grep -q -e '^ *$FLAG ' \$V
    chmod 0755 \$V
    ns mount --bind \$V \$L
    echo "launcher with $FLAG mounted"
else
    rm -f \$V
    echo "stock launcher"
fi
echo "bind mounts over the launcher: \$(ns grep -c " \$L " /proc/mounts || true)"
SCRIPT
