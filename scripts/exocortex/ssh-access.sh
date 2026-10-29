#!/usr/bin/env bash
# SSH into the phone from this Mac and from whale (docs/SSH.md):
#
#   scripts/exocortex/ssh-access.sh [--check]
#
#   1. the phone's /root/.ssh/authorized_keys gets this Mac's key (the one
#      ~/.ssh/config uses for whale) and whale's user keys (~/.ssh/id_*.pub)
#   2. `Host oss-phone` here (through whale's end of the phone's reverse
#      tunnel) and `Host oss-phone-lan` (the phone's Wi-Fi address, direct),
#      and `Host oss-phone` on whale; both ~/.ssh/config files are backed up
#      first and the entries live between "oss-phone" markers, replaced on
#      a rerun (the LAN address follows the phone's current one)
#   3. the phone's host key pinned in both known_hosts as `oss-phone`
#   4. checks: `ssh oss-phone` from here and from whale, scp both ways
#
# The server and the tunnel are installed and kept running by
# scripts/exocortex/install.sh and the keeper. --check runs only step 4.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"

WHALE="${EXOCORTEX_WHALE:-whale}"
ROOT=/data/local/a26-linux
CHECK=0
[[ "${1:-}" != --check ]] || CHECK=1
SERIAL="$(a26_resolve_serial)"
su_run() { adb -s "$SERIAL" shell "/data/local/tmp/su -c '$1'" | tr -d '\r'; }

conf="$(su_run "cat $ROOT/etc/oss-phone/tunnel.conf")"
TUNNEL_PORT="$(sed -n 's/^TUNNEL_PORT=//p' <<<"$conf")"
SSHD_PORT="$(sed -n 's/^SSHD_PORT=//p' <<<"$conf")"
[[ "$TUNNEL_PORT" =~ ^[0-9]+$ && "$SSHD_PORT" =~ ^[0-9]+$ ]] || { echo "no tunnel.conf on the phone (scripts/exocortex/install.sh)" >&2; exit 1; }
LAN_IP="$(su_run "ip -4 -o addr show wlan0" | awk '{ sub("/.*", "", $4); print $4; exit }')"

if [[ "$CHECK" == 0 ]]; then
    identity="$(ssh -G "$WHALE" | awk '$1 == "identityfile" { print $2; exit }')"
    identity="${identity/#\~/$HOME}"
    [[ -f "$identity.pub" ]] || { echo "no public key next to $identity" >&2; exit 1; }
    keys="$(cat "$identity.pub"; ssh "$WHALE" 'cat ~/.ssh/id_*.pub')"
    echo "== the phone's authorized_keys"
    printf '%s\n' "$keys" | adb -s "$SERIAL" shell -T "/data/local/tmp/su -c 'cat > $ROOT/tmp/.oss-phone-keys'"
    su_run "cd $ROOT/root/.ssh && touch authorized_keys && while IFS= read -r key; do [ -n \"\$key\" ] || continue; grep -qxF \"\$key\" authorized_keys || echo \"\$key\" >>authorized_keys; done <$ROOT/tmp/.oss-phone-keys; rm -f $ROOT/tmp/.oss-phone-keys; chmod 0600 authorized_keys; chown 0:0 authorized_keys; cut -d\" \" -f3- authorized_keys | sed \"s/^/  /\""

    hostkey="$(su_run "cat $ROOT/etc/ssh/ssh_host_ed25519_key.pub" | awk '{ print $1, $2 }')"
    [[ "$hostkey" =~ ^ssh-ed25519\ [A-Za-z0-9+/]+=*$ ]] || { echo "no host key on the phone" >&2; exit 1; }

    # The block for one ~/.ssh/config, and the known_hosts pin, applied by a
    # script that runs the same way here and on whale.
    apply='set -euo pipefail
