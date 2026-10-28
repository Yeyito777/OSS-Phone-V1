#!/bin/bash
# From the phone's chroot: whale lets this phone's key borrow the Claude
# login over the /proxy link and do nothing else (authorize-whale.sh).
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

hello='{"type":"provider_proxy_hello","reqId":"h","version":1,"providers":["anthropic","openai"],"clientHost":{"hostname":"phone","user":"root","platform":"linux","home":"/root"}}'
status_openai='{"type":"provider_proxy_status","reqId":"o","provider":"openai"}'
list='{"type":"list_conversations","reqId":"l"}'
send='{"type":"send_message","reqId":"s","convId":"1-aaaaaa","text":"id"}'
exec_cmd='{"type":"provider_proxy_exec","execId":"e","command":"id","timeoutMs":5000}'
ping_cmd='{"type":"provider_proxy_ping","reqId":"p"}'

out="$( { printf '%s\n' "$hello" "$status_openai" "$list" "$send" "$exec_cmd" "$ping_cmd"; sleep 4; } | ssh_whale -T whale exocortexd proxy)"
check "the /proxy link answers, marked link-only" '"provider_proxy_hello_result".*"link-only"' "$out"
check "whale is logged in to Claude" '"anthropic":\{"loggedIn":true' "$out"
check "openai is refused" '"reqId":"o".*Refused' "$out"
check "list_conversations is refused" '"reqId":"l".*Refused' "$out"
check "send_message is refused" '"reqId":"s".*Refused' "$out"
check "remote_bash is refused" '"provider_proxy_exec_result".*remote_bash is not available' "$out"
check "pings are answered" '"provider_proxy_pong"' "$out"

out="$(echo 'id; hostname' | ssh_whale -T whale 'id; hostname')"
check "a command runs the bridge instead (no uid= output)" 'Invalid JSON' "$out"
grep -q 'uid=' <<<"$out" && { echo "  FAIL  a command ran on whale"; fail=1; }

out="$(echo 'id' | ssh_whale -tt whale)"
check "no pty, no login shell" 'PTY allocation request failed|Invalid JSON' "$out"
grep -q 'uid=' <<<"$out" && { echo "  FAIL  a shell ran on whale"; fail=1; }

out="$(ssh_whale -T -s whale sftp </dev/null)"
grep -q 'uid=\|sftp>' <<<"$out" && { echo "  FAIL  sftp subsystem"; fail=1; } || echo "  ok    no sftp subsystem"

# A forward is refused when it is used: open one, connect through it, and
# look for whale's own sshd banner (it must not arrive) and sshd's refusal.
forward_test() {
    local what=$1 flag=$2 port=$3 log
    log="$(mktemp)"
    timeout 20 ssh -o BatchMode=yes -o ConnectTimeout=15 -N "$flag" "$4" whale 2>"$log" </dev/null &
    local pid=$!
    sleep 5
    local got
    if [[ "$flag" == -D ]]; then
        got="$(curl -s --max-time 5 --socks5 "127.0.0.1:$port" telnet://127.0.0.1:22 </dev/null 2>&1 | head -c 64)"
    else
        got="$(timeout 5 bash -c "exec 3<>/dev/tcp/127.0.0.1/$port; head -c 64 <&3" 2>&1)"
    fi
    sleep 1
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    if grep -q 'SSH-2.0' <<<"$got"; then
        echo "  FAIL  $what: whale's port 22 answered through it"
        fail=1
    elif grep -q -E 'administratively prohibited|open failed' "$log"; then
        echo "  ok    $what"
    else
        echo "  FAIL  $what: no refusal seen"
        sed 's/^/          /' "$log" | head -4
        fail=1
    fi
    rm -f "$log"
}
forward_test "local port forwarding is refused" -L 47999 127.0.0.1:47999:127.0.0.1:22
forward_test "dynamic forwarding (SOCKS) is refused" -D 47997 127.0.0.1:47997
out="$(ssh_whale -o ExitOnForwardFailure=yes -N -R 127.0.0.1:47998:127.0.0.1:22 whale </dev/null)"
check "remote port forwarding is refused" 'forwarding failed|rc=255' "$out"

if [[ "$fail" == 0 ]]; then echo "all checks passed"; else echo "some checks FAILED"; exit 1; fi
