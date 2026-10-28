#!/usr/bin/env bash
# Lets the phone's Exocortex borrow whale's Claude login, and nothing else
# (docs/EXOCORTEX.md):
#
#   scripts/exocortex/authorize-whale.sh [--check]
#
# Adds the phone's own key (made on the phone by install.sh; the private half
# never leaves it) to whale's ~/.ssh/authorized_keys as
#
#   restrict,command="… exocortexd proxy --link-only=anthropic" ssh-ed25519 … oss-phone-exocortex
#
# `restrict`: no pty, no port/agent/X11 forwarding, no ~/.ssh/rc. `command=`:
# whatever the phone asks for, sshd runs only Exocortex's link-only bridge,
# which passes only the /proxy model link for Claude to whale's daemon
# (Exocortex docs/provider-proxy.md, "A key that can only borrow the login").
# The bridge runs from its own clone (~/.local/share/exocortex-link-only), so
# whale's own checkout and daemon are untouched; it reaches whale's daemon
# through that install's config directory. An earlier entry for the same key
# comment is replaced; authorized_keys is backed up first.
#
# Then, from the phone (check-whale-link.sh): the link answers, everything
# else is refused, and no shell, pty, command, forwarding or sftp is possible.
# --check runs only those checks.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"

WHALE="${EXOCORTEX_WHALE:-whale}"
TAG=oss-phone-exocortex
BRANCH="${EXOCORTEX_LINK_BRANCH:-main}"
CHECK=0
[[ "${1:-}" != --check ]] || CHECK=1

SERIAL="$(a26_resolve_serial)"
ROOT=/data/local/a26-linux
su_run() { adb -s "$SERIAL" shell "/data/local/tmp/su -c '$1'" | tr -d '\r'; }

if [[ "$CHECK" == 0 ]]; then
    pubkey="$(su_run "cat $ROOT/root/.ssh/exocortex_whale_ed25519.pub")"
    [[ "$pubkey" =~ ^ssh-ed25519\ [A-Za-z0-9+/]+=*\ $TAG$ ]] || { echo "unexpected phone key: $pubkey" >&2; exit 1; }

    home="$(ssh "$WHALE" 'printf %s "$HOME"')"
    [[ "$home" =~ ^/[A-Za-z0-9/._-]+$ ]] || { echo "unexpected HOME on $WHALE: $home" >&2; exit 1; }
    clone="$home/.local/share/exocortex-link-only"
    config="$home/Workspace/exocortex/config"
    entry="restrict,command=\"env EXOCORTEX_CONFIG_DIR=$config $clone/bin/exocortexd proxy --link-only=anthropic\" $pubkey"

    echo "== $WHALE: the link-only bridge's clone ($clone, $BRANCH)"
    ssh "$WHALE" bash -s -- "$clone" "$BRANCH" <<'EOF'
set -euo pipefail
clone=$1 branch=$2
if [[ -d "$clone/.git" ]]; then
    git -C "$clone" fetch -q origin "$branch"
    git -C "$clone" checkout -q "$branch" 2>/dev/null || git -C "$clone" checkout -q -b "$branch" "origin/$branch"
    git -C "$clone" merge -q --ff-only "origin/$branch"
else
    mkdir -p "$(dirname "$clone")"
    git clone -q --branch "$branch" https://github.com/Yeyito777/Exocortex.git "$clone"
fi
cd "$clone"
PATH="$HOME/.bun/bin:$HOME/.local/bun/bin:$PATH" bun install --frozen-lockfile >/dev/null
grep -q -- '--link-only' daemon/src/proxy-main.ts
echo "  bridge at $(git rev-parse --short HEAD)"
EOF

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
