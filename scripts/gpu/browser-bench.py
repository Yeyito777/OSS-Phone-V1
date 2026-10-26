#!/usr/bin/env python3
"""Browser scroll benchmark on the phone (notes/gpu/BROWSER.md).

Loads URL in the running Browser (vimbrowser, over its DevTools port), then
for SECONDS drives real touch swipes through Moon's input path while
measuring:
  - CPU time per process group (browser, GPU process, renderers, other
    Chromium children, Xorg, Moon, moon-compositor), as % of one core;
  - screen updates: moon-compositor flips (GPU mode: its per-flip profile log);
  - the page's requestAnimationFrame rate (main-thread frames).

The Browser must be open and in front (Moon: `a26-shellctl launch browser`).

    scripts/gpu/browser-bench.py [--url URL] [--seconds N] [--label TEXT]
"""
import argparse
import base64
import json
import os
import socket
import struct
import subprocess
import sys
import time
import urllib.request

PORT = 9222


def adb(*args, stdin=None):
    return subprocess.run(["adb", *args], input=stdin, capture_output=True, text=True,
                          check=True).stdout


def su(script):
    return adb("shell", "su", stdin=script)


class Cdp:
    """A minimal DevTools websocket client (no third-party modules)."""

    def __init__(self, url):
        host, rest = url.split("://", 1)[1].split("/", 1)
        addr, port = host.split(":")
        self.sock = socket.create_connection((addr, int(port)))
        key = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall((f"GET /{rest} HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\n"
                           f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                           "Sec-WebSocket-Version: 13\r\n\r\n").encode())
        response = b""
        while b"\r\n\r\n" not in response:
            response += self.sock.recv(4096)
        if b" 101 " not in response.split(b"\r\n", 1)[0]:
            raise RuntimeError(f"websocket upgrade refused: {response[:80]!r}")
        self.buffer = response.split(b"\r\n\r\n", 1)[1]
        self.next_id = 0

    def _read(self, n):
        while len(self.buffer) < n:
            chunk = self.sock.recv(65536)
            if not chunk:
                raise RuntimeError("DevTools closed the connection")
            self.buffer += chunk
        data, self.buffer = self.buffer[:n], self.buffer[n:]
        return data

    def _frame(self):
        head = self._read(2)
        length = head[1] & 0x7F
        if length == 126:
            length = struct.unpack(">H", self._read(2))[0]
        elif length == 127:
            length = struct.unpack(">Q", self._read(8))[0]
        return head[0] & 0x0F, self._read(length)

    def call(self, method, **params):
        self.next_id += 1
        payload = json.dumps({"id": self.next_id, "method": method, "params": params}).encode()
        mask = os.urandom(4)
        header = bytes([0x81])
        if len(payload) < 126:
            header += bytes([0x80 | len(payload)])
        elif len(payload) < 65536:
            header += bytes([0x80 | 126]) + struct.pack(">H", len(payload))
        else:
            header += bytes([0x80 | 127]) + struct.pack(">Q", len(payload))
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(header + mask + masked)
        while True:
            opcode, data = self._frame()
            if opcode != 1:
                continue
            message = json.loads(data)
            if message.get("id") == self.next_id:
                if "error" in message:
                    raise RuntimeError(f"{method}: {message['error']}")
                return message.get("result", {})

    def evaluate(self, expression):
        result = self.call("Runtime.evaluate", expression=expression, returnByValue=True,
                           awaitPromise=True)
        return result.get("result", {}).get("value")


