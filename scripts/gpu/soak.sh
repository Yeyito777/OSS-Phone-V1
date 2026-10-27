#!/usr/bin/env bash
# Robustness soak of GPU mode on the attached phone (notes/gpu/DESIGN.md):
#   scripts/gpu/soak.sh cycles [N]   N screen off/on cycles through Moon (default 50)
#   scripts/gpu/soak.sh crash        one compositor crash (SIGSEGV): the keeper
#                                    restarts it and Moon reconnects
#   scripts/gpu/soak.sh sample       one line of the compositor's footprint
# Each run prints the compositor's footprint (RSS, open fds, dmabufs, Moon's
# RSS) before and after, panel on/off timings and failures from its log, and
# ends with the screen off and the phone locked. The phone must be unlocked
# or locked, either works; the panel goes on and off.
set -euo pipefail
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
MODE="${1:-cycles}"
N="${2:-50}"
case "$MODE" in cycles|crash|sample) ;; *) echo "usage: $0 cycles [N] | crash | sample" >&2; exit 2 ;; esac
case "$N" in ''|*[!0-9]*) echo "N must be a number" >&2; exit 2 ;; esac
adb -s "$SERIAL" shell su <<EOF
R=/data/local/a26-linux; RUN=/data/local/tmp/moon-gpu; BB=\$R/busybox.static
ctl() { \$BB chroot \$R /opt/a26-shell/bin/a26-shellctl "\$@" >/dev/null; }
sample() {
    p=\$(cat \$RUN/compositor.pid)
    rss=\$(awk '/VmRSS/ {print \$2}' /proc/\$p/status)
    fds=\$(ls /proc/\$p/fd | wc -l)
    dmabufs=\$(ls -l /proc/\$p/fd 2>/dev/null | grep -c dmabuf)
    moon=\$(awk '/VmRSS/ {print \$2}' /proc/\$(pidof a26-shell)/status)
    echo "\$1: compositor pid \$p rss \${rss} kB, fds \$fds, dmabufs \$dmabufs; Moon rss \${moon} kB; up \$(cut -d' ' -f1 /proc/uptime)"
}
mark=\$(wc -l < \$RUN/compositor.log)
smark=\$(wc -l < \$RUN/session.log)
sample before
case $MODE in
cycles)
    for i in \$(\$BB seq 1 $N); do
        ctl screen on; sleep 1.2
        ctl screen off; sleep 1.2
    done ;;
crash)
    ctl screen on; sleep 1
    old=\$(cat \$RUN/compositor.pid)
    kill -SEGV \$old
    for i in \$(\$BB seq 1 50); do
        p=\$(cat \$RUN/compositor.pid 2>/dev/null)
        [ -n "\$p" ] && [ "\$p" != "\$old" ] && [ -e \$RUN/ready ] && break
        sleep 0.1
    done
    sleep 2
    echo "restarted: \$([ "\$(cat \$RUN/compositor.pid)" != "\$old" ] && echo yes || echo NO) in about \$((i * 100)) ms"
    ctl screen off; sleep 1 ;;
sample) ;;
esac
ctl lock
sample after
new=\$(tail -n +\$((mark + 1)) \$RUN/compositor.log)
echo "panel on:  \$(echo "\$new" | grep -c 'panel on: ok') ok, \$(echo "\$new" | grep 'panel on' | grep -vc ': ok') failed"
echo "panel off: \$(echo "\$new" | grep -c 'panel off: ok') ok, \$(echo "\$new" | grep 'panel off' | grep -vc ': ok') failed"
echo "\$new" | grep -o 'panel on: ok in [0-9.]* ms' | awk '{t=\$5; s+=t; if (t>m) m=t; n++} END {if (n) printf "panel on: mean %.1f ms, max %.1f ms\n", s/n, m}'
echo "\$new" | grep -o 'panel off: ok in [0-9.]* ms' | awk '{t=\$5; s+=t; if (t>m) m=t; n++} END {if (n) printf "panel off: mean %.1f ms, max %.1f ms\n", s/n, m}'
echo "--- compositor log lines that are not routine:"
echo "\$new" | grep -v -E 'panel (on|off): ok|client (connected|hello)|^kms:|^gpu:|mirror:|ready in|scaling planes' | tail -12
echo "--- session log:"
tail -n +\$((smark + 1)) \$RUN/session.log | tail -6
EOF
