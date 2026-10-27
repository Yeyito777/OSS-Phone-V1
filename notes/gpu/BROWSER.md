# GPU acceleration for the Browser (vimbrowser on the A26)

Research and proposal, 2026-10-26, with first measurements on the phone
(below). Estimates are labelled as such.

## Measured (2026-10-27, `scripts/gpu/browser-bench.py`, 10 s of swipes on a Wikipedia article, GPU mode, planes off)

| Run | Screen updates | page rAF | GPU process | renderers | browser | Xorg | Moon | moon-compositor | total |
|---|---|---|---|---|---|---|---|---|---|
| stock launcher | 47.4/s | 58.2/s | 15.5 % | 33.2 % | 0.7 % | 2.5 % | 11.5 % | 13.6 % | 76.9 % of a core |
| `--disable-gpu-compositing` | 45.9/s | 58.4/s | 15.9 % | 32.3 % | 0.5 % | 2.4 % | 11.7 % | 13.8 % | 76.6 % |
| `--disable-gpu-rasterization` | 47.6/s | 58.5/s | 15.7 % | 31.2 % | 0.8 % | 2.4 % | 11.4 % | 14.3 % | 75.9 % |

Chromium's own GPU report (DevTools `SystemInfo.getInfo`, stock launcher)
explains why A changes nothing: `gpu_compositing` is already
`disabled_software` (Chromium never composites on SwiftShader), while
`rasterization` is `enabled`, on "ANGLE (Google, Vulkan 1.3.0 (SwiftShader
Device ...))": tiles are rastered by Skia on GL on SwiftShader's Vulkan,
i.e. emulated GPU raster on the CPU, in the GPU process. The next flag to
measure is therefore `--disable-gpu-rasterization` (Skia's native CPU
raster in the renderers), measured in the third row: it took effect
(`rasterization` became `disabled_software`) and changed nothing either.
So on this page SwiftShader costs little: the GPU process's ~16 % is viz
compositing in software and presenting to X, the renderers' ~32 % is
raster, layout and script, and the display path behind the Browser (Moon's
mirror copies, 11-12 %, and moon-compositor, ~14 %) is another quarter of a
core. Only real GPU work (C) or fewer copies (B) can lower these. `webgl`
is `enabled_readback`, `vulkan`
`disabled_off`, video decode `disabled_software`.

The Mali's Vulkan driver (`moon-gpu-vkprobe`): Vulkan 1.1.177 (loader
1.3.0), and all three extensions Venus needs on an Android host:
`VK_EXT_external_memory_dma_buf`, `VK_EXT_image_drm_format_modifier`,
`VK_EXT_queue_family_foreign` (plus `VK_KHR_external_memory_fd`, semaphore
and fence fds, timeline semaphores).

## How the Browser draws today

vimbrowser (CEF 147.0.10, Chromium 147.0.7727.118, linuxarm64) runs as root
in a nested Debian bookworm arm64 root (`/opt/vimbrowser-a26/rootfs`, glibc)
with `--ozone-platform` X11, started by vimbrowser's
`scripts/a26/phone-launch.sh` with:

    --use-gl=angle --use-angle=swiftshader --enable-unsafe-swiftshader
    --disable-gpu-sandbox --disable-accelerated-video-decode
    --disable-accelerated-video-encode --no-zygote --force-device-scale-factor=2.5

vimbrowser's `docs/a26-phone-shell.md` explains the constraints: CEF Views
needs a working GPU process (`--disable-gpu` makes it exit after repeated
GPU-process failures), so the GPU process runs on SwiftShader, a CPU
emulation of GLES; `/dev/video*` must never be visible in the app root
(enumerating them enters Samsung's FIMC camera driver, which can panic the
stock kernel).

So every frame is CPU work twice over, plus copies:

1. **Raster on the CPU.** Chromium blocklists GPU rasterization on
   SwiftShader, so renderers raster tiles with Skia on raster threads.
2. **Compositing on emulated GL.** The display compositor runs on SwiftShader
   in the GPU process: real GLES draws, executed by the CPU.
