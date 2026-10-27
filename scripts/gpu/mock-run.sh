#!/usr/bin/env bash
# Runs Moon in GPU mode off-device (Linux): Xvfb, scripts/gpu/mock-compositor.py
# in place of moon-compositor, and Moon pointed at it, like preview.sh.
#
#   scripts/gpu/mock-run.sh start    # build Moon, start Xvfb :93, mock, Moon
#   scripts/gpu/mock-run.sh ctl ...  # a26-shellctl against this Moon
#   scripts/gpu/mock-run.sh restart-moon  # only Moon again (mock and Xvfb kept)
#   scripts/gpu/mock-run.sh shot OUT # the mock's current frame as PNG/PPM
#   scripts/gpu/mock-run.sh stop
#
# PIN 123456. A26_SYSTEM_APP may name any X client to stand in for an app
# (default: xev). XVFB selects the Xvfb binary (LD_LIBRARY_PATH as needed).
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
SOURCE="$PROJECT_ROOT/src/a26-shell"
DIR="${MOCK_DIR:-/tmp/moon-gpu-mock}"
DISPLAY_NUMBER="${MOCK_DISPLAY:-:93}"
BIN="$SOURCE/target/release"

stop() {
    for name in moon mock xvfb; do
        if [[ -f "$DIR/$name.pid" ]]; then
            kill "$(cat "$DIR/$name.pid")" 2>/dev/null || true
            rm -f "$DIR/$name.pid"
        fi
    done
    sleep 0.3
}

start() {
    (cd "$SOURCE" && cargo build --release)
    stop
    mkdir -p "$DIR"
    chmod 0700 "$DIR"
    A26_SHELL_PIN=123456 python3 "$PROJECT_ROOT/scripts/a26-shell/generate-config.py" \
        "$DIR/config.json"
    python3 - "$DIR" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
config = json.loads((root / "config.json").read_text())
config["socket_path"] = str(root / "control.sock")
(root / "config.json").write_text(json.dumps(config, indent=2) + "\n")
for path, value in {
    "sys/class/power_supply/battery/capacity": "82",
    "sys/class/power_supply/battery/status": "Discharging",
    "sys/class/net/wlan0/operstate": "up",
    "sys/class/net/wlan0/carrier": "1",
}.items():
    target = root / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(value + "\n")
PY
    "${XVFB:-Xvfb}" "$DISPLAY_NUMBER" -screen 0 1080x2340x24 -nolisten tcp \
        >"$DIR/xvfb.log" 2>&1 &
    echo $! >"$DIR/xvfb.pid"
    python3 "$PROJECT_ROOT/scripts/gpu/mock-compositor.py" --socket "$DIR/compositor.sock" \
        --shot "$DIR/frame.ppm" --log "$DIR/mock.log" ${MOCK_ARGS:-} &
    echo $! >"$DIR/mock.pid"
    sleep 1.5
    MOON_GPU_SOCKET="$DIR/compositor.sock" MOON_STATUS_ROOT="$DIR/sys" \
        DISPLAY="$DISPLAY_NUMBER" A26_SYSTEM_APP="${A26_SYSTEM_APP:-$(command -v xev)}" \
        setsid "$BIN/a26-shell" --config "$DIR/config.json" \
        >"$DIR/moon.log" 2>&1 </dev/null &
    echo $! >"$DIR/moon.pid"
    sleep 1
    tail -n 5 "$DIR/moon.log"
}

# Only Moon again, the rest (mock, Xvfb) kept, as the supervisor restarts
# it: whatever the Moon before left (the screen off) stays.
restart_moon() {
    if [[ -f "$DIR/moon.pid" ]]; then
        kill "$(cat "$DIR/moon.pid")" 2>/dev/null || true
        sleep 0.5
    fi
    MOON_GPU_SOCKET="$DIR/compositor.sock" MOON_STATUS_ROOT="$DIR/sys" \
        DISPLAY="$DISPLAY_NUMBER" A26_SYSTEM_APP="${A26_SYSTEM_APP:-$(command -v xev)}" \
        setsid "$BIN/a26-shell" --config "$DIR/config.json" \
        >"$DIR/moon.log" 2>&1 </dev/null &
    echo $! >"$DIR/moon.pid"
    sleep 1
    tail -n 5 "$DIR/moon.log"
}

case "${1:-}" in
    start) start ;;
    restart-moon) restart_moon ;;
    stop) stop ;;
    ctl)
        shift
        "$BIN/a26-shellctl" --socket "$DIR/control.sock" "$@"
        ;;
    shot)
        output="${2:?usage: mock-run.sh shot OUTPUT}"
        rm -f "$DIR/frame.ppm"
        kill -USR1 "$(cat "$DIR/mock.pid")"
        for _ in $(seq 1 50); do [[ -s "$DIR/frame.ppm" ]] && break; sleep 0.1; done
        if [[ "$output" == *.ppm ]]; then
            cp "$DIR/frame.ppm" "$output"
        else
            python3 -c 'import sys
from PIL import Image
Image.open(sys.argv[1]).save(sys.argv[2])' "$DIR/frame.ppm" "$output" 2>/dev/null ||
                magick "$DIR/frame.ppm" "$output"
        fi
        tail -n 1 "$DIR/mock.log"
        ;;
    *)
        echo "usage: $0 start|restart-moon|ctl ...|shot OUTPUT|stop" >&2
        exit 2
        ;;
esac