SAMPLE = r"""
RUN=/data/local/tmp/moon-gpu
group() {  # pid -> group name
    exe=$(cat /proc/$1/comm 2>/dev/null)
    case "$exe" in
    Xorg) echo xorg; return ;;
    a26-shell) echo moon; return ;;
    moon-compositor) echo compositor; return ;;
    esac
    cmd=$(tr '\0' ' ' </proc/$1/cmdline 2>/dev/null)
    case "$cmd" in
    *vimbrowser*--type=gpu-process*) echo gpu-process ;;
    *vimbrowser*--type=renderer*) echo renderer ;;
    *vimbrowser*--type=*) echo chromium-other ;;
    *vimbrowser*) echo browser ;;
    esac
}
pids=$(for d in /proc/[0-9]*; do g=$(group ${d#/proc/}); [ -n "$g" ] && echo "$g ${d#/proc/}"; done)
snapshot() {  # group pid ticks, for the processes found above
    echo "$pids" | while read g p; do
        set -- $(cut -d')' -f2 /proc/$p/stat 2>/dev/null)
        [ $# -ge 13 ] && echo "$g $p $((${12} + ${13}))"
    done
}
"""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--url", default="https://en.wikipedia.org/wiki/Mali_(processor)")
    ap.add_argument("--seconds", type=int, default=10)
    ap.add_argument("--label", default="")
    args = ap.parse_args()

    adb("forward", f"tcp:{PORT}", f"tcp:{PORT}")
    targets = json.load(urllib.request.urlopen(f"http://127.0.0.1:{PORT}/json"))
    page = next(t for t in targets if t.get("type") == "page")
    cdp = Cdp(page["webSocketDebuggerUrl"])
    cdp.call("Page.enable")
    cdp.call("Page.navigate", url=args.url)
    time.sleep(6)
    cdp.evaluate("""(() => { window.__frames = 0; const tick = () => { window.__frames++;
        requestAnimationFrame(tick); }; requestAnimationFrame(tick); return 1; })()""")

    ticks_per_s = 100
    swipes = max(1, args.seconds * 1000 // 700)
    out = su(SAMPLE + f"""
pid=$(cat $RUN/compositor.pid 2>/dev/null)
[ -n "$pid" ] && kill -USR2 $pid
marker=$(wc -l < $RUN/compositor.log)
t0=$(cut -d' ' -f1 /proc/uptime)
before=$(snapshot)
R=/data/local/a26-linux
for i in $(seq 1 {swipes}); do
    if [ $((i % 6)) -lt 4 ]; then y0=1700; y1=700; else y0=700; y1=1700; fi
    $R/busybox.static chroot $R /opt/a26-shell/bin/a26-touchpath 540 $y0 540 $y1 300 >/dev/null 2>&1
    sleep 0.35
done
after=$(snapshot)
t1=$(cut -d' ' -f1 /proc/uptime)
[ -n "$pid" ] && kill -USR2 $pid
echo "elapsed $t0 $t1"
echo "flips $(tail -n +$((marker + 1)) $RUN/compositor.log | grep -c ' flip serial')"
echo "$before" | sed 's/^/B /'
echo "$after" | sed 's/^/A /'
""")
    frames = cdp.evaluate("window.__frames")

    elapsed = flips = 0.0
    before, after = {}, {}
    for line in out.splitlines():
        parts = line.split()
        if parts[:1] == ["elapsed"]:
            elapsed = float(parts[2]) - float(parts[1])
        elif parts[:1] == ["flips"]:
            flips = int(parts[1])
        elif parts[:1] in (["B"], ["A"]) and len(parts) == 4:
            (before if parts[0] == "B" else after)[(parts[1], parts[2])] = int(parts[3])
    groups = {}
    for key, value in after.items():
        groups[key[0]] = groups.get(key[0], 0) + value - before.get(key, 0)
    print(f"== {args.label or 'run'}: {args.url}, {elapsed:.1f} s of swipes")
    print(f"screen updates (compositor flips): {flips / elapsed:.1f}/s" if elapsed else "no timing")
    if frames is not None and elapsed:
        print(f"page rAF frames: {frames / (elapsed + 1):.1f}/s (approx.)")
    total = 0.0
    for name in ["browser", "gpu-process", "renderer", "chromium-other", "xorg", "moon",
                 "compositor"]:
        pct = 100.0 * groups.get(name, 0) / ticks_per_s / elapsed if elapsed else 0
        total += pct
        print(f"  {name:15} {pct:6.1f} % of a core")
    print(f"  {'all of these':15} {total:6.1f} % of a core (8 cores = 800 %)")


if __name__ == "__main__":
    sys.exit(main())