3. **Presentation by copy.** SwiftShader's X11 window surface puts the frame
   into the X server (`XPutImage`/`XShmPutImage`, copy 1). Xorg runs
   headless on the dummy driver in GPU mode; Moon copies the visible root
   into the X mirror, a dmabuf the compositor owns (copy 2, through MIT-SHM
   on Moon's request), and moon-compositor samples it on the Mali when it
   composes the screen. At 1080x2340 a frame is 10 MB, so each copy is
   600 MB/s of memory traffic at 60 updates/s.
4. **Video** decodes in software (FFmpeg, libvpx, dav1d).

## What the Mali offers a glibc process

The Mali driver is Android's (bionic): `libGLES_mali.so` (EGL/GLES 3.2,
DDK r32p1) and, on Android 15 devices, a Vulkan driver loaded through
Android's `libvulkan.so` (to be confirmed: `moon-gpu-vkprobe`, below). A
glibc process cannot load bionic libraries. `notes/gpu/DESIGN.md` (Q1)
rejected libhybris for the compositor; for the Browser the root *is*
glibc, which libhybris targets, but it still needs a linker matched to
Android 15's linker namespaces and the TLS-slot patches every Halium port
carries, and nobody maintains that for Android 15. It stays the fallback
idea, not the plan.

The useful finding is that **CEF's arm64 build already contains everything
on the Chromium side** (checked in `third_party/cef-a26-arm64` on whale):

- a Vulkan loader (`libvulkan.so.1`) and Chromium's Vulkan support
  (`--use-vulkan`, `ui::VulkanImplementationX11`);
- ANGLE with its Vulkan back end (`VK_KHR_xcb_surface` and
  `VK_KHR_wayland_surface` in `libGLESv2.so`);
- `viz::SkiaOutputDeviceX11`, Chromium's read-back presenter for a GPU
  that cannot present to the X window itself;
- the Ozone Wayland platform (`ozone_platform_wayland.cc`) next to X11.

So any Vulkan driver that works inside the Debian root would give Chromium
GPU raster and compositing with no Chromium or CEF rebuild.

## Options

| | Option | Gain (estimate) | Effort | Risk |
|---|---|---|---|---|
| A | Chromium's software compositor instead of SwiftShader GL | none: measured, Chromium already composites in software (above); the emulated cost is GPU raster, see A' | hours | low |
| B | Zero-copy presentation into moon-compositor (Wayland) | Removes the X and mirror copies: 2 x 10 MB per update | weeks | medium |
| C | Real GPU raster and compositing through Venus (Vulkan remoting) | Raster and composite leave the CPU: the largest win on heavy pages | 1 week spike, weeks to ship | high |
| D | Hardware video decode (MFC over V4L2, or Codec2) | Video decode leaves the CPU | weeks | high (kernel panics) |

### A. Software compositing (do first)

Keep the GPU process alive on SwiftShader (CEF Views needs it) but tell
Chromium not to composite with it: `--disable-gpu-compositing`, which is
not `--disable-gpu`. Viz then composites with Skia on the CPU
(`SoftwareRenderer`) into a shared-memory bitmap that viz puts into the X
window with MIT-SHM. That is the path Chromium itself uses when
it has no usable GPU; SwiftShader is meant as a fallback for WebGL, and
emulating a GLES pipeline costs more than compositing tiles directly.
WebGL keeps working on SwiftShader.

To check: whether CEF Views on this target accepts it (the GPU process is
still started); scroll smoothness; CPU per process. One flag in
vimbrowser's launcher, so a change in that repository, after measurement.

### B. Zero-copy presentation (with the Wayland work)

The copies in steps 3 and 4 above exist because the Browser is an X11
client of a headless X server. With Chromium's Ozone Wayland platform and
Moon as the Wayland compositor (the Wayland agent's M2), a software frame
arrives as a `wl_shm` buffer that moon-compositor samples directly
(protocol v3 offsets, v4 `MG_SURFACE_FOREIGN`): one upload into the GPU
instead of two CPU copies, and nothing at all for unchanged frames. With C,
the frame would be a dmabuf, imported, not copied.

The cost is on vimbrowser's side: its A26 code is X11-specific (the XInput
watcher that turns raw drags into wheel gestures, the XTEST printable-key
workaround, the keyboard IPC and window code). It is the right long-term
shape, but it depends on M2 and on vimbrowser's maintainers.

### C. Venus: the Mali's Vulkan, remoted

Venus is Mesa's Vulkan driver for virtio-gpu guests. Besides a VM it can
talk to virglrenderer's Vulkan server (`vkr`) over a Unix socket: vtest
(`virgl_test_server --venus`, client `VN_DEBUG=vtest`). Mesa's Venus page
lists the host requirements for an **Android host** as Vulkan 1.1 with
`VK_EXT_external_memory_dma_buf`, `VK_EXT_image_drm_format_modifier` and
`VK_EXT_queue_family_foreign`, and lists **ARM Mali r32p0+** among the
drivers it has been tested on; this phone's DDK is r32p1.

