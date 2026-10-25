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
ZONE_PATTERN='^[A-Za-z0-9_/,.:+<>-]{1,64}$'
[[ "$ZONE" == --clear || "$ZONE" =~ $ZONE_PATTERN ]] || {
    echo "unsupported time zone value: $ZONE" >&2
    exit 2
}

current="$(adb -s "$SERIAL" exec-out "/data/local/tmp/su -c 'cat $CONFIG'")"
updated="$(python3 -c '
import json, sys
config = json.loads(sys.stdin.read())
# Never write back a config that lost the PIN.
assert all(key in config for key in ("pin_salt_hex", "pin_hash_hex", "pin_length"))
if sys.argv[1] == "--clear":
    config.pop("timezone", None)
else:
    config["timezone"] = sys.argv[1]
print(json.dumps(config, indent=2))
' "$ZONE" <<<"$current")"
# adb shell (not exec-in) so the whole command chain runs under su.
printf '%s\n' "$updated" | adb -s "$SERIAL" shell \
    "/data/local/tmp/su -c 'umask 077; cat >$CONFIG.new && chown 0:0 $CONFIG.new && chmod 0600 $CONFIG.new && mv -f $CONFIG.new $CONFIG'"
adb -s "$SERIAL" exec-out "/data/local/tmp/su -c 'cat $CONFIG'" | python3 -c '
import json, sys
zone = json.load(sys.stdin).get("timezone")
print("timezone =", zone)
sys.exit(0 if zone == (None if sys.argv[1] == "--clear" else sys.argv[1]) else 1)
' "$ZONE"
