# moon-gpu

Moon's GPU compositor: native (bionic, NDK) programs that run outside the
Alpine chroot, next to Android's Mali driver, own DRM master on
`/dev/dri/card0`, render with EGL/GLES into dmabufs and page-flip them with
atomic KMS. Design, measurements and the migration plan:
[`notes/gpu/DESIGN.md`](../../notes/gpu/DESIGN.md).

| File | Role |
| --- | --- |
| `kms.c` | atomic KMS over raw ioctls (libdrm is a private platform library on Android): connector/CRTC/primary plane, dmabuf framebuffers, nonblocking flips with `IN_FENCE_FD`, flip events, CRTC on/off |
| `gpu.c` | surfaceless high-priority ES 3.2 context, dma-heap allocation, dmabuf render targets (`EGL_EXT_image_dma_buf_import`), native fences |
| `render.c` | the compositor's frame renderer: textured/solid rounded rectangles with alpha and dim, premultiplied |
| `proto.h` | the socket protocol between Moon and `moon-compositor` |
| `compositor.c` | `moon-compositor`, the daemon |
| `poc.c` | `moon-gpu-poc`, the proof of concept and benchmark |

Build with `scripts/gpu/build.sh` (NDK r27; output in `images/moon-gpu/`).
Run the proof of concept on an attached phone with `scripts/gpu/poc-run.sh`:
it stops Moon for about a minute and reboots the phone afterwards.

Every KMS commit sets the plane's `HDR_FD` to -1: exynos-drm resolves that
property as a file descriptor of the committing process, and the value the
HWC leaves behind panicked the kernel once (see DESIGN.md).
