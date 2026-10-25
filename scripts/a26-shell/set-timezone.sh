#!/usr/bin/env bash
# Sets (or clears) Moon's clock time zone in the phone's config.json without
# touching the PIN fields. Moon reads it at its next start.
#
#   scripts/a26-shell/set-timezone.sh 'EST5EDT,M3.2.0,M11.1.0'
#   scripts/a26-shell/set-timezone.sh --clear
#
# The value is a POSIX TZ rule, or a zone name such as America/Toronto when
# the Alpine rootfs has tzdata installed.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
SERIAL="$(a26_resolve_serial)"
CONFIG=/data/local/a26-linux/etc/a26-shell/config.json
ZONE="${1:?usage: $0 TZ-RULE|--clear}"
[[ "$ZONE" == --clear || "$ZONE" =~ ^[A-Za-z0-9_/,.:+<>-]{1,64}$ ]] || {
    echo "unsupported time zone value: $ZONE" >&2
    exit 2
}

current="$(adb -s "$SERIAL" exec-out "/data/local/tmp/su -c 'cat $CONFIG'")"
updated="$(python3 -c '
import json, sys
config = json.loads(sys.stdin.read())
if sys.argv[1] == "--clear":
    config.pop("timezone", None)
else:
    config["timezone"] = sys.argv[1]
print(json.dumps(config, indent=2))
' "$ZONE" <<<"$current")"
printf '%s\n' "$updated" | adb -s "$SERIAL" exec-in \
    "/data/local/tmp/su -c 'cat >$CONFIG.new && chown 0:0 $CONFIG.new && chmod 0600 $CONFIG.new && mv -f $CONFIG.new $CONFIG'"
adb -s "$SERIAL" exec-out "/data/local/tmp/su -c 'cat $CONFIG'" \
    | python3 -c 'import json, sys; print("timezone =", json.load(sys.stdin).get("timezone"))'
