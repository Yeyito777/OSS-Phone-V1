#!/usr/bin/env bash
# Moon on Xwayland off-device (milestone 4): no Xorg or Xvfb. Moon starts its
# Wayland server, then Xwayland (scripts/wayland/xwayland-podman.sh: Alpine's
# Xwayland in a container), and connects to it as its X server. Touches come
# from a FIFO (MOON_TOUCH_REPLAY; scripts/wayland/touch-replay.py writes it).
#
#   scripts/wayland/xwayland-run.sh start      # Moon + Xwayland (+ the mock compositor with GPU=1)
#   scripts/wayland/xwayland-run.sh ctl ...    # a26-shellctl
#   scripts/wayland/xwayland-run.sh touch ...  # touch-replay.py ACTION ARGS
#   scripts/wayland/xwayland-run.sh shot OUT   # X root screenshot (PPM)
#   scripts/wayland/xwayland-run.sh x CMD...   # run an X client on Moon's Xwayland
#   scripts/wayland/xwayland-run.sh log | stop
#
# PIN 123456. State lives under MOON_DEV_DIR (default ~/moon-wl-run/xwayland).
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="$PROJECT_ROOT/src/a26-shell/target/release"
BASE="${MOON_DEV_DIR:-$HOME/moon-wl-run}/xwayland"
FIFO="$BASE/touch.fifo"
mkdir -p "$BASE"

display() { sed -n 's/^xwayland: display \(:[0-9]*\) ready.*/\1/p' "$BASE/moon.log" | tail -n 1; }
ctl() { "$BIN/a26-shellctl" --socket "$BASE/control.sock" "$@"; }

stop() {
    # Moon leads its own process group (setsid): its apps and Xwayland go
    # with it, as the supervisor stops a session.
    for name in moon mock; do
        if [[ -f "$BASE/$name.pid" ]]; then
            pid="$(cat "$BASE/$name.pid")"
            kill -TERM -- "-$pid" 2>/dev/null || kill "$pid" 2>/dev/null || true
            rm -f "$BASE/$name.pid"
        fi
    done
    sleep 0.5
}

case "${1:-}" in
start)
    stop
    (cd "$PROJECT_ROOT/src/a26-shell" && cargo build --release --quiet)
    A26_SHELL_PIN=123456 python3 "$PROJECT_ROOT/scripts/a26-shell/generate-config.py" "$BASE/config.json"
    python3 - "$BASE" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
config = json.loads((root / "config.json").read_text())
config["socket_path"] = str(root / "control.sock")
(root / "config.json").write_text(json.dumps(config, indent=2) + "\n")
for path, value in {"sys/class/power_supply/battery/capacity": "82",
                    "sys/class/power_supply/battery/status": "Discharging",
                    "sys/class/net/wlan0/operstate": "up", "sys/class/net/wlan0/carrier": "1"}.items():
    target = root / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(value + "\n")
PY
    # Auto-Lock off: tests run longer than 30 s.
    echo '{"version":1,"wallpaper":"moon","auto_lock_s":0}' >"$BASE/settings.json"
    rm -rf "$BASE/wayland" && mkdir -m 0700 "$BASE/wayland"
    rm -f "$FIFO" && mkfifo "$FIFO"
    gpu_env=()
    if [[ "${GPU:-0}" == 1 ]]; then
        python3 "$PROJECT_ROOT/scripts/gpu/mock-compositor.py" --socket "$BASE/gpu.sock" \
            --shot "$BASE/gpu-shot.ppm" ${MOCK_ARGS:-} >"$BASE/mock.log" 2>&1 </dev/null &
        echo $! >"$BASE/mock.pid"
        sleep 1
        gpu_env=(MOON_GPU_SOCKET="$BASE/gpu.sock")
    fi
    env -u DISPLAY -u WAYLAND_DISPLAY "${gpu_env[@]}" \
        MOON_STATUS_ROOT="$BASE/sys" MOON_WAYLAND_DIR="$BASE/wayland" MOON_SETTINGS="$BASE/settings.json" \
        MOON_XWAYLAND="${MOON_XWAYLAND:-$PROJECT_ROOT/scripts/wayland/xwayland-podman.sh}" \
        MOON_TOUCH_REPLAY="$FIFO:1079,2339" \
        setsid "$BIN/a26-shell" --config "$BASE/config.json" >"$BASE/moon.log" 2>&1 </dev/null &
    echo $! >"$BASE/moon.pid"
    for _ in $(seq 1 100); do [[ -S "$BASE/control.sock" ]] && ctl ping >/dev/null 2>&1 && break; sleep 0.2; done
    grep -E '^(xwayland|touch|wayland: Xwayland)' "$BASE/moon.log" || true
    for digit in 1 2 3 4 5 6; do ctl digit "$digit" >/dev/null 2>&1 || true; done
    ;;
ctl) shift; ctl "$@" ;;
touch) shift; python3 "$PROJECT_ROOT/scripts/wayland/touch-replay.py" "$FIFO" "$@" ;;
shot) DISPLAY="$(display)" "$BIN/a26-shellshot" "${2:?usage: shot OUT.ppm}" ;;
x) shift; DISPLAY="$(display)" "$@" ;;
log) cat "$BASE/moon.log" ;;
stop) stop ;;
*) sed -n '2,13p' "$0"; exit 2 ;;
esac
