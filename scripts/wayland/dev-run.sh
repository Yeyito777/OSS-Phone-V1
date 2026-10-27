#!/usr/bin/env bash
# Moon with its Wayland server off-device (Linux), on either display path:
#
#   scripts/wayland/dev-run.sh preview start   # software path: Xvfb + Moon
#   scripts/wayland/dev-run.sh gpu start       # GPU path: Xvfb + mock compositor + Moon
#   scripts/wayland/dev-run.sh MODE ctl ...    # a26-shellctl
#   scripts/wayland/dev-run.sh MODE shot OUT   # screenshot (GPU: the mock's frame)
#   scripts/wayland/dev-run.sh MODE log        # Moon's log (the demo logs into it)
#   scripts/wayland/dev-run.sh MODE stop
#
# The System slot runs src/moon-wl-demo (a native Wayland app) unless
# A26_SYSTEM_APP says otherwise; PIN 123456. State lives under
# MOON_DEV_DIR (default ~/moon-wl-run: whale's /tmp has a quota).
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
MODE="${1:?preview|gpu}"
shift
BASE="${MOON_DEV_DIR:-$HOME/moon-wl-run}/$MODE"
mkdir -p "$BASE"
export MOON_WAYLAND_DIR="$BASE/wayland"
DEMO="$PROJECT_ROOT/src/moon-wl-demo/target/release/moon-wl-demo"
export A26_SYSTEM_APP="${A26_SYSTEM_APP:-$DEMO}"
case "$MODE" in
    preview)
        export PREVIEW_DIR="$BASE" PREVIEW_DISPLAY="${PREVIEW_DISPLAY:-:97}"
        RUN="$PROJECT_ROOT/scripts/a26-shell/preview.sh"
        ;;
    gpu)
        export MOCK_DIR="$BASE" MOCK_DISPLAY="${MOCK_DISPLAY:-:98}"
        RUN="$PROJECT_ROOT/scripts/gpu/mock-run.sh"
        ;;
    *) echo "mode is preview or gpu" >&2; exit 2 ;;
esac

case "${1:-}" in
    start)
        (cd "$PROJECT_ROOT/src/moon-wl-demo" && cargo build --release --quiet)
        rm -rf "$MOON_WAYLAND_DIR"
        mkdir -m 0700 -p "$MOON_WAYLAND_DIR"
        # Started processes inherit our stdout; keep it off the caller's
        # pipe (an ssh session would otherwise never end).
        "$RUN" start >"$BASE/start.log" 2>&1 </dev/null
        tail -n 3 "$BASE/start.log"
        for digit in 1 2 3 4 5 6; do
            "$RUN" ctl digit "$digit" >/dev/null 2>&1 || true
        done
        ;;
    log) cat "$BASE/moon.log" ;;
    *) "$RUN" "$@" ;;
esac
