/* Draws moon-compositor frames: textured and solid rounded rectangles with
 * alpha and dim, premultiplied blending, into a dmabuf render target. */
#ifndef MOON_GPU_RENDER_H
#define MOON_GPU_RENDER_H

#include "gpu.h"
#include "proto.h"

struct texture {
    GLuint id;
    GLenum target;   /* GL_TEXTURE_2D or GL_TEXTURE_EXTERNAL_OES */
    int width, height;
    int swizzle;     /* bytes are B, G, R, A (Moon's canvases) */
    int opaque;      /* ignore alpha */
};

struct renderer {
    int width, height;
    int no_culling; /* draw hidden parts too (MOON_GPU_NO_CULLING=1, tests) */
    GLuint vao, vbo;
    GLuint sampler_nearest, sampler_linear;
    struct image_prog {
        GLuint id;
        GLint dst, src, screen, alpha, dim, radius, swizzle, opaque;
    } image, image_ext;
    struct fill_prog {
        GLuint id;
        GLint dst, src, screen, color, alpha, radius;
    } fill;
};

int render_init(struct renderer *r, int width, int height);

/* Looks a node's texture up; NULL draws nothing. */
typedef const struct texture *(*texture_lookup)(void *ctx, uint32_t id);

/* Draws a frame into the bound framebuffer. Returns fragments drawn. */
uint64_t render_frame(struct renderer *r, const struct texture *base,
                      const struct mg_node *nodes, uint32_t count,
                      texture_lookup lookup, void *ctx);

/* Copies src (pixels of `from`) into all of the bound framebuffer (width x
 * height), with transparent (premultiplied) rounded corners of `radius`
 * when it is positive. */
void render_copy(struct renderer *r, const struct texture *from, float sx,
                 float sy, float sw, float sh, int width, int height,
                 float radius);

#endif
