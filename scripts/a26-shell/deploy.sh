#!/usr/bin/env bash
# Builds and atomically replaces only the Moon binaries on the phone, keeping
# its config.json (PIN) and audio bridge. Moon picks them up on its next start;
# pass --reboot to restart through the normal autonomous boot path.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
IMAGE="$PROJECT_ROOT/images/a26-shell-0.1.0"
BIN=/data/local/a26-linux/opt/a26-shell/bin
NAMES=(a26-shell a26-shellctl a26-shellshot a26-renderbench a26-touchpath)

"$PROJECT_ROOT/scripts/a26-shell/build.sh"

for name in "${NAMES[@]}"; do
    adb -s "$SERIAL" push "$IMAGE/bin/$name" "/data/local/tmp/$name" >/dev/null
done
adb -s "$SERIAL" shell "/data/local/tmp/su -c 'set -e; for n in ${NAMES[*]}; do cp /data/local/tmp/\$n $BIN/\$n.new; chown 0:0 $BIN/\$n.new; chmod 0755 $BIN/\$n.new; mv -f $BIN/\$n.new $BIN/\$n; done'"

phone_hashes="$(adb -s "$SERIAL" shell "/data/local/tmp/su -c 'cd $BIN && sha256sum ${NAMES[*]}'" | tr -d '\r')"
for name in "${NAMES[@]}"; do
    expected="$(sha256sum "$IMAGE/bin/$name" | awk '{print $1}')"
    grep -q "^$expected  $name$" <<<"$phone_hashes" || {
        echo "hash mismatch on phone for $name" >&2
        exit 1
    }
done
echo "Deployed Moon binaries to $SERIAL."

if [[ "${1:-}" == --reboot ]]; then
    adb -s "$SERIAL" reboot
    echo "Rebooting; Moon takes over about 90 seconds after Android boots."
fi
