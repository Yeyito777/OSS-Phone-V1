#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"

adb -s "$SERIAL" get-state >/dev/null
if [[ -z "$(adb -s "$SERIAL" shell '/data/local/tmp/su -c "pidof Xorg 2>/dev/null || true"' | tr -d '\r')" ]]; then
    "$PROJECT_ROOT/scripts/a26-shell/audio-start.sh"
    "$PROJECT_ROOT/scripts/start-native-xorg.sh"
fi

"$PROJECT_ROOT/scripts/a26-shell/start.sh"
"$PROJECT_ROOT/scripts/a26-shell/wifi-start.sh" ||
    echo 'native Wi-Fi start failed; continuing without Wi-Fi' >&2
adb -s "$SERIAL" shell '/data/local/tmp/su -c "setprop sys.rescue_boot_count 0; setprop sys.rescue_boot_start 0"'

echo "A26 native desktop is ready (Xorg + a26-shell)."
