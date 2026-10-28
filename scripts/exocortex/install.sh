#!/usr/bin/env bash
# Installs or updates Exocortex on the attached phone (docs/EXOCORTEX.md):
#
#   scripts/exocortex/install.sh [--branch NAME] [--openai-key] [--no-start]
#
#   1. the keeper (Android side: /data/adb/exocortex, the Magisk service.d
#      hook) and the chroot commands (exocortexd, exocortex-supervisor,
#      android-sh)
#   2. inside the chroot (scripts/device/exocortex/phone-setup.sh): Alpine
#      packages, Bun (pinned), Claude Code, the Exocortex checkout in
#      /opt/exocortex with its config (system.md, Claude through /proxy whale),
#      the phone's own SSH key for whale, /opt/src sources
#   3. --openai-key: OPENAI_API_KEY from the Mac `secrets` store, over adb
#      stdin into a root-only file, then `exocortexd login openai key` from it
#   4. starts (or restarts) the keeper and daemon
#
# Idempotent. whale's side (the restricted authorized_keys entry for the
# phone's key) is scripts/exocortex/authorize-whale.sh.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
DEVICE="$PROJECT_ROOT/scripts/device/exocortex"

BRANCH=main
OPENAI_KEY=0
START=1
while [[ $# -gt 0 ]]; do
    case "$1" in
        --branch) BRANCH="${2:?--branch needs a name}"; shift ;;
        --openai-key) OPENAI_KEY=1 ;;
        --no-start) START=0 ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done

BUN_VERSION=1.3.14
BUN_SHA256=b98e0ad3625c5c00d1d5b5ff55605c7adddbfae151861e68ade57b2d3b8703bb
# whale's host key, from this Mac's known_hosts (trusted), pinned on the phone.
WHALE_HOST_KEY="$(ssh-keygen -F yeyito.dev 2>/dev/null | awk '!/^#/ && $2 == "ssh-ed25519" { print $2, $3; exit }')"
[[ -n "$WHALE_HOST_KEY" ]] || { echo "no ssh-ed25519 host key for yeyito.dev in ~/.ssh/known_hosts" >&2; exit 1; }

SERIAL="$(a26_resolve_serial)"
ROOT=/data/local/a26-linux
STAGE=/data/local/tmp/exocortex-stage
su_run() { adb -s "$SERIAL" shell "/data/local/tmp/su -c '$1'" | tr -d '\r'; }
# A command in the daemon's environment: its own mount namespace and chroot
# (exocortex-ns.sh exec). No single quotes in it.
in_ns() { su_run "cd / && $ROOT/busybox.static unshare -m --propagation private /system/bin/sh /data/adb/exocortex/exocortex-ns.sh exec $1"; }
step() { printf '\n== %s\n' "$*"; }

for script in "$DEVICE"/*.sh "$DEVICE/exocortex-supervisor" "$DEVICE/exocortexd" "$DEVICE/android-sh"; do
    sh -n "$script"
done
bash -n "$DEVICE/phone-setup.sh"

step "preflight ($SERIAL)"
[[ "$(su_run 'id -u')" == 0 ]] || { echo "no root through /data/local/tmp/su" >&2; exit 1; }
for path in "$ROOT/busybox.static" "$ROOT/etc/alpine-release" /data/adb/service.d; do
    [[ "$(su_run "[ -e $path ] && echo yes")" == yes ]] || { echo "missing on the phone: $path" >&2; exit 1; }
done

step "keeper and chroot commands"
adb -s "$SERIAL" shell "rm -rf $STAGE; umask 077; mkdir $STAGE"
for file in exocortex-keeper.sh exocortex-ns.sh 95-exocortex.sh exocortex-supervisor exocortexd android-sh phone-setup.sh system.md; do
    adb -s "$SERIAL" push "$DEVICE/$file" "$STAGE/$file" >/dev/null
done
su_run "set -eu
stage=$STAGE root=$ROOT
put() { cp \"\$stage/\$1\" \"\$2.new\"; chown 0:0 \"\$2.new\"; chmod \"\$3\" \"\$2.new\"; mv -f \"\$2.new\" \"\$2\"; }
mkdir -p /data/adb/exocortex \$root/var/lib/exocortex-keeper \$root/var/log/exocortex \$root/usr/local/bin
chmod 0700 /data/adb/exocortex \$root/var/lib/exocortex-keeper
put exocortex-keeper.sh /data/adb/exocortex/exocortex-keeper.sh 0700
put exocortex-ns.sh /data/adb/exocortex/exocortex-ns.sh 0700
put 95-exocortex.sh /data/adb/service.d/95-exocortex.sh 0755
for name in exocortex-supervisor exocortexd android-sh; do put \$name \$root/usr/local/bin/\$name 0755; done
# Not under /tmp: the daemon namespace mounts its own /tmp.
mkdir -p \$root/var/lib/exocortex-keeper/install
cp \$stage/phone-setup.sh \$stage/system.md \$root/var/lib/exocortex-keeper/install/
echo installed"

step "runtime and Exocortex (inside the chroot)"
in_ns "BUN_VERSION=$BUN_VERSION BUN_SHA256=$BUN_SHA256 \
    EXOCORTEX_REPO=https://github.com/Yeyito777/Exocortex.git EXOCORTEX_BRANCH=$BRANCH \
    WHALE_HOST_KEY=\"$WHALE_HOST_KEY\" STAGE=/var/lib/exocortex-keeper/install \
    /bin/bash /var/lib/exocortex-keeper/install/phone-setup.sh"
su_run "rm -rf $ROOT/var/lib/exocortex-keeper/install $STAGE"

if [[ "$OPENAI_KEY" == 1 ]]; then
    step "OpenAI API key (secrets store -> adb stdin -> exocortexd login openai key)"
    key_file="$ROOT/root/.exocortex-openai-key"
    # The key goes only through stdin (no pty, so nothing echoes it): never
    # argv, a Mac file or a log.
    secrets run --allow OPENAI_API_KEY -- sh -c 'printf "%s" "$OPENAI_API_KEY"' |
        adb -s "$SERIAL" shell -T "/data/local/tmp/su -c 'umask 077; cat > $key_file'"
    login_ok=1
    in_ns "/usr/local/bin/exocortexd login openai key < $key_file; echo rc=\$?" | tee /dev/stderr | grep -q '^rc=0$' || login_ok=0
    su_run "rm -f $key_file"
    [[ "$login_ok" == 1 ]] || { echo "OpenAI login failed" >&2; exit 1; }
fi

if [[ "$START" == 1 ]]; then
    step "start"
    if [[ "$(su_run "sh /data/adb/exocortex/exocortex-keeper.sh status" | grep -c '^daemon: running')" == 1 ]]; then
        in_ns "/usr/local/bin/exocortex-supervisor restart"
    else
        su_run "sh /data/adb/exocortex/exocortex-keeper.sh start"
        for _ in $(seq 1 60); do
            su_run "[ -S $ROOT/opt/exocortex/config/runtime/exocortexd.sock ] && echo up" | grep -q up && break
            sleep 2
        done
    fi
    su_run "sh /data/adb/exocortex/exocortex-keeper.sh status"
fi

step "the phone's key for whale"
su_run "cat $ROOT/root/.ssh/exocortex_whale_ed25519.pub"
echo "authorize it on whale (restricted to the /proxy link): scripts/exocortex/authorize-whale.sh"
