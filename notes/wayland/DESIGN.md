# Wayland on the A26: measurements, design and migration plan

Status: experiment, October 2026. Branch `wayland-experiment`; nothing here
changes the boot path. Device: SM-A266M, OSS kernel 5.10.223, Moon direct
boot, Alpine 3.24 chroot. Companion document: `notes/gpu/DESIGN.md` on
`gpu-compositor` (the GPU agent's bionic EGL compositor).

## Summary

1. **A compositor that owns KMS beats Xorg here, even rendering on the CPU.**
   It runs the same pixman code that Xorg's RENDER runs. Moon on Xorg fits a
   120 Hz refresh only below about 0.75 Mpx of redraw, and its switcher
   (about 1.07 Mpx a frame) runs mostly at 60 Hz. On the same phone:
   - weston (DRM backend, pixman) holds 120 Hz up to 1.07 Mpx a frame (1.7 %
     missed refreshes);
   - cage (wlroots, pixman) holds 120 Hz up to 2.0 Mpx (0.3 % missed) and
     shows a full 2.53 Mpx at 108 fps;
   - a bare KMS loop with four pixman threads composing in place into cached
     dma-heap buffers draws a full-screen bilinear-scaled frame (2.53 Mpx)
     every refresh with 0 missed.

   What makes the difference is getting rid of the X server's single-threaded
   RENDER, the extra shadow copy and Present's copy-based presentation, and
   adding real page flips and parallelism. Wayland as a protocol does not
   contribute to it.
2. **The display controller can compose Moon's scenes by itself.**
   exynos-drmdpu exposes 8 planes on the panel's CRTC, each with per-plane
   alpha and zpos; three of them scale (2x down, 8x up). The atomic check
   accepts all 8 at full screen at once. An app-switcher animation (background
   plus up to 6 moving, scaling, fading full-screen cards) ran at 120 Hz with
   0 missed refreshes over 724 frames, using 0.03 ms of CPU a frame. The
   lock-screen cross-fade, the one transition that still misses 120 Hz on
   Xorg, becomes a plane alpha ramp. No off-the-shelf compositor uses these
   planes without a GPU (see "Candidates").
3. **Recommendation: don't switch the session to weston, cage or another
   stock Wayland compositor. Move KMS ownership into one Moon compositor that
   puts layers on planes first, then uses the GPU (GPU agent), then pixman
   (this work), and add a Wayland front end for apps later, keeping Xwayland
   for System and CEF.** The plan is under "Migration". It's the GPU agent's
   architecture with a plane-first policy and a CPU backend, so the
   Xorg-to-KMS win doesn't depend on the bionic GPU path. Whether to change
   the session is the user's decision; this branch only measures.

## Setup and safety

- A separate Alpine 3.24 aarch64 rootfs at `/data/local/a26-wayland`
  (`scripts/wayland/build-rootfs.sh`, built with podman and qemu on whale):
  weston 14.0.2, cage 0.3.0 with wlroots 0.19.3, Xwayland 24.1.14, libinput
  1.31.3, libudev-zero, libseat (noop backend), plus the bench tools. Moon's
  chroot and Xorg's libraries are never touched.
- `scripts/wayland/phone.sh handoff` (`device/a26-wayland-handoff.sh`):
  1. wakes the panel through Moon;
  2. **SIGSTOPs** the supervisor, so its liveness loop can't see Xorg gone and
     restore Android;
  3. **SIGKILLs** the Xorg takeover chain, so its EXIT traps (which restore
     Android) never run;
  4. stops Moon's session group, then Xorg.

  `moon-session` stays running, because `moon-session.sh` is blocked in
  `wait` on the frozen supervisor, so the init-level fallback never fires.
  `a26-wayland-check.sh` verifies the state before every run: no Xorg, no
  SurfaceFlinger/HWC/bootanim, zygote stopped, moon-session running,
  supervisor in state T. Nothing on `/data/adb` or the boot image changes.
  The way back is `adb reboot` (`phone.sh restore`).
- **Kernel hazard (found by the GPU agent):** exynos-drm reads the plane's
  `HDR_FD` (and the CRTC's `DQE_FD`) as a file descriptor of the *committing*
  process on every commit (`exynos_drm_hdr.c:103-146`, values below 3 are
  ignored). Samsung's HWC leaves `HDR_FD=16` on the primary plane; in another
  process fd 16 can be an unrelated dmabuf, and the GPU agent's first commit
  panicked the kernel. Neither weston nor wlroots knows these properties.
  `kmsbench` sets `HDR_FD=-1` / `DQE_FD=-1` in every commit, and
  `kmsbench sanitize` was run once before any compositor. The values persist
  in the atomic state: drm_info read -1 before and after weston and cage.
  **Any compositor that owns KMS on this phone must do this in its first
  commit.**
- Session of 2026-10-26: about 35 min, one reboot, no panic. Moon came back
  at 21.7 s uptime with `start-failures` 0.

## What the display hardware offers

From drm_info (`raw/drm_info.txt`), the device tree (`drmdpp@*`) and the DPU
source (`drivers/gpu/drm/samsung/dpu/exynos_p2`):

| Plane (id) | DPP | Type | Scaling | Other |
|---|---|---|---|---|
| 54 | 0 | primary of CRTC 120 | no | RGB only |
| 61 | 1 | "primary" of CRTC 121, usable on 120 | **yes** (0.5x-8x) | AFBC, rotation 90/270, YUV, CSC |
| 68 | 2 | overlay | no | AFBC |
| 75 | 3 | overlay | **yes** | YUV, CSC, SBWC |
| 82 | 4 | overlay | no | AFBC |
| 89 | 5 | overlay | no | YUV, CSC |
| 96 | 6 | overlay | no | |
| 103 | 7 | overlay | **yes** | AFBC, YUV, CSC |

- All planes have `alpha` (16-bit), `pixel blend mode` (None, Pre-multiplied,
  Coverage) and `zpos` 0-8. The writeback connector is on CRTC 121 only, so it
  can't capture the panel. There's no async flip and no syncobj, but
  `IN_FENCE_FD`, `OUT_FENCE_PTR` and `CRTC_IN_VBLANK_EVENT` are present.
- TEST_ONLY matrix (`kmsbench probe`, `raw/exp-kms-results.txt`):
  - every overlay is accepted at 1:1 and at 50 % alpha;
  - planes 61, 75 and 103 are accepted at x0.6, x0.5 and x1.5, and with a
    negative CRTC_X (a card sliding off the edge);
  - x0.45 is rejected (the 2x down limit), as is a 40x10 destination (the
    minimum is 64x16);
  - 1 to 7 full-screen overlays at 50 % alpha are all accepted together.
- Dumb buffers come from the `system-uncached` dma-heap and are mapped
  write-combined (`exynos_drm_gem.c:117`). Measured on a 10.1 MB frame
  (`kmsbench bw`):

  | Operation | Time |
  |---|---|
  | Copy, cached to cached | 1.57 ms |
  | Copy, cached to dumb | 1.88 ms |
  | **Read from a dumb buffer** | **40.9 ms** |
  | Read from a cached `system` heap buffer | 1.33 ms |
  | `DMA_BUF_IOCTL_SYNC` start + end, whole buffer | 0.16 + 0.21 ms |

  So a CPU compositor must never blend inside a dumb buffer. Composing in
  place in a cached `system` heap buffer and scanning that out (PRIME import
  works) costs no extra copy.
- **GBM works without a GPU driver.** Mesa's GBM (`drm` backend) allocates a
  linear 1080x2340 scanout buffer, and `AddFB2` accepts it. wlroots' GBM
  allocator and smithay's `GbmFramebufferExporter` are therefore usable on
  this display.
- **A new DRM master needs no modeset.** When Xorg exited, the kernel
  disabled only the primary plane (`atomic_remove_fb`); the CRTC stayed
  `ACTIVE=1` with its mode (`raw/drm_info-post-handoff.json`). Over the
  session the DRM master changed hands more than 30 times (kmsbench runs, weston
  ×4, cage). weston reused the current mode; cage did a full modeset to the
  same 1080x2340@120 mode. All of them worked. Only *changing* the mode (to
  60 Hz) or disabling the CRTC is known to break on this firmware
  (`notes/direct-boot/RESEARCH.md`), and neither is needed. This also answers
  the GPU agent's question about a second master: it can take over in place.

## Q1: KMS-owning compositor vs Xorg, measured

The panel runs at 120 Hz (8.33 ms). Every row is a 6-8 s run; intervals come
from page-flip events (kmsbench) or `wp_presentation` feedback (wlbench), so
they are real scanout times. "Missed" is the number of refreshes in which no
new frame was shown, as a share of all refreshes.

### CPU composition on the primary plane (no planes)

`wlbench` is a full-screen client. Each frame it redraws a band of M Mpx
(bilinear-scaled content, like a card), attaches a `wl_shm` buffer and
damages the band; the compositor then composites that damage. `kmsbench
flip` composes the same scaled band in-process: shadow plus copy for dumb
buffers, in place for `system` heap buffers.

| Mpx / frame | Moon on Xorg | weston (pixman) | cage (pixman) | kmsbench 1 thread | kmsbench 4 threads | kmsbench 4 threads, heap |
|---|---|---|---|---|---|---|
| 0.50 | 120 | 120, 0 % | 120, 0 % | 120, 0 % | | |
| 0.75 | limit | 120, 0.7 % | 120, 0 % | 120, 0.1 % | | |
| 1.07 (switcher) | mostly 60 | 118.5 fps, 1.7 % | **120, 0 %** | 119.4, 1.0 % | **120, 0 %** | |
| 1.50 | 60 | 111 fps, 7.9 % | **120, 0.1 %** | 117.5, 2.5 % | | |
| 2.00 | 60 | 74 fps, 38 % | **120, 0.3 %** | 60 | | |
| 2.53 (full screen) | 60 | 60 | 108 fps, 10.6 % | 60 | 115 fps, 4.3 % | **120, 0 %** |

Moon on Xorg: `src/a26-shell/README.md` and commit db58693 ("a frame fits
one refresh up to about 0.75 Mpx of damage"; switcher entry about 1.07 Mpx,
mostly 60 Hz). Other runs:
- blending (OVER) of a 1.07 Mpx band: 120 Hz, 0.3 %;
- a 2.53 Mpx unscaled SRC copy: 118.5 fps;
- composing directly into the write-combined buffer with no shadow is
  *slower* (1.07 Mpx: 1.7 % missed, 6.1 ms per frame against 5.1 ms);
- triple buffering changes nothing.

Why cage beats weston: weston's pixman output always uses a shadow
framebuffer ("DRM: output DSI-1 uses shadow framebuffer"), so each damaged
pixel is copied twice (into the shadow, then out to the dumb buffer). wlroots
renders straight into its dumb-buffer swapchain, with damage tracked by
buffer age. In both, the client renders frame N+1 in its own process while
the compositor presents frame N. That pipelining is something Moon on Xorg
can't do, because the X server does all composition on one thread. The atomic
commit itself costs 0.04-0.1 ms.

### Composition by planes

`kmsbench planes`: the primary plane shows a static wallpaper. N cards are
full-screen ARGB buffers (app snapshots) on overlay planes. Each frame
changes only plane properties: scale 0.62-0.98 on the scaling planes,
position (partly off-screen), and alpha 55-100 %.

| Cards (scaling) | Buffers | Missed | CPU per frame | Commit ioctl mean / max |
|---|---|---|---|---|
| 1 (1) | dumb | 0 / 724 | 0.03 ms | 0.48 / 3.5 ms |
| 2 (2) | dumb | 0 / 724 | 0.02 ms | 0.46 / 3.2 ms |
| 3 (3) | dumb | 0 / 724 | 0.02 ms | 0.25 / 2.6 ms |
| 4 (3) | dumb | 0 / 724 | 0.03 ms | 0.25 / 2.3 ms |
| 6 (3) | dumb | 0 / 723 | 0.03 ms | 0.21 / 2.7 ms |
| 3 (3) | cached `system` heap | 0 / 724 | 0.03 ms | 0.29 / 2.9 ms |
| 3 (3) + 0.5 Mpx CPU layer, 2 threads | dumb | 1 / 723 | 1.8 ms | 0.12 / 0.7 ms |
| 3 (3) + 1.07 Mpx CPU layer, 2 threads | dumb | 0 / 724 | 3.3 ms | 0.11 / 0.2 ms |

The hybrid rows are the realistic shell: cards on planes while a renderer
(here pixman) redraws part of the layer beneath them every frame.

### Screen off and on without Xorg

`kmsbench dpms`: `ACTIVE=0`, then `ACTIVE=1` on the unchanged mode, which is
the commit Xorg's DPMS makes for Moon (`display_power.rs`). In 5 of 5
cycles:
- the off commit took 258-268 ms (blocking);
- the on commit took 146-153 ms;
- the first page flip arrived 154-162 ms after the on request.

## Candidates

`build-rootfs.sh`): weston 14.0.2 (+ backend-drm, xwayland, shells), sway
1.12_rc3, cage 0.3.0, labwc 0.20.0, wlroots 0.19.3 / 0.20.2, libliftoff
0.5.0, phoc 0.55.1, phosh 0.55.0, squeekboard 1.43.1, wvkbd 0.19.4,
Xwayland 24.1.14, seatd/libseat 0.9.3, libudev-zero 1.0.4. Smithay is a crate
(0.7.0), not a package.

| Candidate | Runs here | Planes | Fit for Moon |
|---|---|---|---|
| **weston 14** (DRM, pixman) | Yes, measured; libinput finds the touchscreen, keys and touchpad through libudev-zero; Xwayland works | **Never with pixman.** `drm_assign_planes` requires `b->gbm`, which only the GL renderer creates (`state-propose.c:983`), and the pixman renderer has no linux-dmabuf import | Kiosk/desktop shells only, not a phone shell; it has input-method v1 (`weston-keyboard`). A good reference, not a product |
| **cage / wlroots 0.19** (pixman) | Yes, measured; the fastest stock path | wlroots can scan out a fullscreen client dmabuf and has output layers via libliftoff, but with pixman it advertises no linux-dmabuf (missing from `raw/exp-cage/wayland-info.txt`), so client buffers never reach a plane | Single-app kiosk. It offers virtual-keyboard, foreign-toplevel, screencopy and export-dmabuf. Its keymap failed to allocate because the chroot has no `/dev/shm` (fixable with a tmpfs) |
| **sway / labwc** | Same wlroots base (not run) | As cage | Desktop window managers, wrong UX |
| **phoc + phosh** | Not run | wlroots, as above | Built for phones (layer-shell, input-method-v2, gestures), but phosh is a GTK4/libadwaita shell: a CPU-rendered GTK UI at 1080x2340 is the workload this phone can least afford without a GPU. Taking phoc and dropping phosh would leave Moon as a layer-shell client (option B below) |
| **smithay 0.7** (Rust) | Not run | `DrmCompositor` assigns elements to overlay/underlay/primary planes by TEST_ONLY and scans out client dmabufs; it needs GBM, which works here (above) | The natural base if Moon itself becomes the Wayland server: same language, and it has protocols, an Xwayland X11 WM, libinput, layer-shell, input-method and text-input. Its plane logic is built around client buffers; Moon's own animated layers (cards with alpha and scale) need custom dmabuf-backed elements, or Moon's own plane assignment |

**None of the stock compositors gets the plane win without a GPU, and none
provides Moon's UX.** Only a compositor that knows Moon's scene can put the
background, cards and fades on planes.

## Xwayland and the existing X11 apps

- Under weston, Xwayland 24.1.14 started on demand (`xdpyinfo`: X.Org 24.1.14).
  xeyes rendered (`weston-xwayland-xeyes.png`).
- **Moon's real System app ran unchanged** from Moon's own chroot through
  weston's Xwayland (`weston-xwayland-system.png`). Two caveats:
  - Each Magisk `su` session has its own mount namespace, so the app has to
    be started inside the compositor's namespace with the X socket directory
    bind-mounted (`device/a26-wayland-xapp.sh`).
  - weston 14 listens only on the filesystem socket, not the abstract one.
- Not tested: CEF (vimbrowser), which needs its private rootfs. Chromium
  itself supports `--ozone-platform=wayland`. Whether the A26 CEF build
  includes the Wayland ozone backend is unknown.
- Cost: an X11 app under Xwayland without glamor draws into the X server,
  which commits `wl_shm` buffers; the compositor then copies the damage once.
  That's the same copy count as today's ShadowFB. For a static app it's
  nothing; during Moon's animations the app is a snapshot on a plane, so
  there's no per-frame copy.

## Q2: how Moon would move

**A. Moon becomes the Wayland compositor (smithay or a thin custom server),
with KMS composition delegated.** This is the recommended endgame.
- Moon keeps its model, gestures, keyboard UI and lifecycle code (about half
  of its 15.4 k lines are X11-free: model, ui, gfx, text, motion, freezer,
  lease, audio, suspend).
- What changes is the X11-coupled code:
  - `main.rs` (2.6 k lines; WM, XI2, focus);
  - `compositor.rs` (2.3 k; RENDER/Present);
  - `present.rs`, `keyboard.rs` injection (1.6 k; XTEST), `status_bar.rs`,
    `volume.rs`, `display_power.rs`;
  - app launch/close in `apps.rs`.

  These become: Wayland surfaces with xdg-shell instead of
  MapRequest/ConfigureRequest; libinput touch delivered by Moon itself
  instead of XI2 raw touch plus XTEST pointer injection; `wl_keyboard`/text-input
  instead of XTEST; `wp_presentation` and flip events instead of Present; an
  Xwayland X11 WM for System and CEF.

  Estimate: **5-8 k lines changed, about 4-8 weeks of focused work**, most of
  the risk in input and focus (the Chromium/CEF paths Moon special-cases
  today) and in Xwayland window management.
- Benefits beyond speed:
  - the XTEST and X11-sequence workarounds (`notes/a26-shell/X11-INPUT-DEADLOCK.md`)
    disappear;
  - the keyboard can use text-input-v3/input-method-v2, so apps get real
    text, purposes (password, URL, number) and show/hide without Moon's
    IPC;
  - per-client credentials (`SO_PEERCRED`) give Moon app identity for free.

**B. Moon becomes a client of a stock phone compositor (phoc, sway, cage)**
through layer-shell plus foreign-toplevel or a private protocol. Rejected:
- Moon's signature transitions animate *other* apps' pixels: snapshot zooms,
  cards and the finger-following home gesture. A client can only reach those
  through screencopy/export-dmabuf, so every animated frame becomes copies.
- The stock compositors can't use the planes without a GPU.
- phosh-style shells assume GTK.

Estimate: smaller (2-4 k lines), but the result is slower than what option A
or C gives and is shaped by someone else's shell model.

**C. Keep X11 for apps and let a Moon-driven compositor own KMS (the GPU
agent's architecture).** Xorg moves to a headless driver, keeps input,
XI2/XTEST and the WM protocol, and exports the root into a dmabuf; Moon sends
its `Frame` (base plus Image/Fill nodes) to the compositor. This is the
fastest way to the performance win because no app or input code changes.
Its cost is in the GPU agent's document; with this work's results it needs
neither the GPU nor Wayland to beat today's frame rates.

**Xwayland only for legacy apps** isn't a separate option. It is how A and C
keep System and CEF working; under A, System (Rust) can later move to native
Wayland.

## Q3: input, keyboard, screen-off, supervisor

- **Touch and keys through libinput:**
  - libinput (with libudev-zero, no udevd, no logind, libseat `noop`)
    enumerated `sec_touchscreen` (event3), `sec_touchpad`, `gpio_keys`
    (volume) and `sec-pmic-key` (power) inside a chroot. Both weston and cage
    configured them.
  - `sec_touchproximity` has a broken axis range (libinput logs a kernel
    bug) and should be ignored.
  - Moon already reads the power and volume keys from evdev itself; under
    option A it reads the touchscreen the same way or through libinput, and
    the XI2 raw-touch layer goes away.
  - No VT exists (`/dev/tty0` is absent), which libseat noop and running as
    root handle.
- **Keyboard:**
  - Today Moon's keyboard is an override-redirect window that injects XTEST
    keys, plus a CEF-specific CHAR-event workaround.
  - Under Wayland the compositor owns focus, so Moon sends `wl_keyboard`
    keys to the focused surface directly (no injection, no round-trip
    hazard). Better still, Moon implements text-input-v3 (Chromium/ozone
    and GTK speak it) so the app announces the field's purpose and asks for
    the keyboard: no `keyboard show` IPC, and passwords never leave the
    compositor-to-app channel.
  - zwp_virtual_keyboard (cage has it) and input-method-v2 (phoc) are the
    stock equivalents if Moon's keyboard ever runs as a separate client.
  - Through Xwayland, X11 apps get keys from the compositor as normal X
    events (Xwayland synthesises them), so the XTEST path isn't needed for
    them either.
- **Screen-off/DPMS:** measured above. `ACTIVE=0/1` from the compositor works
  in place. The panel takes about 155 ms to the first frame; the off commit
  blocks for about 260 ms and should be issued off the UI thread. Deep
  suspend keeps its own warm-reboot path. The rule "never change the mode,
  never disable the CRTC" carries over unchanged.
- **Coexistence with the boot supervisor and takeover:**
  - The takeover (stop bootanim/SF/HWC, wait for card0 to be free) is
    unchanged.
  - The new session process replaces `a26-xorg-persistent-session` and must
    report readiness (socket up plus first flip) the way the Xorg session
    writes `XORG_READY=1`.
  - The supervisor's `pidof Xorg` liveness and the restore script's Xorg kill
    must learn the compositor's pidfile.
  - The compositor's first commit must sanitize `HDR_FD`/`DQE_FD`.
  - The Xorg session stays the fallback, as in the GPU plan. A26-specific
    pieces with no Wayland equivalent today (the 124-px safe strip, the
    volume HUD) simply become Moon layers.
  - Each `su` session has a private mount namespace: anything that has to
    reach the compositor's sockets from outside (ctl, screenshots) goes
    through the existing chroot `/run` set up by the session itself, as
    Moon's control socket already does.

## How this fits the GPU work: one KMS owner, three tiers

DRM master is exclusive, so there must be exactly one compositor. The design
both documents converge on:

```text
 apps ──Wayland (later) / X11 via Xwayland or headless Xorg (now)──▶ Moon (WM, model, gestures, input, keyboard)
                                                                      │ scene: layers + dmabufs, rects, scale, alpha, zpos
                                                                      ▼
                                    moon-compositor: the only DRM master, first commit sanitizes HDR_FD/DQE_FD
                                      1. planes   up to 8 layers, 3 scaled, per-plane alpha (this work: 0 CPU, 120 Hz)
                                      2. GPU      what planes can't do: rounded/blurred/many layers (GPU agent: 120 Hz, ~1 ms)
                                      3. pixman   fallback without the GPU: 4 threads, cached heap buffers (this work: 2.53 Mpx at 120 Hz)
                                      ACTIVE=0/1 screen power, flip events with kernel timestamps back to Moon
```

- **Plane assignment policy for Moon's `Frame`:**
  - The opaque base (wallpaper, home or app) goes on the primary plane, or on
    an unscaled overlay when a scaled card has to sit under it.
  - Each `Image` node with scale 0.5-8 and no corner repair goes on a scaling
    plane (DPP1/3/7) with plane alpha; below 0.5, use a pre-halved copy
    (mip).
  - Unscaled layers (status strip, keyboard, volume HUD, lock clock and
    keypad) go on the five non-scaling overlays.
  - Rounded card corners go in the ARGB buffer (premultiplied, transparent
    corners). The corner radius then scales with the card; if that matters,
    corners fall back to tier 2/3.
  - Anything left over, or any frame the atomic TEST_ONLY rejects, is
    composed by the GPU into the primary buffer, else by pixman.
  - TEST_ONLY costs about 0.1-0.5 ms, so a frame can afford one attempt plus
    a fallback.
- **Buffers:** everything scanned out is a dmabuf. Use cached `system` heap
  buffers for CPU-written layers (PRIME import works, sync costs 0.4 ms a
  frame). GPU targets come from whatever the GPU agent allocates (it uses
  `system-uncached`); AFBC is possible on DPP1/2/4/7.
- **Where the Wayland server lives** (when it comes): in Moon (Rust, musl,
  smithay protocol pieces), not in the bionic compositor. Moon turns client
  buffers into scene layers:
  - linux-dmabuf buffers are passed through to planes or GPU textures;
  - `wl_shm` buffers are copied once into a heap dmabuf on damage.

  This keeps the bionic process small and policy-free, as the GPU design
  wants.

## Migration plan

Each step ships behind a switch, with today's Xorg session as the fallback.

1. **Make KMS ownership safe and shared.** In moon-compositor (GPU agent
   step 1), add:
   - the `HDR_FD`/`DQE_FD` sanitize;
   - the plane tier and the pixman tier from `scripts/wayland/bench/kmsbench.c`
     (plane assignment, heap buffers, threaded pixman);
   - `ACTIVE` power control, readiness and pidfile for the supervisor.

   Ship it first with the pixman tier alone: Moon on a KMS compositor
   without the GPU already meets 120 Hz for the switcher and fades.
2. **Moon's backend** (GPU agent step 2): `Frame` to scene layers. Prefer
   planes for the switcher, home gesture, launch zoom and lock fades. Fade
   copies and stripe dithering become plane alpha. Present is replaced by the
   compositor's flip events.
3. **Session integration** (GPU agent step 3): supervisor, restore script and
   readiness. Xorg moves to the headless driver.
4. **Wayland front end in Moon (option A):** smithay's protocol, seat and
   Xwayland pieces, with Moon as the WM. System and CEF run through Xwayland
   first (proven here for System); text-input-v3 replaces the XTEST keyboard
   for Wayland-native apps; then headless Xorg goes away.
5. **Native Wayland apps:** System (Rust) ported to wl_surface with dmabuf
   buffers, so it can be put on a plane directly. CEF/vimbrowser gets an
   ozone-wayland build if its CEF supports it.

Not recommended: adopting weston, cage, sway or phoc as the session. They
measured faster than Xorg, but they can't use the planes, have no phone UX
(phosh aside, which needs GTK on a CPU), and would make Moon a client that
can't animate other apps' surfaces cheaply.

## Uncertainties

- wlbench's frame is an opaque scaled band. Moon's real frames mix blends,
  rounded-corner repairs and many small rectangles; the OVER and hybrid
  rows bound this, but Moon itself wasn't ported.
- The benchmarks were meant to run in the same `moon-display` cpu group as
  Xorg (uclamp.min 60). The run wrapper requests it, but membership wasn't
  verified. The governor is `energy_aware`.
- Plane bandwidth: 8 full-screen planes passed TEST_ONLY and 7 (1 primary + 6
  cards) scanned out without a miss for 6 s. Longer runs, thermals and
  AFBC/YUV mixes weren't tested.
- CEF under Xwayland or Wayland wasn't run.

## Reproducing

```sh
scripts/wayland/build-rootfs.sh                 # on whale: rootfs + kmsbench/wlbench
scripts/wayland/phone.sh install                # push to /data/local/a26-wayland
scripts/wayland/phone.sh handoff                # Moon/Xorg off, supervisor frozen, checked
scripts/wayland/phone.sh run kmsbench sanitize  # HDR_FD/DQE_FD = -1 (do this first)
scripts/wayland/phone.sh run exp-kms.sh 6       # probe, bw, gbm, flip/planes matrix, dpms
scripts/wayland/phone.sh run exp-compositor.sh weston 8
scripts/wayland/phone.sh run exp-compositor.sh cage 8
scripts/wayland/phone.sh run env HOLD_SECONDS=50 exp-compositor.sh weston 3 0.5 &
  adb shell su -c 'sh /data/local/tmp/a26-wayland/a26-wayland-xapp.sh 0 25 system'
scripts/wayland/phone.sh restore                # adb reboot, wait for Moon
```

Raw results: `raw/exp-kms-results.txt`, `raw/exp-weston-run1/`, `raw/exp-cage/`,
`raw/drm_info*.{txt,json}`, `raw/handoff-log.txt`.
