#!/usr/bin/env bash
# Gives the phone's Exocortex full access to whale, like the user's Mac
# (docs/EXOCORTEX.md):
#
#   scripts/exocortex/authorize-whale.sh [--check]
#
# Adds the phone's own key (made on the phone by install.sh; the private half
# never leaves it) to whale's ~/.ssh/authorized_keys as a plain entry,
#
#   ssh-ed25519 … oss-phone-exocortex
#
# with no forced command or restrictions: the phone's daemon borrows whale's
# Claude login over the ordinary /proxy bridge (`ssh whale exocortexd proxy`),
# gets remote_bash on whale, and the phone's agent can ssh to whale. An earlier
# entry for the same key comment is replaced; authorized_keys is backed up
# first.
#
# Then, from the phone (check-whale-link.sh): the /proxy link answers with the
# full protocol and whale is logged in to Claude, and a command runs on whale.
# --check runs only those checks.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"

WHALE="${EXOCORTEX_WHALE:-whale}"
TAG=oss-phone-exocortex
CHECK=0
[[ "${1:-}" != --check ]] || CHECK=1

SERIAL="$(a26_resolve_serial)"
ROOT=/data/local/a26-linux
su_run() { adb -s "$SERIAL" shell "/data/local/tmp/su -c '$1'" | tr -d '\r'; }

if [[ "$CHECK" == 0 ]]; then
    pubkey="$(su_run "cat $ROOT/root/.ssh/exocortex_whale_ed25519.pub")"
    [[ "$pubkey" =~ ^ssh-ed25519\ [A-Za-z0-9+/]+=*\ $TAG$ ]] || { echo "unexpected phone key: $pubkey" >&2; exit 1; }

    entry="$pubkey"

    echo "== $WHALE: authorized_keys"
    # The script comes on stdin, so the entry travels as an argument (base64:
    # it holds quotes and spaces).
    ssh "$WHALE" bash -s -- "$TAG" "$(printf '%s' "$entry" | base64 | tr -d '\n')" <<'EOF'
set -euo pipefail
tag=$1
entry="$(printf '%s' "$2" | base64 -d)"
keys="$HOME/.ssh/authorized_keys"
cp -p "$keys" "$keys.bak-$(date +%Y%m%d%H%M%S)"
{ grep -v " $tag\$" "$keys" || true; printf '%s\n' "$entry"; } >"$keys.new"
chmod 0600 "$keys.new"
mv -f "$keys.new" "$keys"
echo "  $(grep -c " $tag\$" "$keys") entry for $tag:"
grep " $tag\$" "$keys" | sed 's/ssh-ed25519 \([A-Za-z0-9+/]\{12\}\)[^ ]*/ssh-ed25519 \1…/; s/^/    /'
EOF
fi

echo "== checks from the phone"
adb -s "$SERIAL" push "$PROJECT_ROOT/scripts/device/exocortex/check-whale-link.sh" /data/local/tmp/check-whale-link.sh >/dev/null
su_run "cd / && $ROOT/busybox.static unshare -m --propagation private /system/bin/sh /data/adb/exocortex/exocortex-ns.sh exec /bin/bash -s </data/local/tmp/check-whale-link.sh; rc=\$?; rm -f /data/local/tmp/check-whale-link.sh; exit \$rc"
