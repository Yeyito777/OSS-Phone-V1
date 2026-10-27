#!/usr/bin/env python3
"""Off-device stand-in for moon-compositor (src/moon-gpu/proto.h).

Speaks the protocol to a Moon running in GPU mode on an ordinary X server
(Xvfb), so Moon's GPU backend (src/a26-shell/src/gpu.rs) can be exercised
without the phone: layers, frames, the X mirror (a memfd here instead of a
dmabuf), snapshots, presentation pacing at 120 Hz and panel power. Frames are
composed on the CPU with numpy (nearest sampling, rounded corners ignored)
only when asked: `kill -USR1 PID` writes the current frame to --shot.

    mock-compositor.py --socket PATH [--shot frame.png] [--log FILE]
"""
import argparse
import mmap
import os
import select
import signal
import socket
import struct
import sys
import time

import numpy as np

W, H = 1080, 2340
STRIDE = 4352
PERIOD = 1 / 120
MIRROR_ID = 0xFFFFFFFF


class Surface:
    def __init__(self, w, h, stride=None, fmt=None, mem=None, pixels=None):
        self.w, self.h, self.stride, self.fmt, self.mem = w, h, stride, fmt, mem
        self.pixels = pixels  # HxWx3 uint8 RGB (copies), else read from mem
        self.uploads = 0
        self.radius = 0.0  # baked corners (copies)
        self.opaque = fmt != 2
        self.texture = None  # --textures: a private copy, updated by damage only

    def keep_texture(self):
        self.texture = self.live().copy()

    def upload(self, x0, y0, x1, y1):
        x0, y0, x1, y1 = max(x0, 0), max(y0, 0), min(x1, self.w), min(y1, self.h)
        if self.texture is not None and x1 > x0 and y1 > y0:
            self.texture[y0:y1, x0:x1] = self.live()[y0:y1, x0:x1]

    def rgb(self):
        if self.pixels is not None:
            return self.pixels
        return self.bgra()[:, :, [2, 1, 0]]

    def bgra(self):
        return self.texture if self.texture is not None else self.live()

    def live(self):
        a = np.frombuffer(self.mem, dtype=np.uint8, count=self.stride * self.h)
        return a.reshape(self.h, self.stride)[:, : self.w * 4].reshape(self.h, self.w, 4)

    def coverage(self):
        """Per-pixel alpha (premultiplied ARGB layers), else None."""
        if self.pixels is not None or self.fmt != 2:
            return None
        return self.bgra()[:, :, 3]


