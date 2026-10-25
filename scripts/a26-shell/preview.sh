#!/usr/bin/env bash
# Runs Moon off-device on a 1080x2340 virtual X server for UI work.
#
#   scripts/a26-shell/preview.sh start   # build, start Xvfb :91 and Moon
#   scripts/a26-shell/preview.sh ctl state
#   scripts/a26-shell/preview.sh shot out.png
#   scripts/a26-shell/preview.sh stop
#
# The preview PIN is 123456. Battery and Wi-Fi are simulated under
# $PREVIEW_DIR/sys (edit those files to change the status bar). Point
# A26_SYSTEM_APP / A26_BROWSER_APP and A26_SYSTEM_ICON / A26_BROWSER_ICON at
# host builds and BGRX icons to preview real apps. XVFB selects the Xvfb
# binary and XVFB_ARGS adds arguments. Linux only, like Moon itself.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
SOURCE="$PROJECT_ROOT/src/a26-shell"
PREVIEW_DIR="${PREVIEW_DIR:-/tmp/moon-preview}"
DISPLAY_NUMBER="${PREVIEW_DISPLAY:-:91}"
BIN="$SOURCE/target/release"

start() {
    (cd "$SOURCE" && cargo build --release)
    mkdir -p "$PREVIEW_DIR"
    chmod 0700 "$PREVIEW_DIR"
    A26_SHELL_PIN=123456 python3 "$PROJECT_ROOT/scripts/a26-shell/generate-config.py" \
        "$PREVIEW_DIR/config.json"
    python3 - "$PREVIEW_DIR" <<'PY'
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
    if not target.exists():
        target.write_text(value + "\n")
PY
    if ! DISPLAY="$DISPLAY_NUMBER" "$BIN/a26-shellshot" /dev/null 2>/dev/null; then
        "${XVFB:-Xvfb}" "$DISPLAY_NUMBER" ${XVFB_ARGS:-} -screen 0 1080x2340x24 -nolisten tcp \
            >"$PREVIEW_DIR/xvfb.log" 2>&1 &
        echo $! >"$PREVIEW_DIR/xvfb.pid"
        sleep 1
    fi
    stop_moon
    MOON_STATUS_ROOT="$PREVIEW_DIR/sys" DISPLAY="$DISPLAY_NUMBER" \
        setsid "$BIN/a26-shell" --config "$PREVIEW_DIR/config.json" \
        >"$PREVIEW_DIR/moon.log" 2>&1 </dev/null &
    echo $! >"$PREVIEW_DIR/moon.pid"
    sleep 0.5
    tail -n 1 "$PREVIEW_DIR/moon.log"
}

stop_moon() {
    if [[ -f "$PREVIEW_DIR/moon.pid" ]]; then
        kill "$(cat "$PREVIEW_DIR/moon.pid")" 2>/dev/null || true
        rm -f "$PREVIEW_DIR/moon.pid"
        sleep 0.3
    fi
}

case "${1:-}" in
    start) start ;;
    ctl)
        shift
        "$BIN/a26-shellctl" --socket "$PREVIEW_DIR/control.sock" "$@"
        ;;
    shot)
        output="${2:?usage: preview.sh shot OUTPUT.png|OUTPUT.ppm}"
        DISPLAY="$DISPLAY_NUMBER" "$BIN/a26-shellshot" "$PREVIEW_DIR/shot.ppm"
        if [[ "$output" == *.ppm ]]; then
            mv "$PREVIEW_DIR/shot.ppm" "$output"
        else
            magick "$PREVIEW_DIR/shot.ppm" "$output"
        fi
        echo "$output"
        ;;
    stop)
        stop_moon
        if [[ -f "$PREVIEW_DIR/xvfb.pid" ]]; then
            kill "$(cat "$PREVIEW_DIR/xvfb.pid")" 2>/dev/null || true
            rm -f "$PREVIEW_DIR/xvfb.pid"
        fi
        ;;
    *)
        echo "usage: $0 start|ctl COMMAND...|shot OUTPUT|stop" >&2
        exit 2
        ;;
esac
