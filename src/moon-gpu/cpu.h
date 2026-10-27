/* moon-compositor's CPU tier: draws frames without the GPU, in parallel
 * bands, into cached memory (a dma-heap "system" buffer that KMS scans out
 * after a cache flush). Pixels are native-endian 0xAARRGGBB, premultiplied,
 * like Moon's canvases (DRM XRGB8888/ARGB8888). */
#ifndef MOON_GPU_CPU_H
#define MOON_GPU_CPU_H

#include <stdint.h>

#include "proto.h"

struct cpu_image {
    const uint8_t *px;
    int width, height, stride;
    int opaque; /* ignore the alpha byte */
};

struct cpu_target {
    uint8_t *px;
    int width, height, stride;
};

typedef const struct cpu_image *(*cpu_lookup)(void *ctx, uint32_t id);

/* Starts `threads` workers (the caller's thread is one of them). */
void cpu_init(int threads);

/* Draws a frame: `base` 1:1 (or black), then the nodes in order. */
void cpu_render(const struct cpu_target *t, const struct cpu_image *base,
                const struct mg_node *nodes, uint32_t count, cpu_lookup lookup,
                void *ctx);

/* Copies src (pixels of `from`) scaled into all of `to`, with transparent
 * rounded corners of `radius` (in `to` pixels) when radius > 0. */
void cpu_copy(const struct cpu_target *to, const struct cpu_image *from,
              float sx, float sy, float sw, float sh, float radius);

#endif