cfg=$HOME/.ssh/config known=$HOME/.ssh/known_hosts block=$1 hostkey="$(printf %s "$2" | base64 -d)"
cp -p "$cfg" "$cfg.bak-oss-phone-$(date +%Y%m%d%H%M%S)"
awk "/^# >>> oss-phone/{skip=1} !skip{print} /^# <<< oss-phone/{skip=0}" "$cfg" >"$cfg.new"
printf "\n%s\n" "$(printf %s "$block" | base64 -d)" >>"$cfg.new"
chmod 0600 "$cfg.new" && mv -f "$cfg.new" "$cfg"
touch "$known"
grep -v "^oss-phone " "$known" >"$known.new" || true
echo "oss-phone $hostkey" >>"$known.new"
chmod 0600 "$known.new" && mv -f "$known.new" "$known"
echo "  $cfg (backup kept), $known: oss-phone pinned"'

    mac_block="# >>> oss-phone (OSS-Phone-V1 docs/SSH.md; scripts/exocortex/ssh-access.sh rewrites this block)
# The phone through its reverse tunnel to whale: works on any network.
Host oss-phone
    HostName 127.0.0.1
    Port $TUNNEL_PORT
    User root
    ProxyJump $WHALE
    HostKeyAlias oss-phone
    IdentityFile $identity
    IdentitiesOnly yes
    ServerAliveInterval 15
# The phone directly on its Wi-Fi address (when this Mac can reach it).
Host oss-phone-lan
    HostName $LAN_IP
    Port $SSHD_PORT
    User root
    HostKeyAlias oss-phone
    IdentityFile $identity
    IdentitiesOnly yes
    ServerAliveInterval 15
# <<< oss-phone"
    whale_block="# >>> oss-phone (OSS-Phone-V1 docs/SSH.md; scripts/exocortex/ssh-access.sh rewrites this block)
# The phone through its reverse tunnel, which ends here.
Host oss-phone
    HostName 127.0.0.1
    Port $TUNNEL_PORT
    User root
    HostKeyAlias oss-phone
    ServerAliveInterval 15
# <<< oss-phone"
    echo "== this Mac's ssh config"
    # Arguments in base64: ssh hands them to the remote shell to split again.
    b64() { printf %s "$1" | base64 | tr -d '\n'; }
    bash -c "$apply" -- "$(b64 "$mac_block")" "$(b64 "$hostkey")"
    echo "== $WHALE's ssh config"
    ssh "$WHALE" bash -s -- "$(b64 "$whale_block")" "$(b64 "$hostkey")" <<<"$apply"
fi

echo "== checks"
fail=0
check() {   # WHAT PATTERN OUTPUT
    if grep -q -E -- "$2" <<<"$3"; then echo "  ok    $1"; else echo "  FAIL  $1"; sed 's/^/          /' <<<"$3" | head -5; fail=1; fi
}
probe='id -un; uname -m; android-sh -c "getprop ro.product.model"'
check "ssh oss-phone from this Mac" 'root.*aarch64.*SM-A266M' "$(ssh -o BatchMode=yes -o ConnectTimeout=20 oss-phone "$probe" 2>&1 | tr '\n' ' ')"
check "ssh oss-phone from $WHALE" 'root.*aarch64.*SM-A266M' "$(ssh "$WHALE" "ssh -o BatchMode=yes -o ConnectTimeout=20 oss-phone '$probe'" 2>&1 | tr '\n' ' ')"
if nc -z -G 5 "$LAN_IP" "$SSHD_PORT" 2>/dev/null; then
    check "ssh oss-phone-lan ($LAN_IP) from this Mac" 'root.*aarch64.*SM-A266M' "$(ssh -o BatchMode=yes -o ConnectTimeout=10 oss-phone-lan "$probe" 2>&1 | tr '\n' ' ')"
else
    echo "  --    oss-phone-lan: $LAN_IP:$SSHD_PORT not reachable from this Mac (client isolation on this Wi-Fi?); oss-phone works anywhere"
fi
tmp="$(mktemp)"; echo "scp test $(date +%s)" >"$tmp"
scp -q -o BatchMode=yes "$tmp" oss-phone:/tmp/oss-phone-scp-test && scp -q -o BatchMode=yes oss-phone:/tmp/oss-phone-scp-test "$tmp.back" &&
    cmp -s "$tmp" "$tmp.back" && echo "  ok    scp to and from the phone" || { echo "  FAIL  scp"; fail=1; }
ssh -o BatchMode=yes oss-phone 'rm -f /tmp/oss-phone-scp-test'; rm -f "$tmp" "$tmp.back"
[[ "$fail" == 0 ]] && echo "all checks passed" || { echo "some checks FAILED" >&2; exit 1; }