The shape:

- `virgl_test_server --venus` built with the NDK (bionic, like
  moon-compositor), running beside moon-compositor on the Mali's Vulkan
  driver, listening on a socket bind-mounted into the app root;
- Mesa's Venus ICD (`libvulkan_virtio.so`) built for the Debian bookworm
  root (bookworm's `mesa-vulkan-drivers` has no Venus, so a small Mesa
  build with `-Dvulkan-drivers=virtio` in vimbrowser's arm64 builder
  container);
- Chromium with `--use-vulkan=native --enable-features=Vulkan` (Skia on
  Vulkan) or `--use-angle=vulkan` (ANGLE on Vulkan), presenting through
  Vulkan's X11 surface if Venus's WSI works over vtest, else through
  `SkiaOutputDeviceX11` (read back, then `XPutImage`: the same copies as
  today, but no CPU raster or composite).

Estimate: if the probe passes, a spike to `vulkaninfo` and a first Chromium
frame on the Mali is about a week; making it robust (server lifetime,
restarts, memory, the X or Wayland presentation) several more. Gains:
Chromium on a GPU typically spends a fraction of the CPU of software raster
on heavy pages; the cost is Venus's per-command marshalling over the socket
and vtest's single-threaded server.

Risks, in order:

1. The Mali's Vulkan driver must expose the three extensions above.
   `moon-gpu-vkprobe` answers that in seconds.
2. Mesa's own caveat: vkr relies on implementation-defined behaviour for
   `vkMapMemory` and "is not expected to work on all drivers meeting the
   requirements".
3. vtest is a test transport (one server process, synchronous parts). It
   is what exists without a virtio-gpu kernel driver; nothing better is
   available on this kernel.
4. Venus's X11 WSI over vtest is unproven; the read-back presenter is the
   fallback.
5. GPU memory and power are shared with moon-compositor: the server needs
   the same care (priority, a clock floor while frames flow).

gfxstream (the Android Emulator's and Cuttlefish's GLES/Vulkan streaming)
is the alternative remoting stack; it is larger to build and is made for
emulator transports, so Venus comes first.

### D. Hardware video decode

The Exynos 1280's MFC is a stateful V4L2 decoder. Chromium's V4L2 decoders
are built for ChromeOS (`use_v4l2_codec`); Linux desktop builds, CEF's
included, use VA-API or software. It would need a custom CEF build, exposing
exactly the MFC nodes in the app root (never the camera's, see the panic
above) under the names Chromium probes, and, without C, CPU conversion of
the decoded NV12 frames for software compositing. Samsung's Codec2 HAL is
the Android way, through binder from a bionic helper: more plumbing still.
Worth it only after C, and only for battery during video.

## Recommendation

1. **Measure the baseline** on the phone (below), then **A** in the same
   slot by changing only the compositing flag; propose the flag to
   vimbrowser's launcher with the numbers.
2. **Run `moon-gpu-vkprobe`** in the same slot. If the three extensions are
   there, **spike C** off-device first (virglrenderer and Venus builds),
   then on the phone with `vulkaninfo`, then Chromium.
3. **B** with the Wayland agent once M2 direct mode is in, as vimbrowser's
   Wayland port.
4. **D** last, if at all.

## Measuring

- `scripts/gpu/browser-bench.py [--url URL] [--seconds N] [--label L]`:
  with the Browser open in front, loads the page over its DevTools port
  (9222, forwarded with adb), drives real swipes through Moon's touch path
  for N seconds, and reports CPU time per process group (browser, GPU
  process, renderers, other Chromium children, Xorg, Moon, moon-compositor),
  screen updates per second (moon-compositor's flips, from its profile
  log) and the page's requestAnimationFrame rate.
- For a variant (A), run the launcher with the changed flag through a
  temporary bind mount over `/opt/vimbrowser-a26/bin/vimbrowser-a26`, so
  no installed file changes.
- `moon-gpu-vkprobe [-a]` (built with the compositor): Vulkan API and driver
  version, the extensions above, memory types. Read-only.

Not measured on whale: it is an x86 desktop, and the costs that matter here
(memory bandwidth of the copies, the A78/A55 cores, SwiftShader on arm64)
do not transfer.
