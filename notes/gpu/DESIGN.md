# GPU compositing for Moon: research, design and migration plan

Status: research, an on-device proof of concept, and an integration
verified on the phone behind `GPU_COMPOSITOR` (off by default) (October
2026). Device: SM-A266M,
A266MUBU1AYB8, OSS kernel (5.10.223, GKI KMI), Moon direct boot. Companion
study: `notes/wayland/DESIGN.md` on `wayland-experiment` (the Wayland
agent's measurements of planes, CPU composition and stock compositors).

## Goal and the problem

Moon composes every frame inside Xorg with RENDER (pixman on the CPU); Xorg
runs the modesetting driver with `AccelMethod none`, a shadow framebuffer
and no page flips. A frame fits one 120 Hz refresh only below about
0.5-0.75 Mpx of redraw, so Moon limits damage aggressively, the app switcher
(about 1.07 Mpx a frame) runs at 60 Hz, and background fades use dozens of
pre-darkened copies (about 400 MB). The panel is 1080x2340 = 2.53 Mpx at
120 Hz: a full frame is 10.1 MB, and 120 of them a second are 1.2 GB/s of
writes before any blending. A CPU compositor cannot do that and leave the
CPU for anything else; the Mali-G68 is idle.

## What is on the device (probed over adb, Moon mode)

| Item | Finding |
| --- | --- |
| GPU | `Mali-G68 4 cores r1p1 0x9204` (`/sys/class/misc/mali0/device/gpuinfo`), 104-897 MHz, `Interactive` DVFS governor |
| Kernel driver | Samsung's `mali_kbase` module (DDK `r32p1-01bet2`, UK 11.31), `/dev/mali0` mode 0666 |
| User driver | `/vendor/lib64/egl/libGLES_mali.so` (40 MB, DDK `r32p1-01bet2`, Android/bionic). NEEDED: libnativewindow, libutils, libgralloctypes, liblog, libion_exynos, libbase, libz, libcutils, mapper@4.0, libhidlbase, libhardware, libc++, libc, libm, libdl |
| EGL extensions in the blob | `EGL_EXT_image_dma_buf_import` (+`_modifiers`), `EGL_ANDROID_native_fence_sync`, `EGL_KHR_surfaceless_context`, `EGL_KHR_no_config_context`, `EGL_IMG_context_priority`, `EGL_KHR_partial_update`, `EGL_ARM_implicit_external_sync`, `EGL_ANDROID_image_native_buffer`. **No** GBM, `EGL_KHR/MESA_platform_gbm`, `EGL_EXT_platform_device`, Wayland or fbdev platforms |
| Display driver | `exynos-drmdpu` (`drivers/gpu/drm/samsung/dpu`), atomic, universal planes, `DRM_CAP_PRIME=3`, `ADDFB2_MODIFIERS`, monotonic timestamps, `CRTC_IN_VBLANK_EVENT`; no async flips, no syncobj |
| Display topology | CRTC 120 (DECON0, video mode, 1080x2340@120 preferred, @60 also listed) -> DSI connector 123; CRTC 121 for writeback (connector 115, ABGR8888/NV12) |
| Planes | 0-3 (+ more DPP channels): primary/overlay, `IN_FENCE_FD`, `zpos` 0-8, plane `alpha`, blend modes, rotation, `ARM_AFBC` modifiers on some planes, NV12/P010 on video planes |
| Buffers | `/dev/dma_heap/{system,system-uncached,...}`; exynos-drm allocates its own dumb buffers from `system-uncached` (`exynos_drm_gem.c:117`) and imports any dmabuf with PRIME |
| Graphics services in Moon mode | running: `vendor.gralloc-4-0` (allocator HAL), `gpu` (gpuservice), hidl allocator; stopped: `surfaceflinger`, `vendor.hwcomposer-2-4` (Moon's takeover stops them so Xorg can be DRM master) |
| Chroot | Alpine 3.24.1 (musl) with mesa, libdrm, drm_info, xorg-server; Xorg on `/dev/dri/card0` (modesetting, ShadowFB, no glamor) |

So the GPU and its full driver are alive in Moon mode; only the
window-system half of Android (SurfaceFlinger, HWC) is gone, and nothing in
the GPU path needs it.

## Q1: how to reach the Mali from the Linux side

### libhybris (Halium, Ubuntu Touch, Droidian, Sailfish): rejected

libhybris loads bionic libraries into a glibc process with its own copy of
the Android linker. Upstream targets glibc; bionic graphics blobs read fixed
TLS slots (`TLS_SLOT_OPENGL_API = 6`, `TLS_SLOT_OPENGL = 7` in
`bionic_tls.h`) that collide with the host libc's thread layout, which is
why every Halium/Sailfish port also patches its Android base
(hybris-patches). postmarketOS, whose userspace is musl like ours, did
carry a libhybris package (with `glamor-hybris` for X 2D acceleration), but
it failed to build on musl targets (pmaports issue #700) and the project
dropped Halium support for lack of users (pmaports issue #703), preferring
mainline drivers. For us it would mean a glibc chroot or a glibc compositor
binary, a libhybris build matched to Android 15's linker and namespaces, and
still no window system: the blob has no GBM, so stock glamor cannot use it.
All of that buys nothing a bionic process doesn't already have: the driver,
its linker namespaces and its HAL dependencies, natively.

### Panfrost: not with this kernel

Mesa's Panfrost supports the G68 (Valhall v9, job manager) in userspace,
but the kernel driver only learned the G68 (`GPU_MODEL(g68, 0x9004, r1p0,
r1p1)` in `drivers/gpu/drm/panfrost/panfrost_gpu.c`) after Linux 6.16;
v6.16 and older have no G68 entry, and Valhall JM support as a whole starts
around 6.2 (MT8192's G57). This phone runs a 5.10 GKI kernel with
`CONFIG_TRIM_UNUSED_KSYMS=y` (only KMI symbols are exported, so a large
out-of-tree DRM driver would not even link against it), and Samsung's GPU
power domain, clocks and DVFS live inside the kbase platform code. It would
also replace mali_kbase, which gpuservice and Android use. Panfrost becomes
interesting only on a mainline kernel, which is a different project.

### A bionic compositor process using EGL/GLES directly: chosen

A small native (NDK) process outside the chroot, like Moon's existing
`moon-audiod`, links Android's `libEGL`/`libGLESv3`, which load the Mali
blob. It needs no window system:

- `eglGetDisplay(EGL_DEFAULT_DISPLAY)`, a surfaceless ES 3.2 context with
  high priority (`EGL_IMG_context_priority`);
- render targets are dmabufs imported with `EGL_EXT_image_dma_buf_import`
  into renderbuffers (no gralloc, no ANativeWindow);
- each frame ends with an `EGL_ANDROID_native_fence_sync` sync_file that
  goes straight into the KMS plane's `IN_FENCE_FD`, so the CPU never waits
  for the GPU.

The display side has three candidates:

| Path | Verdict |
| --- | --- |
| Samsung's HWC HAL (composer 2.4) | Rejected. It needs the composer service (stopped in Moon mode), HIDL/AIDL plumbing, HWC2's validate/present protocol with a SurfaceFlinger-like client, and it owns DRM master itself. It is what Droidian/Sailfish do through libhybris because they have no other path. |
| gralloc buffers | Not a display path: the allocator only allocates. Useful later for AFBC-compressed buffers whose layout gralloc knows. |
| **DRM/KMS directly with dmabuf framebuffers** | Chosen. It is the same atomic API the HWC uses underneath; Xorg already proves the CRTC/connector/plane on this firmware. Planes take `IN_FENCE_FD`; flips deliver kernel vblank timestamps. No Android service beyond the kernel and the GPU driver is involved. Android's own recovery UI (minui) drives KMS the same way. |

## Q2: the display path

Who owns KMS decides everything else, because DRM master is exclusive:
Xorg's modesetting driver and any other compositor cannot both drive CRTC
120.

| Option | Assessment |
| --- | --- |
| Keep Xorg on KMS, add glamor | Glamor needs GBM and a GBM EGL platform; the blob has neither (probed). Rejected. |
| Keep Xorg on KMS, GPU renders, Xorg shows it | Without glamor the modesetting driver never page-flips (`present.c:278`, `if (!ms->drmmode.glamor) return FALSE`) and cannot import a dmabuf as a pixmap (no DRI3). The best it could do is CPU-copy a GPU frame through MIT-SHM and the shadow framebuffer: two 10 MB copies a frame on the CPU, no flips, tearing. Rejected. |
| A stock Wayland compositor (weston, cage, sway, phoc) as the session | Measured by the Wayland agent (`notes/wayland/DESIGN.md`, branch `wayland-experiment`): faster than Xorg on the CPU (cage holds 120 Hz up to 2.0 Mpx a frame), but none uses the display planes without a GPU, none has Moon's UX, and Moon would become a client that cannot animate other apps' pixels cheaply. Rejected by both studies. |
| **One Moon-driven compositor that owns KMS** | Chosen, by both studies independently. What it shows comes from Moon's scene (`compositor::Frame`); how it shows it is tiered: planes, then GPU, then CPU. Below. |

### The chosen architecture

```text
             Alpine chroot (musl)                         Android side (bionic)
 ┌────────────────────────────────────────────┐    ┌────────────────────────────────┐
 │ Xorg :0 on the dummy video driver           │    │ moon-compositor (src/moon-gpu) │
 │  evdev touch/keys, XI2, XTEST, RENDER       │    │  the only DRM master on card0  │
 │  apps (System, CEF) draw here as today      │    │  per frame, the cheapest of:   │
 │        │ root DAMAGE: CopyArea into         │    │   1. display planes (DPP0-7)   │
 │        ▼ a MIT-SHM pixmap on the dmabuf     │    │   2. GPU: GLES on the Mali     │
 │  "X mirror" (cached dma-heap) ──────────────┼───▶│   3. CPU: 4 threads, cached    │
 │                                             │    │  flips with IN_FENCE_FD, flip  │
 │ Moon (a26-shell)                            │    │  events with kernel timestamps │
 │  WM, input, gestures, model, ui.rs          │    │  ACTIVE=0/1 for screen off/on  │
 │  compositor.rs + gpu.rs: Frame { base,  ────┼───▶│                                │
 │    nodes }, layers in dma-heap buffers  ────┼───▶│                                │
 │  ◀─────────────── presented(seq, time) ─────┼────│                                │
 └────────────────────────────────────────────┘    └────────────────────────────────┘
```

- **moon-compositor** (C, NDK) owns DRM master and shows Moon's existing
  scene description (`compositor::Frame`: a base surface plus Image/Fill
  nodes with source/destination rectangles, alpha, corner radius and
  filtering) one frame per vblank. Each frame takes the first tier that can
  show it:
  1. **Planes.** The base and every node on its own DPP plane: plane alpha
     for fades, the three scaling planes (DPP1/3/7, 0.5x-8x) for scaled
     nodes, and a dim of the bottom layer as plane alpha over the CRTC's
     black. Rounded corners only where the buffer has them baked in at that
     scale (switcher card copies). One TEST_ONLY commit decides; nothing is
     drawn. The Wayland agent measured this tier: up to six moving, scaling,
     fading full-screen cards over a background at 120 Hz with 0 missed
     vblanks and 0.03 ms of CPU a frame, and with a 1.07 Mpx CPU-drawn
     layer under them still 0 missed.
  2. **GPU.** Two GLES programs draw the frame into a dmabuf: a dim is a
     uniform, a rounded corner a signed-distance coverage term, scaled
     snapshots are bilinear textures. The proof of concept below runs a
     heavier scene than any Moon frame at 120 Hz in 3.4 ms of GPU time.
  3. **CPU.** Without a usable EGL (the blob fails to load, or `--cpu`),
     four threads draw the frame in interleaved bands into cached `system`
     dma-heap buffers, which are flushed and scanned out. The Wayland agent
     measured the pixman equivalent at 2.53 Mpx a frame, 120 Hz, 0 missed,
     and found the rule that makes it possible: never read a scanout buffer
     that is mapped uncached (40.9 ms for a frame against 1.3 ms cached).
     This tier is the fallback, not the target.
- **Surfaces.** Moon's CPU-drawn layers (wallpaper, home, lock clock,
  keypad, labels) live in cached `system` dma-heap buffers that Moon
  allocates and writes between `DMA_BUF_IOCTL_SYNC` start and end, exactly
  where its `Uploader` sent `PutImage` before; the compositor samples them
  in place or scans them out. Snapshots (cards) are copies of the X mirror
  made by the compositor when Moon asks, where `capture_root` copied the
  root before; for the switcher, a second copy at card size with the card's
  corners baked in lets the cards go on planes.
- **X content.** Xorg keeps running with all its input drivers, XI2,
  XTEST, focus and the WM protocol, but on the `dummy` video driver: it no
  longer touches DRM. The visible root (apps, status strip, keyboard, volume
  and banner windows) is copied by the X server into a MIT-SHM pixmap on the
  compositor's mirror dmabuf (`ShmAttachFd` maps any fd whose `fstat` has a
  size; dmabufs have `i_size` = buffer size, `drivers/dma-buf/dma-buf.c:496`),
  for the rectangles X DAMAGE reported. A trailing one-pixel `ShmPutImage`
  with `send_event` gives Moon an asynchronous completion event in request
  order, so no reply is ever awaited on Moon's connection. The compositor
  cleans the mirror's caches (about 0.4 ms, measured by the Wayland agent)
  and shows it: on the primary plane while an app is in front (nothing
  drawn), or as a texture inside a frame. The CopyArea is the one CPU copy
  apps already paid (shadow framebuffer to scanout).
- **Pacing.** The compositor reports each flip's vblank sequence and
  kernel timestamp to Moon, replacing Present's completion events in
  `compositor.rs` (`ready`, `next_display_time`).
- **Screen off** is `ACTIVE=0/1` on the CRTC, the commit DPMS makes today
  (`display_power.rs`), requested over the socket.

What must run for this path: the kernel's mali_kbase and exynos-drm, nothing
else. gralloc's allocator HAL and gpuservice keep running as today but are
not used; SurfaceFlinger and the HWC stay stopped.

## The two answers compared: this design and Moon as a Wayland compositor

The Wayland agent's study (`notes/wayland/DESIGN.md`) reached the same KMS
half: one Moon-driven compositor, planes first, then GPU, then CPU. The two
designs differ only in how apps reach that compositor. Its recommended
endgame (option A there) makes Moon a Wayland server (smithay pieces, an
Xwayland X11 WM for System and CEF); this one keeps X11 with Xorg headless.
Its option C *is* this design.

| | This design: X11 apps, headless Xorg, X mirror | Moon as a Wayland compositor (+ Xwayland) |
| --- | --- | --- |
| Shell animations | Identical: the same tiered compositor shows Moon's frames | Identical |
| Latency, shell | A frame is on screen at the next vblank after Moon submits it; touch goes evdev → Xorg (XI2) → Moon, one local socket hop more than libinput in Moon | Next vblank; touch read by Moon directly |
| Latency, apps | App → X server (CPU, pixman) → CopyArea into the mirror (about 1.6 ms for a full 10 MB frame, cached to cached) → next vblank, scanned out directly | X11 apps: the same copy (Xwayland's `wl_shm` buffer into a dmabuf). Native Wayland apps with dmabufs: none |
| Tearing | The mirror is one buffer the display scans while Xorg writes it (as Xorg tears today); double-buffering it is a known step | Client buffers are released per frame: no tearing |
| Robustness | More processes (Xorg, moon-compositor, Moon); each failure lands on today's software path, in place (measured: a compositor-to-Xorg handoff without a reboot) | Fewer processes, but the new code sits in the most fragile layers (input, focus, Xwayland window management) |
| Migration cost | Built on `gpu-compositor`: about 2.1k lines of C, 1.2k of Rust and 0.5k of scripts; no app, input or keyboard code changes | 5-8k lines changed, 4-8 weeks (its estimate), mostly input, focus, keyboard and Xwayland |
| App compatibility | System and CEF unchanged: same X server, same XTEST keyboard and taps | System ran under Xwayland; CEF under Xwayland or ozone-wayland untested; text-input-v3 would replace the XTEST keyboard |
| X11 workarounds | Stay (the sequence-number deadlock rules, XTEST injection) | Disappear |

They are steps, not rivals. This design delivers the 120 Hz shell now
without touching apps or input. moon-compositor, its protocol and Moon's
backend are the KMS half the Wayland endgame needs; going there later
replaces the X mirror with per-surface client buffers and retires headless
Xorg, and nothing in the compositor has to change.

## Q3: risk, fallback and robustness

- **Opt-in, and the software path stays the fallback.** GPU mode is
  `GPU_COMPOSITOR=1` in `/data/adb/moon/config` (`scripts/gpu/install.sh
  --enable|--disable`). Moon's XRender compositor is untouched and is what
  runs when the switch is off, when the compositor binary is missing, when
  it is not ready within four seconds (EGL, buffers, scanout, the first
  commit), or after a GPU-mode failure earlier in the same boot. Xorg on the
  panel with the modesetting configuration is exactly today's session.
- **At runtime** (`scripts/device/moon-gpu-session.sh keep`): a dead
  compositor is restarted (twice per boot); Moon notices the closed socket,
  reconnects once a second and re-sends every layer (it keeps them all), and
  the mirror is re-attached. After the restarts the session falls back in
  place: headless Xorg is stopped, the takeover starts Xorg on the panel
  again (no modeset is needed: the CRTC stays active across masters), and
  the supervisor, which watches for that switch every second, restarts Moon
  on the software path. Wi-Fi and audio keep running.
- **A hung compositor** (alive, no flips) is not detected yet: Moon keeps
  running (its frame watchdog moves on after 250 ms) but the screen
  freezes. Moon could report it to the session script; not built.
- **Kernel hazards.** Every commit sets `HDR_FD=-1` on every plane it
  touches and a modeset `DQE_FD=-1` (see the panic below). The mode is never
  changed and the CRTC never disabled other than through `ACTIVE`.
- **Restoring Android** (`a26-android-graphics-restore.sh`) stops the
  compositor before it starts the HWC.
- **Deep suspend** (opt-in, off by default) still hands off through
  `xrandr --off`, which does nothing on the headless Xorg; it is not
  supported together with GPU mode yet.

## Proof of concept

`src/moon-gpu/poc.c` (`moon-gpu-poc`): the architecture above minus Moon.
It opens card0 as master with atomic modesetting, allocates three
1080x2340 ABGR8888 dmabufs from `system-uncached`, imports each as an EGL
render target and as a KMS framebuffer, and renders a Moon-like app
switcher every refresh: the full-screen wallpaper fading between 100 % and
45 % brightness, three 70 %-scale app cards scrolling sideways with soft
shadows and 48 px rounded corners. That is about 5 Mpx of fragments for a
full 2.53 Mpx frame. A second scene zooms a card from a dock icon to full
screen and back. Each frame's native fence goes into `IN_FENCE_FD` and the
frame is flipped nonblocking with a page-flip event; the next frame is
rendered as soon as the event arrives, for the vblank after it. Statistics
come from kernel timestamps: the flip events' vblank times and sequence
numbers, and the sync_file signal time of each GPU fence.

### Results on the phone (2026-10-26, OSS kernel, Moon stopped for the run)

`scripts/gpu/poc-run.sh` stops Moon's session (with the supervisor's
liveness checks deferred), runs the PoC, hands the display back to Xorg
and Moon, and reboots. Logs and per-frame CSVs are in `notes/gpu/poc/`;
`switcher-scanout.png` is the scanout buffer of the last switcher frame,
read back from its dmabuf.

Flip intervals from kernel vblank timestamps, 120 Hz panel (8.296 ms):

| Scene | GPU clock | Flips | Interval mean / p99 / max (ms) | Missed vblanks | GPU time per frame mean / p99 (ms) |
| --- | --- | --- | --- | --- | --- |
| switcher, 7.2 Mpx fragments | floor 507 MHz | 723 in 6.01 s | 8.296 / 8.407 / 8.727 | **0** | 3.36 / 4.61 |
| switcher | floor 702 MHz | 723 in 6.01 s | 8.296 / 8.416 / 8.627 | **0** | 3.16 / 3.78 |
| switcher | Samsung DVFS only | 980 in 10.01 s | 10.20 / 16.88 / 17.28 | 225 (23 %) | 6.28 / 8.47 |
| switcher | DVFS only, 30 s after boot | 723 in 6.01 s | 8.296 / 8.308 / 8.334 | 0 | 4.68 / 6.24 |
| zoom, 3.6 Mpx | floor 507 MHz | 723 in 6.01 s | 8.296 / 8.405 / 9.080 | **0** | 3.50 / 4.30 |
| zoom | DVFS only | 1178 in 10.0 s | 8.486 / 16.59 / 16.90 | 27 (2.3 %) | 5.65 / 7.58 |
| switcher without shadows, 4.2 Mpx | floor 403 MHz | 722 in 6.01 s | 8.307 / 8.384 / 16.19 | 1 | 4.30 / 5.31 |
| switcher without shadows | DVFS only | 703 in 6.01 s | 8.532 / 16.60 / 16.84 | 20 (2.8 %) | 5.68 / 7.48 |
| wallpaper fade only, 2.53 Mpx | DVFS only | 481 in 4.01 s | 8.313 / 8.419 / 16.59 | 1 | 5.75 / 7.17 |
| switcher after CRTC off/on (x2) | floor 507 MHz | 240 + 240 | 8.296 / 8.33 / 8.48 | **0** | 3.93 / 4.70 |

"GPU time" is from the start of the frame's submission to its fence's
signal timestamp; the frame is rendered right after the previous flip's
event, so it has to be done within one refresh. CPU time to record a frame
is 1.3-1.5 ms mean (2-3 ms p99) on the shell's cores.

Findings:

- **The GPU has the headroom; the frame is bounded by DVFS, not
  throughput.** Offscreen with the GPU kept busy, the switcher scene renders
  at 597 frames/s into linear dmabufs (1.67 ms a frame) and 847 frames/s
  into GPU-native (AFBC) textures. Paced at one frame per vblank, Samsung's
  `Interactive` governor sees ~45 % load, settles at 312-507 MHz and frames
  take 5-8 ms, so some miss. A clock floor while animating (here
  `/sys/kernel/gpu/gpu_min_clock` = 507000) gives 0 misses with 4-5 ms of
  slack per frame. The compositor holds the floor only while frames keep
  coming, the same idea as Moon's CPU utilization clamp.
- **Moon's app switcher today**: about 1.07 Mpx of pixman work a frame,
  paced at 60 Hz. The GPU composites a full 2.53 Mpx frame from 7.2 Mpx of
  textured, blended, rounded and shadowed fragments in 3.4 ms, at 120 Hz.
- **DPMS-style screen off/on works under our own master**: `ACTIVE=0` in
  265 ms, `ACTIVE=1` in 151-164 ms, then flips continue at 120 Hz.
- **In-place handoff back to the software path works**: the PoC kept its
  last frame on screen with DRM master dropped, Xorg started, became master,
  modeset and drew; Moon started on it and answered (`view: locked`). So a
  GPU-path failure can fall back to today's Xorg without a reboot.
- The compositor daemon's startup (KMS, EGL, three targets, X mirror
  imported as a plain 2D texture with a 4352-byte stride) takes 124 ms.

**A kernel panic, and why every KMS client here must reset HDR_FD.** The
first attempt reset the phone (`notes/gpu/poc/panic-hdr-fd-excerpt.txt`):
`__memcpy` from `hdr_prepare_context` in `exynos_drm`, reached from
`exynos_plane_atomic_check` during our first nonblocking commit. Samsung's
planes carry an `HDR_FD` property, a file descriptor that the driver
resolves *in the committing process* on every commit
(`exynos_p2/exynos_drm_hdr.c`, `dma_buf_get(hdr_fd)` then `vmap` and copy
`total_bytesize` bytes). The HWC sets it (16 on this firmware) and it stays
in the plane state after the HWC exits. Xorg never touches it and is lucky
that its own fd 16 is not a dmabuf; in our process fd 16 was one of our
render targets, whose first word, once drawn, made the copy run past the
end of the kernel buffer. `kms.c` now sets `HDR_FD = -1` on its plane in
every commit and `DQE_FD = -1` on the CRTC in a modeset. Two smaller
lessons: when the CRTC is already active, the first commit must be
blocking (otherwise its stray flip event offsets every later one and
commits fail with EBUSY), and a fresh frame should be committed only after
the previous one's flip event.

## What is built (branch `gpu-compositor`)

| Piece | Where |
| --- | --- |
| moon-compositor: KMS (all planes, scaler probe, fd sanitising), the three tiers, the X mirror, copies, the protocol, a GPU clock floor while frames flow | `src/moon-gpu/{compositor,kms,gpu,render,cpu}.c`, `proto.h` |
| Moon's backend: layers in dma-heap buffers, Frame to the wire, the X mirror with DAMAGE and completion markers, snapshots and card copies, exact dims instead of darkened copies, flip events as the frame clock, reconnects | `src/a26-shell/src/gpu.rs`; switches in `compositor.rs`, `ui.rs`, `main.rs`, `display_power.rs` |
| Session: start/keep/stop, the takeover loop with in-place fallback, the supervisor (MOON_GPU_SOCKET, fallback watch), restore, the chroot bind of `/run/moon-gpu` | `scripts/device/moon-gpu-session.sh`, `a26-xorg-takeover.sh`, `moon-boot-supervisor.sh`, `a26-android-graphics-restore.sh`, `a26-enter-chroot.sh`, `a26-xorg-persistent-session.sh` |
| Headless Xorg configuration and the dummy driver | `scripts/gpu/xorg/20-a26-gpu.conf`, `scripts/gpu/install.sh` |
| Off-device test: Moon in GPU mode on Xvfb against a protocol stand-in | `scripts/gpu/mock-run.sh`, `scripts/gpu/mock-compositor.py` |

Verified off-device first (whale, Xvfb, the stand-in compositor): Moon
connects, uploads its layers, and the lock and home screens and the unlock
animation arrive frame by frame, paced by presentation events; an app (xev)
in front is shown through the X mirror; the switcher shows the app's
snapshot, at rest through its card copy. The CPU tier's renderer was
checked on whale (rounded, scaled, translucent and dimmed nodes; 4.9 ms for
a heavy frame on four x86 threads).

### On the phone (2026-10-26, main f0b6332 plus this branch)

Both display paths were measured in one session with the same Moon build
and the same injected 120 Hz touches (`scripts/gpu/gestures.sh`,
`scripts/gpu/transitions.sh`); numbers are Moon's own per-animation
summaries (back-to-back flip intervals from vblank timestamps). Raw output
in `notes/gpu/round/`.

GPU mode came up on the first boot: the compositor was ready 108 ms after
it started (EGL, buffers, the X mirror, the scaler probe found DPP 61, 75
and 103), and Moon was ready at 22 s of uptime, as on the software path.

| Transition (two runs each) | Software path (XRender) | GPU mode |
| --- | --- | --- |
| Launch from home | 59-60 frames, 0 missed, mean 8.30 ms | 61-62 frames, 0 missed, mean 8.30 |
| Swipe up out of an app | 53 frames, 3 missed, **10 paced at 60 Hz**, mean 10.37 | 82-83 frames, 0-1 missed, mean 8.30-8.40 |
| Slow swipe up into the switcher | 60-61 frames, 0-1 missed | 63-64 frames, 0 missed |
| Switcher scroll | 78-84 frames, 1-2 missed | 81-85 frames, 0 missed |
| Switcher back | 72-86 frames, 0-4 missed, **10 at 60 Hz** in one run | 85 frames, 0 missed |
| Lock screen drag to the keypad | 38 frames, 0-1 missed | 53 frames, 0 missed |
| Keypad back to the clock | 64 frames, 2 missed | 47-49 frames, 0 missed |
| Banner in, over home | 38-39 frames, 0 missed | 44-45 frames, 0 missed |
| Banner out | 16-17 frames, 0 missed | 42-43 frames, 0-1 missed |
| Notification centre in / out | 88-90 frames, 0 missed | 87-90 frames, 0 missed |
| Banner over an app | 58-59 frames, 1-2 missed | 60 frames, 0 missed |
| Ringing screen from screen-off | not measurable as an animation (frames during panel power-up) | same; once lit, frames every 16.6 ms on both paths while Moon ramps the backlight |

Over all gesture runs in GPU mode: about 900 frames with 1 missed vblank.
The software path's frames fit 120 Hz most of the time but fall to 60 Hz
for the heaviest moments (swipe-out, switcher back), and its animations
show fewer distinct frames because it limits redraw.

- **The GPU clock floor matters.** At a 507 MHz floor the GPU drew Moon's
  frames in 5-6 ms (full-screen layers sampled from linear buffers) and 1-4
  of every 80 frames missed; at 702 MHz, 3.8 ms on average and none. The
  floor is now 702 MHz while frames flow, released 150 ms after.
- **Planes take half the frames.** In the final runs 704 of 1311 gesture
  flips went straight to planes; every GPU-drawn one was a card whose corner
  radius changes with its scale. In the notification transitions planes took
  a third; the rest had a dimmed layer above the bottom or a layer under
  64x16.
- **Found and fixed on the device:** the kernel rejected plane commits with
  `ENOSPC` when a node's source reached a fraction of a pixel past its
  buffer (Moon's card nodes do), and a 1:1 layer must be exactly 1:1 on a
  non-scaling plane. With the source snapped and clamped, a switcher-scroll
  loop went from 40 % to 97 % planes.

Battery (on battery via slate mode, 60 s each, same session, brightness
128; `notes/gpu/round/drain.txt`):

| Scenario | Software path | GPU mode |
| --- | --- | --- |
| Home screen, idle | 220 mA, CPU 1 % | 218 mA, CPU 2 % |
| Switcher scrolling continuously | 443 mA, CPU 15 % | 337 mA, CPU 10 % (GPU-drawn); **273 mA, CPU 7 %** (planes) |
| Screen off | 37 mA, CPU 1 % | **46 mA, CPU 2 %** (under investigation) |

Fallback, on the phone: killing the compositor twice restarted it within a
second and Moon reconnected and re-sent its layers both times; the third
kill made the session give up on GPU mode for the boot, run Xorg on the
panel again and restart Moon, which answered 14 s after the kill, with no
reboot. A reboot with `GPU_COMPOSITOR=0` returned the plain software path.

## Migration plan

1. **On the phone, opt-in** (done, above): merged with `GPU_COMPOSITOR`
   off by default. Open: the screen-off drain in GPU mode.
2. **Default on** once a week of daily use shows no regressions; the
   software session remains the fallback.
3. **Fewer copies and more planes:** double-buffer the X mirror (no
   tearing); the status strip, keyboard, volume and banner windows as Moon
   layers on planes instead of X windows; the foreground app's region of the
   mirror on its own plane; AFBC for GPU output (DPP1/2/4/7 scan it out).
4. **Wayland front end** (the Wayland agent's option A): Moon as the
   Wayland server with an Xwayland WM, per-surface client buffers instead of
   the mirror, headless Xorg retired. moon-compositor and its protocol stay.
