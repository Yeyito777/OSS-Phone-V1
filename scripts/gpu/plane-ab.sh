#!/usr/bin/env bash
# The display planes on the panel itself, for someone looking at it: the
# running compositor switches between the planes path and GPU composition
# of the same frame every 1.5 s (SIGHUP), for ROUNDS rounds, ending with
# planes as they were. Identical paths show a still picture; any jump,
# shear or streak at a switch means the planes fetch some layer wrongly.
# A test notification is posted first (a layer that does not start at the
# top: the case that once came out wrong) and dismissed after.
#   scripts/gpu/plane-ab.sh [ROUNDS]
set -euo pipefail
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
ROUNDS="${1:-5}"
adb -s "$SERIAL" shell su <<EOF
R=/data/local/a26-linux
RUN=/data/local/tmp/moon-gpu
notify() {
    A26_ROOT=\$R A26_BUSYBOX=\$R/busybox.static /system/bin/sh \$R/a26-enter-chroot.sh \
        /opt/a26-shell/bin/moon-notifyctl "\$@"
}
pid=\$(cat \$RUN/compositor.pid)
id=\$(notify notify.post app=system title=Planes body='Plane test card' | sed -n 's/.*"id": *"\([^"]*\)".*/\1/p')
sleep 2
for i in \$(seq 1 $((ROUNDS * 2))); do
    kill -HUP \$pid
    sleep 1.5
done
grep -E "planes (on|off)" \$RUN/compositor.log | tail -2
[ -n "\$id" ] && notify notify.dismiss notification=\$id >/dev/null
echo "notification \${id:-?} posted and dismissed"
EOF
