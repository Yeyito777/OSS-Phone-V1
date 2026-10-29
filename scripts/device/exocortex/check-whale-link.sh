#!/bin/bash
# From the phone's chroot: this phone's key has full access to whale, like the
# user's Mac (authorize-whale.sh): the ordinary /proxy bridge answers with the
# whole protocol (not link-only), whale is logged in to Claude, and commands run
# on whale.
set -u

fail=0
check() {
    local what=$1 pattern=$2 output=$3
    if grep -q -E -- "$pattern" <<<"$output"; then
        echo "  ok    $what"
    else
        echo "  FAIL  $what"
        printf '%s\n' "$output" | cut -c1-200 | head -6 | sed 's/^/          /'
        fail=1
    fi
}
ssh_whale() { timeout 25 ssh -o BatchMode=yes -o ConnectTimeout=15 "$@" 2>&1; echo "rc=$?"; }

hello='{"type":"provider_proxy_hello","reqId":"h","version":1,"providers":["anthropic"],"clientHost":{"hostname":"phone-check","user":"root","platform":"linux","home":"/root"}}'
ping_cmd='{"type":"provider_proxy_ping","reqId":"p"}'

out="$( { printf '%s\n' "$hello" "$ping_cmd"; sleep 4; } | ssh_whale -T whale exocortexd proxy)"
check "the /proxy link answers" '"provider_proxy_hello_result"' "$out"
if grep -q '"link-only"' <<<"$out"; then echo "  FAIL  the link is still link-only"; fail=1; else echo "  ok    full protocol (not link-only)"; fi
check "whale is logged in to Claude" '"anthropic":\{"loggedIn":true' "$out"
check "pings are answered" '"provider_proxy_pong"' "$out"

out="$(ssh_whale whale 'id -un; uname -m')"
check "a command runs on whale" 'x86_64' "$out"

if [[ "$fail" == 0 ]]; then echo "all checks passed"; else echo "some checks FAILED"; exit 1; fi