class Mock:
    def __init__(self, args):
        self.args = args
        self.surfaces = {}
        fd = os.memfd_create("moon-mirror")
        os.ftruncate(fd, STRIDE * H)
        self.mirror_fd = fd
        self.mirror = Surface(W, H, STRIDE, 1, mmap.mmap(fd, STRIDE * H))
        self.frame = None
        self.pending = None
        self.flip_due = None
        self.sequence = 0
        self.stats = {"frames": 0, "mirror_damage": 0, "uploads": 0, "copies": 0}
        self.paths = {}
        self.log = open(args.log, "a") if args.log else sys.stderr
        self.client = None
        self.powered = True
        self.shot_requested = False
        self.version = 2
        self.report_idle = set()  # v4: surfaces that asked for MG_SURFACE_IDLE
        self.referenced = set()  # surfaces named by the frame on screen

    def say(self, *parts):
        print(f"[{time.monotonic():.3f}]", *parts, file=self.log, flush=True)

    def send(self, kind, body, fd=None):
        msg = struct.pack("<HHI", kind, 0, 8 + len(body)) + body
        try:
            if fd is None:
                self.client.send(msg)
            else:
                socket.send_fds(self.client, [msg], [fd])
        except OSError as error:
            self.say("send failed", error)

    def lookup(self, sid):
        return self.mirror if sid == MIRROR_ID else self.surfaces.get(sid)

    def handle(self, msg, fds):
        kind, _, length = struct.unpack_from("<HHI", msg)
        body = msg[8:length]
        if kind == 1:  # HELLO
            refresh = 120000
            (version,) = struct.unpack_from("<I", body)
            if version > self.args.max_version:
                # A compositor older than the client drops it, as v2 did.
                self.say(f"client speaks version {version}; dropping it")
                self.client.close()
                self.client = None
                return
            self.version = version
            self.report_idle.clear()
            self.referenced.clear()
            self.send(64, struct.pack("<IIIIQII", version, W, H, refresh, 8333333, 1, 0))
            self.send(65, struct.pack("<IIIIIII", 0, W, H, STRIDE, 1, 0, 0), self.mirror_fd)
            self.say("hello")
        elif kind == 2:  # SURFACE_SHM
            sid, w, h, stride, fmt, flags = struct.unpack_from("<IIIIII", body)
            offset = struct.unpack_from("<I", body, 24)[0] if len(body) >= 28 else 0  # v3
            try:
                if os.fstat(fds[0]).st_size < offset + stride * h:
                    raise OSError("fd smaller than its pixels")
                mem = mmap.mmap(fds[0], offset + stride * h, prot=mmap.PROT_READ)
            except OSError as error:
                # v4: MG_SURFACE_FAILED (MG_FAIL_INVALID); older clients are
                # simply not told, as moon-compositor does.
                self.say(f"surface {sid} refused: {error}")
                if self.version >= 4:
                    self.send(70, struct.pack("<II", sid, 1))
                return
            mem = memoryview(mem)[offset:]
            self.surfaces[sid] = Surface(w, h, stride, fmt, mem)
            if self.args.textures:
                self.surfaces[sid].keep_texture()
            if self.version >= 4 and flags & 4:
                self.report_idle.add(sid)
            self.say(f"surface {sid} {w}x{h} fmt {fmt}")
        elif kind == 3:  # SURFACE_DAMAGE
            sid, serial, count, _ = struct.unpack_from("<IIII", body)
            self.stats["uploads"] += 1
            surface = self.surfaces.get(sid)
            if surface is not None:
                for i in range(count):
                    surface.upload(*struct.unpack_from("<4i", body, 16 + 16 * i))
            if self.args.upload_ms_per_mpx > 0:
                # A texture upload blocks like glTexSubImage2D: by area.
                area = 0
                for i in range(count):
                    x0, y0, x1, y1 = struct.unpack_from("<4i", body, 16 + 16 * i)
                    area += max(x1 - x0, 0) * max(y1 - y0, 0)
                time.sleep(area / 1e6 * self.args.upload_ms_per_mpx / 1000)
            self.send(66, struct.pack("<II", sid, serial))
        elif kind == 4:  # SURFACE_COPY
            sid, source = struct.unpack_from("<II", body)
            sx, sy, sw, sh = struct.unpack_from("<4f", body, 8)
            w, h = struct.unpack_from("<II", body, 24)
            (radius,) = struct.unpack_from("<f", body, 32)
            src = self.lookup(source)
            if src is not None:
                rgb = src.rgb()[int(sy) : int(sy + sh), int(sx) : int(sx + sw)]
                ys = (np.arange(h) * rgb.shape[0] // h).clip(0, rgb.shape[0] - 1)
                xs = (np.arange(w) * rgb.shape[1] // w).clip(0, rgb.shape[1] - 1)
                copy = Surface(w, h, pixels=rgb[ys][:, xs].copy())
                copy.radius = radius
                copy.opaque = radius <= 0.5 and src.opaque
                self.surfaces[sid] = copy
                self.stats["copies"] += 1
                self.say(f"copy {sid} from {source:#x} {w}x{h} radius {radius:.0f} mean {rgb.mean():.1f}")
        elif kind == 5:  # FREE
            (sid,) = struct.unpack_from("<I", body)
            self.surfaces.pop(sid, None)
            self.report_idle.discard(sid)  # no IDLE follows a free
        elif kind == 6:  # MIRROR_DAMAGE
            self.stats["mirror_damage"] += 1
        elif kind == 7:  # FRAME
            serial, flags, base, count = struct.unpack_from("<IIII", body)
            nodes = [struct.unpack_from("<II4f4f3fIII", body, 16 + 64 * i) for i in range(count)]
            if self.pending is not None:
                # A queued frame replaced before it was shown: what only it
                # named is idle at once.
                replaced = self.names(self.pending)
                new = self.names((serial, flags, base, nodes))
                self.idle(replaced - new - self.referenced - self.names(self.frame))
            self.pending = (serial, flags, base, nodes)
            why = self.plane_plan(flags, base, nodes)
            key = "planes" if why is None else why
            self.paths[key] = self.paths.get(key, 0) + 1
        elif kind == 8:  # POWER
            (on,) = struct.unpack_from("<I", body)
            self.powered = bool(on)
            self.say("power", "on" if on else "off")
            self.send(68, struct.pack("<I", on))
        else:
            self.say("unknown message", kind)

    def names(self, frame):
        if frame is None:
            return set()
        _, flags, base, nodes = frame
        named = {node[1] for node in nodes if node[0] == 1}
        if flags & 1:
            named.add(base)
        return named

    def idle(self, sids):
        for sid in sorted(sids & self.report_idle):
            self.send(69, struct.pack("<I", sid))

    def plane_plan(self, flags, base, nodes, scalers=3, planes=8):
        """moon-compositor's plan_planes(), to see which frames would skip
        drawing: None if the frame fits the display planes, else why not."""
        n = scaled = 0
        if flags & 1:
            s = self.lookup(base)
            if s is None or not s.opaque:
                return "base"
            n += 1
        for kind, sid, sx, sy, sw, sh, dx, dy, dw, dh, alpha, dim, radius, nflags, color, _ in nodes:
            if alpha <= 0.002 or dw <= 0 or dh <= 0:
                continue
            if kind == 2:
                if n == 0 and (color & 0xFFFFFF) == 0:
                    continue
                return "fill"
            s = self.lookup(sid)
            if s is None or n == planes:
                return "count"
            kx, ky = dw / sw, dh / sh
            if abs(radius - s.radius * kx) > 1.5:
                return "radius"
            x0, y0, x1, y1 = max(round(dx), 0), max(round(dy), 0), min(round(dx + dw), W), min(round(dy + dh), H)
            if x1 <= x0 or y1 <= y0:
                continue
            if abs(kx - 1) > 0.002 or abs(ky - 1) > 0.002:
                scaled += 1
                if kx < 0.5 or ky < 0.5 or kx > 8 or ky > 8 or scaled > scalers:
                    return "scale"
            if x1 - x0 < 64 or y1 - y0 < 16:
                return "small"
            if dim < 0.998 and (n != 0 or not s.opaque):
                return "dim"
            n += 1
        return None

    def tick(self):
        now = time.monotonic()
        if self.flip_due is not None and now >= self.flip_due:
            serial = self.frame[0]
            self.sequence += 1
            self.flip_due = None
            self.stats["frames"] += 1
            if serial:
                t = time.clock_gettime_ns(time.CLOCK_MONOTONIC)
                self.send(67, struct.pack("<IIQQII", serial, self.sequence, t, 2_000_000, 2, 0))
            # v4: what the frame now on screen no longer names is idle.
            names = self.names(self.frame)
            self.idle(self.referenced - names)
            self.referenced = names
        if self.flip_due is None and self.pending is not None and self.powered:
            # v4: surfaces the previous frame named and this one does not are
            # idle once it flips (reported when the flip is, below).
            self.frame, self.pending = self.pending, None
            # The next vblank on a 120 Hz grid once the frame is rendered.
            ready = now + self.args.render_ms / 1000
            self.flip_due = (int(ready / PERIOD) + 1) * PERIOD

    def render(self):
        out = np.zeros((H, W, 3), dtype=np.float32)
        if self.frame is None:
            return out
        serial, flags, base, nodes = self.frame
        if flags & 1 and self.lookup(base) is not None:
            src = self.lookup(base).rgb()
            out[: src.shape[0], : src.shape[1]] = src[:H, :W]
        for kind, sid, sx, sy, sw, sh, dx, dy, dw, dh, alpha, dim, radius, nflags, color, _ in nodes:
            x0, y0 = max(int(dx), 0), max(int(dy), 0)
            x1, y1 = min(int(dx + dw), W), min(int(dy + dh), H)
            if x1 <= x0 or y1 <= y0 or alpha <= 0:
                continue
            if kind == 2:
                rgb = np.array([(color >> 16) & 255, (color >> 8) & 255, color & 255], np.float32)
                out[y0:y1, x0:x1] = out[y0:y1, x0:x1] * (1 - alpha) + rgb * alpha
                continue
            surface = self.lookup(sid)
            if surface is None:
                continue
            src = surface.rgb()
            ys = (sy + (np.arange(y0, y1) + 0.5 - dy) * sh / dh).astype(int).clip(0, src.shape[0] - 1)
            xs = (sx + (np.arange(x0, x1) + 0.5 - dx) * sw / dw).astype(int).clip(0, src.shape[1] - 1)
            pix = src[ys][:, xs].astype(np.float32) * dim
            cov = surface.coverage()
            a = alpha if cov is None else alpha * cov[ys][:, xs, None].astype(np.float32) / 255
            m = 1.0
            if radius > 0.5:
                # The node's rounded corners, as moon-compositor draws them.
                r = min(radius, dw / 2, dh / 2)
                px = np.arange(x0, x1) + 0.5
                py = np.arange(y0, y1) + 0.5
                qx = np.maximum(np.maximum(dx + r - px, px - (dx + dw - r)), 0)[None, :]
                qy = np.maximum(np.maximum(dy + r - py, py - (dy + dh - r)), 0)[:, None]
                m = np.clip(r - np.sqrt(qx * qx + qy * qy) + 0.5, 0, 1)[:, :, None]
            out[y0:y1, x0:x1] = out[y0:y1, x0:x1] * (1 - a * m) + pix * alpha * m
        return out

    def shot(self):
        path = self.args.shot
        img = self.render().clip(0, 255).astype(np.uint8)
        with open(path + ".ppm", "wb") as f:
            f.write(b"P6 %d %d 255\n" % (W, H))
            f.write(img.tobytes())
        os.replace(path + ".ppm", path)
        serial, flags, base, nodes = self.frame or (0, 0, 0, [])
        self.say(f"shot {path}: frame {serial} base {base if flags & 1 else '-'} nodes "
                 + ", ".join(f"{'img' if n[0] == 1 else 'fill'}:{n[1]:#x}" for n in nodes)
                 + f" stats {self.stats} paths {self.paths}")
        for kind, sid, *_rest in nodes:
            surface = self.lookup(sid)
            if kind == 1 and surface is not None:
                self.say(f"  surface {sid:#x} {surface.w}x{surface.h} mean {surface.rgb().mean():.1f} "
                         f"node src {_rest[0]:.0f},{_rest[1]:.0f} {_rest[2]:.0f}x{_rest[3]:.0f} "
                         f"dst {_rest[4]:.0f},{_rest[5]:.0f} {_rest[6]:.0f}x{_rest[7]:.0f} "
                         f"alpha {_rest[8]:.2f} dim {_rest[9]:.2f}")

    def run(self):
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        try:
            os.unlink(self.args.socket)
        except FileNotFoundError:
            pass
        listener.bind(self.args.socket)
        listener.listen(2)
        signal.signal(signal.SIGUSR1, lambda *_: setattr(self, "shot_requested", True))
        self.say("listening on", self.args.socket)
        while True:
            fds = [listener] + ([self.client] if self.client else [])
            ready, _, _ = select.select(fds, [], [], 0.002)
            if listener in ready:
                if self.client:
                    self.client.close()
                self.client, _ = listener.accept()
                self.surfaces.clear()
                self.say("client connected")
            if self.client and self.client in ready:
                try:
                    msg, fds_in, _, _ = socket.recv_fds(self.client, 65536, 8)
                except OSError:
                    msg = b""
                if not msg:
                    self.say("client gone")
                    self.client = None
                    continue
                self.handle(msg, fds_in)
                if self.client is None:
                    continue
                for fd in fds_in:
                    os.close(fd)
            self.tick()
            if self.shot_requested:
                self.shot_requested = False
                self.shot()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--socket", required=True)
    parser.add_argument("--shot", default="/tmp/moon-gpu-mock.ppm")
    parser.add_argument("--log")
    parser.add_argument("--max-version", type=int, default=4,
                        help="drop clients that say hello with a newer version (2: an old compositor)")
    parser.add_argument("--textures", action="store_true",
                        help="show client surfaces from copies updated only by their damage, "
                             "as moon-compositor's textures are")
    parser.add_argument("--render-ms", type=float, default=0,
                        help="a frame flips at the first vblank this long after it arrives")
    parser.add_argument("--upload-ms-per-mpx", type=float, default=0,
                        help="client damage blocks this long per megapixel (texture upload)")
    Mock(parser.parse_args()).run()
