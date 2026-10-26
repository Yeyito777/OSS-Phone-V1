#!/usr/bin/env bash
# Inspect Moon's GPU display path on the attached phone.
#
#   scripts/gpu/phone.sh status        # mode, compositor tier, recent logs
#   scripts/gpu/phone.sh shot OUT.png  # the frame on screen, as composed
#   scripts/gpu/phone.sh log [N]       # last N compositor log lines
#   scripts/gpu/phone.sh kill          # SIGKILL the compositor (fallback tests)
#   scripts/gpu/phone.sh profile       # toggle the compositor's per-flip log
set -euo pipefail
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
RUN=/data/local/tmp/moon-gpu
su_sh() { adb -s "$SERIAL" shell su; }

case "${1:-status}" in
status)
    su_sh <<EOF
echo "config: \$(grep GPU_COMPOSITOR /data/adb/moon/config 2>/dev/null || echo 'GPU_COMPOSITOR unset (default: on)')"
echo "active: \$([ -e $RUN/active ] && echo yes || echo no)  failed-boot: \$(cat $RUN/failed-boot 2>/dev/null || echo -)"
echo "compositor: \$(cat $RUN/ready 2>/dev/null | tr '\n' ' ')"
echo "Xorg: \$(pidof Xorg) moon-compositor: \$(pidof moon-compositor)"
echo "--- session"; tail -5 $RUN/session.log 2>/dev/null
echo "--- compositor"; tail -8 $RUN/compositor.log 2>/dev/null
echo "--- moon"; grep -E "^gpu|compositor:|panel power" /data/local/a26-linux/root/a26-shell.log | tail -8
EOF
    ;;
shot)
    out="${2:?usage: phone.sh shot OUT.png}"
    su_sh <<EOF
rm -f $RUN/shot.ppm
kill -USR1 \$(cat $RUN/compositor.pid)
last=-1
for i in \$(seq 1 50); do
    size=\$(stat -c %s $RUN/shot.ppm 2>/dev/null || echo 0)
    [ "\$size" -gt 1000000 ] && [ "\$size" = "\$last" ] && break
    last=\$size
    sleep 0.2
done
cp $RUN/shot.ppm /data/local/tmp/moon-gpu-shot.ppm && chmod 0644 /data/local/tmp/moon-gpu-shot.ppm
EOF
    adb -s "$SERIAL" pull /data/local/tmp/moon-gpu-shot.ppm /tmp/moon-gpu-shot.ppm >/dev/null
    adb -s "$SERIAL" shell rm -f /data/local/tmp/moon-gpu-shot.ppm
    magick /tmp/moon-gpu-shot.ppm "$out"
    echo "wrote $out"
    ;;
log)
    su_sh <<<"tail -${2:-40} $RUN/compositor.log"
    ;;
kill)
    su_sh <<<"kill -KILL \$(cat $RUN/compositor.pid); echo killed"
    ;;
profile)
    su_sh <<<"kill -USR2 \$(cat $RUN/compositor.pid); sleep 0.3; tail -1 $RUN/compositor.log"
    ;;
*)
    echo "usage: $0 status|shot OUT.png|log [N]|kill|profile" >&2
    exit 2
    ;;
esac
