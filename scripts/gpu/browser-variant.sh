#!/usr/bin/env bash
# Runs the phone's Browser with extra Chromium flags and environment, for a
# measurement or an experiment (notes/gpu/BROWSER.md), without changing any
# installed file: a copy of vimbrowser's launcher with the additions is
# bind-mounted over it in Moon's mount namespace, and the Browser is stopped.
#   scripts/gpu/browser-variant.sh on ARG...   --flag[=value] or NAME=value
#   scripts/gpu/browser-variant.sh off         unmount; the next launch is stock
# e.g. on --disable-gpu-rasterization
#      on --use-vulkan=native --enable-features=Vulkan VN_DEBUG=vtest
# Open the Browser again afterwards (a26-shellctl launch browser).
set -euo pipefail
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
FLAGS=() ENVS=() MODE="${1:-}"
case "$MODE" in
on)
    shift
    [[ $# -gt 0 ]] || { echo "on needs at least one flag or NAME=value" >&2; exit 2; }
    for arg in "$@"; do
        case "$arg" in *[!A-Za-z0-9=,._/:+-]*) echo "unexpected characters in $arg" >&2; exit 2 ;; esac
        case "$arg" in
        --*) FLAGS+=("$arg") ;;
        [A-Z_]*=*) ENVS+=("$arg") ;;
        *) echo "not a flag or NAME=value: $arg" >&2; exit 2 ;;
        esac
    done
    ;;
off) ;;
*) echo "usage: $0 on ARG... | off" >&2; exit 2 ;;
esac
# awk lines that print the additions after the anchors in the launcher.
add_flags="" add_envs=""
for f in "${FLAGS[@]+"${FLAGS[@]}"}"; do add_flags+="print \"      $f \\\\\"; "; done
for e in "${ENVS[@]+"${ENVS[@]}"}"; do add_envs+="print \"  $e \\\\\"; "; done
adb -s "$SERIAL" shell su <<SCRIPT
set -e
L=/data/local/a26-linux/opt/vimbrowser-a26/bin/vimbrowser-a26
V=/data/local/tmp/vimbrowser-a26.variant
ns() { nsenter -t \$(pidof a26-shell) -m -- "\$@"; }
ns umount \$L 2>/dev/null || true
for p in \$(pidof vimbrowser); do kill -TERM \$p; done
sleep 2
for p in \$(pidof vimbrowser); do kill -KILL \$p; done
if [ "$MODE" = on ]; then
    awk '{ print }
         /^ *--disable-gpu-sandbox \\\\\$/ { $add_flags }
         /^ *A26_VIMBROWSER_PROXY=/ { $add_envs }' \$L >\$V
    [ "\$(wc -l <\$V)" -eq "\$((\$(wc -l <\$L) + ${#FLAGS[@]} + ${#ENVS[@]}))" ]
    chmod 0755 \$V
    ns mount --bind \$V \$L
    echo "launcher with ${FLAGS[*]-} ${ENVS[*]-} mounted"
else
    rm -f \$V
    echo "stock launcher"
fi
echo "bind mounts over the launcher: \$(ns grep -c " \$L " /proc/mounts || true)"
SCRIPT
