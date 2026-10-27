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
NAMES=(a26-shell a26-shellctl a26-shellshot a26-renderbench a26-touchpath moon-notifyd moon-notifyctl)

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
# Fallback fonts: pushed only when the phone's copy differs (they are large
# and rarely change).
FONTS=/data/local/a26-linux/opt/a26-shell/share/fonts
phone_fonts="$(adb -s "$SERIAL" shell "/data/local/tmp/su -c 'mkdir -p $FONTS && cd $FONTS && sha256sum * 2>/dev/null; true'" | tr -d '\r')"
for font in "$IMAGE"/share/fonts/*; do
    name="$(basename "$font")"
    expected="$(sha256sum "$font" | awk '{print $1}')"
    if ! grep -q "^$expected  $name$" <<<"$phone_fonts"; then
        adb -s "$SERIAL" push "$font" "/data/local/tmp/$name" >/dev/null
        adb -s "$SERIAL" shell "/data/local/tmp/su -c 'cp /data/local/tmp/$name $FONTS/$name.new && chmod 0644 $FONTS/$name.new && mv -f $FONTS/$name.new $FONTS/$name && rm -f /data/local/tmp/$name'"
        echo "font updated: $name"
    fi
done
adb -s "$SERIAL" push "$IMAGE/share/VERSION" /data/local/tmp/moon-VERSION >/dev/null
adb -s "$SERIAL" shell "/data/local/tmp/su -c 'cp /data/local/tmp/moon-VERSION /data/local/a26-linux/opt/a26-shell/share/VERSION && chmod 0644 /data/local/a26-linux/opt/a26-shell/share/VERSION && rm /data/local/tmp/moon-VERSION'"
echo "Deployed Moon binaries to $SERIAL."

if [[ "${1:-}" == --reboot ]]; then
    adb -s "$SERIAL" reboot
    echo "Rebooting; Moon takes over about 90 seconds after Android boots."
fi
