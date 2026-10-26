# GPU compositing for Moon: research, design and migration plan

Status: research and proof of concept (October 2026). Device: SM-A266M,
A266MUBU1AYB8, OSS kernel (5.10.223, GKI KMI), Moon direct boot.

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
Xorg's modesetting driver and a GPU compositor cannot both drive CRTC 120.

| Option | Assessment |
| --- | --- |
| Keep Xorg on KMS, add glamor | Glamor needs GBM and a GBM EGL platform; the blob has neither (probed). Rejected. |
| Keep Xorg on KMS, GPU renders, Xorg shows it | Without glamor the modesetting driver never page-flips (`present.c:278`, `if (!ms->drmmode.glamor) return FALSE`) and cannot import a dmabuf as a pixmap (no DRI3). The best it could do is CPU-copy a GPU frame through MIT-SHM and the shadow framebuffer: two 10 MB copies a frame on the CPU, no flips, tearing. Rejected. |
| Wayland compositor + Xwayland | Right long-term shape, but Moon is an X11 window manager and both apps and the browser are X11. wlroots needs GBM; Smithay would need a custom renderer/allocator for this blob. Rewriting the WM, input, keyboard and lifecycle code first is a large migration with no performance gain over the option below. Deferred. |
| **A GPU compositor that owns KMS, Moon drives it, Xorg moves to a headless driver** | Chosen. Below. |

### The chosen architecture

```text
             Alpine chroot (musl)                       Android side (bionic)
 ┌──────────────────────────────────────────┐    ┌──────────────────────────────┐
 │ Xorg :0 on the dummy video driver         │    │ moon-compositor              │
 │  evdev touch/keys, XI2, XTEST, RENDER     │    │  DRM master on card0          │
 │  apps (System, CEF) draw here as today    │    │  EGL/GLES on libGLES_mali     │
 │        │ root: CopyArea into a dmabuf     │    │  dmabuf framebuffers, atomic  │
 │        ▼ (MIT-SHM AttachFd)               │    │  flips with IN_FENCE_FD        │
 │  "X mirror" dmabuf ───────────────────────┼───▶│  textures: X mirror, layers,  │
 │                                           │    │  snapshots                    │
 │ Moon (a26-shell)                          │    │                               │
 │  WM, input, gestures, model, ui.rs        │    │                               │
 │  compositor.rs: Frame { base, nodes }  ───┼───▶│  renders the Frame, flips at  │
 │  CPU-drawn layers (memfd)              ───┼───▶│  vblank, reports presentation │
 │  ◀──────────── presented(seq, time) ──────┼────│                               │
 └──────────────────────────────────────────┘    └──────────────────────────────┘
```

- **moon-compositor** (C, NDK; `src/moon-gpu/`) is the only DRM master.
  It renders Moon's existing scene description (`compositor::Frame`: a base
  surface plus Image/Fill nodes with source/destination rectangles, alpha,
  corner radius and filtering) with two GLES programs, one fragment pass per
  node, and flips one frame per vblank. A dim is a uniform, not a
  pre-darkened copy; a rounded corner is a signed-distance coverage term, not
  a mask repair; scaled snapshots are bilinear textures. Full-frame redraw
  costs the GPU about a millisecond (measured below), so damage tracking
  becomes an optimisation for power rather than a condition for 120 Hz.
- **Surfaces.** Moon's CPU-drawn layers (wallpaper, home, lock clock,
  keypad, labels) are written into shared memory (memfd) and uploaded with
  `glTexSubImage2D` of the changed rectangles, exactly where Moon's
  `Uploader` sends `PutImage` today. Snapshots (cards) are GPU copies of the
  X mirror taken when Moon asks, where `capture_root` copies the root today.
- **X content.** Xorg keeps running with all its input drivers, XI2,
  XTEST, focus and the WM protocol, but on the `dummy` video driver: it no
  longer touches DRM. The visible root (apps, status strip, keyboard and
  volume windows) is copied by the X server into a dmabuf-backed MIT-SHM
  pixmap (`ShmAttachFd` maps any fd whose `fstat` has a size; dmabufs have
  `i_size` = buffer size, `drivers/dma-buf/dma-buf.c:496`), on Moon's
  request, for the rectangles X DAMAGE reported. A trailing one-pixel
  `ShmPutImage` with `send_event` gives Moon an asynchronous completion event
  in request order, so no reply is ever awaited on Moon's connection. The
  compositor samples that dmabuf as a texture with no further copy. This is
  the one CPU copy apps already pay today (shadow framebuffer to scanout).
- **Pacing.** The compositor reports each flip's vblank sequence and
  kernel timestamp to Moon, which replaces Present's completion events in
  `compositor.rs` (`ready`, `next_display_time`, the 60 Hz fallback
  heuristics become unnecessary at 120 Hz but stay as a safety net).
- **Screen off** becomes `ACTIVE=0/1` on the CRTC, the same commit DPMS
  makes today (`display_power.rs`), requested over the socket.

What must run for this path: the kernel's mali_kbase and exynos-drm, nothing
else. gralloc's allocator HAL and gpuservice keep running as today but are
not used; SurfaceFlinger and the HWC stay stopped.

## Q3: risk, fallback and robustness

- **The software path stays the default and the fallback.** GPU mode is a
  session-level switch (`"gpu": true` in Moon's config plus the
  compositor binary present). Moon's XRender compositor is untouched and
  remains what runs when the switch is off, when moon-compositor fails its
  startup check (EGL, dmabuf import, first modeset, first flip within a
  deadline), or after repeated runtime failures. Starting Xorg with the
  modesetting configuration is exactly today's session.
- **A compositor crash must not blank the panel.** DRM framebuffers belong
  to the open file, not the process: if the card is opened by a tiny holder
  that keeps the fd and passes duplicates to moon-compositor (`SCM_RIGHTS`),
  a crashed compositor leaves its last frame on screen with the CRTC still
  active, and its restart continues on the same file. Without a holder, the
  kernel removes the framebuffers on close and, because the primary plane
  loses its framebuffer, turns the CRTC off (`atomic_remove_fb`). Whether
  this firmware can re-enable the CRTC from a second master in place was a
  hard question for the existing design (deep suspend and a runtime modeset
  could not); it is measured below.
- **Moon reconnects.** Moon keeps the CPU copy of every layer already
  (`Uploader::shadow`), so a restarted compositor is re-fed in one frame.
- Timeouts: Moon never blocks on the compositor socket (nonblocking,
  bounded queue); if no presentation arrives within 250 ms it drops the
  frame clock to a timer, as it already does when Present stops.

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

## Migration plan

1. **moon-compositor daemon** (from the PoC): socket protocol, surfaces,
   Frame rendering, X mirror import, presentation events, power on/off, fd
   holder, startup self-test.
2. **Moon GPU backend** in `compositor.rs`, behind the existing
   `Compositor` API (create_layer/upload/create_snapshot/capture_root/
   compose/handle_event): selected at startup, the XRender backend stays.
   App switcher and background fades first: with dims as uniforms, the
   fade copies (`create_dimmed`, `set_rows`, `RowFade`) are skipped in GPU
   mode.
3. **Session integration**: the supervisor starts the holder and
   moon-compositor after the HWC handoff, Xorg with the dummy-driver
   configuration, then Moon with the socket path; on any startup failure,
   today's modesetting Xorg session.
4. Later: every shell surface as a compositor layer (status strip, keyboard
   and volume windows leave X), KMS overlay planes for the foreground app
   (no composition at all while an app is static), AFBC buffers, and
   eventually a Wayland front end for apps.
