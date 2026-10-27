#include "render.h"

#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <GLES2/gl2ext.h>

static const char *VS =
    "#version 300 es\n"
    "layout(location=0) in vec2 a_pos;\n"
    "uniform vec4 u_dst; uniform vec4 u_src; uniform vec2 u_screen;\n"
    "out vec2 v_uv; out vec2 v_local;\n"
    "void main() {\n"
    "  vec2 p = u_dst.xy + a_pos * u_dst.zw;\n"
    "  v_uv = mix(u_src.xy, u_src.zw, a_pos);\n"
    "  v_local = a_pos * u_dst.zw;\n"
    /* Pixel row 0 (the top) is GL row 0: memory row 0 is what KMS scans
     * out first, and textures are uploaded top row first. */
    "  gl_Position = vec4(p / u_screen * 2.0 - 1.0, 0.0, 1.0);\n"
    "}\n";

#define RBOX                                                                   \
    "float rbox(vec2 p, vec2 b, float r) {\n"                                  \
    "  vec2 q = abs(p) - b + r;\n"                                             \
    "  return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;\n"            \
    "}\n"                                                                      \
    "float coverage() {\n"                                                     \
    "  if (u_radius <= 0.0) return 1.0;\n"                                     \
    "  vec2 h = u_dst.zw * 0.5;\n"                                             \
    "  return clamp(0.5 - rbox(v_local - h, h, u_radius), 0.0, 1.0);\n"        \
    "}\n"

#define IMAGE_BODY                                                             \
    "uniform float u_alpha; uniform float u_dim; uniform float u_radius;\n"   \
    "uniform vec4 u_dst; uniform int u_swizzle; uniform int u_opaque;\n"       \
    "in vec2 v_uv; in vec2 v_local; out vec4 o;\n" RBOX                       \
    "void main() {\n"                                                          \
    "  vec4 c = texture(u_tex, v_uv);\n"                                       \
    "  if (u_swizzle == 1) c = c.bgra;\n"                                      \
    "  if (u_opaque == 1) c.a = 1.0;\n"                                        \
    "  o = vec4(c.rgb * u_dim, c.a) * (u_alpha * coverage());\n"               \
    "}\n"

static const char *FS_IMAGE = "#version 300 es\n"
                              "precision highp float;\n"
                              "uniform sampler2D u_tex;\n" IMAGE_BODY;

static const char *FS_IMAGE_EXT =
    "#version 300 es\n"
    "#extension GL_OES_EGL_image_external_essl3 : require\n"
    "precision highp float;\n"
    "uniform samplerExternalOES u_tex;\n" IMAGE_BODY;

static const char *FS_FILL =
    "#version 300 es\n"
    "precision highp float;\n"
    "uniform vec3 u_color; uniform float u_alpha; uniform float u_radius;\n"
    "uniform vec4 u_dst;\n"
    "in vec2 v_uv; in vec2 v_local; out vec4 o;\n" RBOX
    "void main() {\n"
    "  float a = u_alpha * coverage();\n"
    "  o = vec4(u_color * a, a);\n"
    "}\n";

static void image_prog(struct image_prog *p, GLuint id) {
    p->id = id;
    p->dst = glGetUniformLocation(id, "u_dst");
    p->src = glGetUniformLocation(id, "u_src");
    p->screen = glGetUniformLocation(id, "u_screen");
    p->alpha = glGetUniformLocation(id, "u_alpha");
    p->dim = glGetUniformLocation(id, "u_dim");
    p->radius = glGetUniformLocation(id, "u_radius");
    p->swizzle = glGetUniformLocation(id, "u_swizzle");
    p->opaque = glGetUniformLocation(id, "u_opaque");
    glUseProgram(id);
    glUniform1i(glGetUniformLocation(id, "u_tex"), 0);
}

int render_init(struct renderer *r, int width, int height) {
    memset(r, 0, sizeof *r);
    r->no_culling = getenv("MOON_GPU_NO_CULLING") != NULL;
    r->width = width;
    r->height = height;
    GLuint pi = gpu_program(VS, FS_IMAGE), pe = gpu_program(VS, FS_IMAGE_EXT),
           pf = gpu_program(VS, FS_FILL);
    if (!pi || !pf)
        return -1;
    image_prog(&r->image, pi);
    if (pe)
        image_prog(&r->image_ext, pe);
    r->fill.id = pf;
    r->fill.dst = glGetUniformLocation(pf, "u_dst");
    r->fill.src = glGetUniformLocation(pf, "u_src");
    r->fill.screen = glGetUniformLocation(pf, "u_screen");
    r->fill.color = glGetUniformLocation(pf, "u_color");
    r->fill.alpha = glGetUniformLocation(pf, "u_alpha");
    r->fill.radius = glGetUniformLocation(pf, "u_radius");

    static const float quad[] = {0, 0, 1, 0, 0, 1, 1, 1};
    glGenVertexArrays(1, &r->vao);
    glBindVertexArray(r->vao);
    glGenBuffers(1, &r->vbo);
    glBindBuffer(GL_ARRAY_BUFFER, r->vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof quad, quad, GL_STATIC_DRAW);
    glEnableVertexAttribArray(0);
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, 0);

    glGenSamplers(1, &r->sampler_nearest);
    glGenSamplers(1, &r->sampler_linear);
    GLuint s[2] = {r->sampler_nearest, r->sampler_linear};
    for (int i = 0; i < 2; i++) {
        GLint f = i ? GL_LINEAR : GL_NEAREST;
        glSamplerParameteri(s[i], GL_TEXTURE_MIN_FILTER, f);
        glSamplerParameteri(s[i], GL_TEXTURE_MAG_FILTER, f);
        glSamplerParameteri(s[i], GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glSamplerParameteri(s[i], GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    }
    return 0;
}

static uint64_t visible(const struct renderer *r, float x, float y, float w,
                        float h) {
    float x0 = fmaxf(x, 0), y0 = fmaxf(y, 0);
    float x1 = fminf(x + w, (float)r->width), y1 = fminf(y + h, (float)r->height);
    return x1 > x0 && y1 > y0 ? (uint64_t)((x1 - x0) * (y1 - y0)) : 0;
}

/* Binds the image program and its uniforms for one image (draw after). */
static int set_image(struct renderer *r, const struct texture *t, float sx,
                     float sy, float sw, float sh, float dx, float dy,
                     float dw, float dh, float alpha, float dim,
                     float radius, int smooth, float screen_w,
                     float screen_h) {
    const struct image_prog *p =
        t->target == GL_TEXTURE_EXTERNAL_OES ? &r->image_ext : &r->image;
    if (!p->id)
        return 0;
    glUseProgram(p->id);
    glUniform4f(p->dst, dx, dy, dw, dh);
    glUniform4f(p->src, sx / t->width, sy / t->height, (sx + sw) / t->width,
                (sy + sh) / t->height);
    glUniform2f(p->screen, screen_w, screen_h);
    glUniform1f(p->alpha, alpha);
    glUniform1f(p->dim, dim);
    glUniform1f(p->radius, radius);
    glUniform1i(p->swizzle, t->swizzle);
    glUniform1i(p->opaque, t->opaque);
    glActiveTexture(GL_TEXTURE0);
    glBindTexture(t->target, t->id);
    glBindSampler(0, smooth ? r->sampler_linear : r->sampler_nearest);
    return 1;
}

/* Occlusion: what part of each node a later opaque node does not hide, so
 * that hidden pixels are never shaded (an opaque full-screen app over the X
 * mirror leaves only the strips of the mirror around it). */
struct irect {
    int x0, y0, x1, y1;
};
#define MAX_PIECES 16

/* An opaque node covers the pixels of its rectangle that it fully covers
 * (the interior of its destination, in whole pixels). */
static int occluder(const struct renderer *r, const struct mg_node *n,
                    texture_lookup lookup, void *ctx, struct irect *out) {
    if (n->alpha < 1 || n->radius > 0 || n->dst_w <= 0 || n->dst_h <= 0)
        return 0;
    if (n->kind == MG_NODE_IMAGE) {
        const struct texture *t = lookup(ctx, n->surface);
        if (!t || !t->opaque)
            return 0;
    } else if (n->kind != MG_NODE_FILL) {
        return 0;
    }
    out->x0 = (int)ceilf(fmaxf(n->dst_x, 0));
    out->y0 = (int)ceilf(fmaxf(n->dst_y, 0));
    out->x1 = (int)floorf(fminf(n->dst_x + n->dst_w, (float)r->width));
    out->y1 = (int)floorf(fminf(n->dst_y + n->dst_h, (float)r->height));
    return out->x1 > out->x0 && out->y1 > out->y0;
}

/* Cuts `cut` out of the pieces; returns the new count, or -1 if there would
 * be too many pieces (then the node is simply drawn whole). */
static int subtract(struct irect *p, int n, struct irect cut) {
    struct irect out[MAX_PIECES];
    int m = 0;
    for (int i = 0; i < n; i++) {
        struct irect a = p[i];
        if (cut.x0 >= a.x1 || cut.x1 <= a.x0 || cut.y0 >= a.y1 || cut.y1 <= a.y0) {
            if (m == MAX_PIECES)
                return -1;
            out[m++] = a;
            continue;
        }
        int y0 = a.y0 > cut.y0 ? a.y0 : cut.y0, y1 = a.y1 < cut.y1 ? a.y1 : cut.y1;
        struct irect parts[4] = {
            {a.x0, a.y0, a.x1, cut.y0}, /* above */
            {a.x0, cut.y1, a.x1, a.y1}, /* below */
            {a.x0, y0, cut.x0, y1},     /* left */
            {cut.x1, y0, a.x1, y1},     /* right */
        };
        for (int k = 0; k < 4; k++) {
            if (parts[k].x1 <= parts[k].x0 || parts[k].y1 <= parts[k].y0)
                continue;
            if (m == MAX_PIECES)
                return -1;
            out[m++] = parts[k];
        }
    }
    memcpy(p, out, (size_t)m * sizeof *out);
    return m;
}

/* The visible pieces of the screen rectangle (x, y, w, h) under nodes
 * after `after` (all nodes for the base: after = -1). Returns the count
 * (0: hidden), or -1 to draw it whole. */
static int pieces(const struct renderer *r, float x, float y, float w, float h,
                  const struct mg_node *nodes, uint32_t count, int64_t after,
                  const struct irect *occ, const int *has_occ, struct irect *p) {
    p[0] = (struct irect){(int)floorf(fmaxf(x, 0)), (int)floorf(fmaxf(y, 0)),
                          (int)ceilf(fminf(x + w, (float)r->width)),
                          (int)ceilf(fminf(y + h, (float)r->height))};
    int n = 1;
    for (uint32_t j = (uint32_t)(after + 1); j < count && n > 0; j++)
        if (has_occ[j] && (n = subtract(p, n, occ[j])) < 0)
            return -1;
    (void)nodes;
    return n;
}

/* Draws the current program's quad once per piece (scissored), or whole. */
static uint64_t draw_pieces(const struct irect *p, int n, uint64_t whole) {
    if (n < 0) {
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        return whole;
    }
    uint64_t area = 0;
    glEnable(GL_SCISSOR_TEST);
    for (int i = 0; i < n; i++) {
        /* GL row 0 is pixel row 0 (see the vertex shader). */
        glScissor(p[i].x0, p[i].y0, p[i].x1 - p[i].x0, p[i].y1 - p[i].y0);
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        area += (uint64_t)(p[i].x1 - p[i].x0) * (uint64_t)(p[i].y1 - p[i].y0);
    }
    glDisable(GL_SCISSOR_TEST);
    return area;
}

uint64_t render_frame(struct renderer *r, const struct texture *base,
                      const struct mg_node *nodes, uint32_t count,
                      texture_lookup lookup, void *ctx) {
    float W = (float)r->width, H = (float)r->height;
    uint64_t fragments = 0;
    enum { MAX_OCC = 64 };
    struct irect occ[MAX_OCC];
    int has_occ[MAX_OCC];
    int culling = !r->no_culling && count <= MAX_OCC;
    for (uint32_t i = 0; culling && i < count; i++)
        has_occ[i] = occluder(r, &nodes[i], lookup, ctx, &occ[i]);
    struct irect p[MAX_PIECES];
    int n;
    glViewport(0, 0, r->width, r->height);
    glDisable(GL_BLEND);
    /* Always cleared first: a tiler then never loads the old frame, and
     * whatever no node shows is black. */
    glClearColor(0, 0, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    if (base) {
        n = culling ? pieces(r, 0, 0, W, H, nodes, count, -1, occ, has_occ, p) : -1;
        if (n && set_image(r, base, 0, 0, (float)base->width, (float)base->height, 0, 0, W,
                           H, 1, 1, 0, 0, W, H))
            fragments += draw_pieces(p, n, (uint64_t)(W * H));
    }
    glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
    int blending = 0;
    for (uint32_t i = 0; i < count; i++) {
        const struct mg_node *nd = &nodes[i];
        if (nd->alpha <= 0 || nd->dst_w <= 0 || nd->dst_h <= 0)
            continue;
        uint64_t area = visible(r, nd->dst_x, nd->dst_y, nd->dst_w, nd->dst_h);
        if (!area)
            continue;
        const struct texture *t = NULL;
        if (nd->kind == MG_NODE_IMAGE && !(t = lookup(ctx, nd->surface)))
            continue;
        if (nd->kind != MG_NODE_IMAGE && nd->kind != MG_NODE_FILL)
            continue;
        n = culling ? pieces(r, nd->dst_x, nd->dst_y, nd->dst_w, nd->dst_h, nodes, count,
                             i, occ, has_occ, p)
                    : -1;
        if (!n)
            continue;
        int blend = nd->alpha < 1 || nd->radius > 0 || (t && !t->opaque);
        if (blend != blending) {
            (blend ? glEnable : glDisable)(GL_BLEND);
            blending = blend;
        }
        if (t) {
            if (!set_image(r, t, nd->src_x, nd->src_y, nd->src_w, nd->src_h, nd->dst_x,
                           nd->dst_y, nd->dst_w, nd->dst_h, nd->alpha, nd->dim, nd->radius,
                           nd->flags & MG_NODE_SMOOTH, W, H))
                continue;
        } else {
            glUseProgram(r->fill.id);
            glUniform4f(r->fill.dst, nd->dst_x, nd->dst_y, nd->dst_w, nd->dst_h);
            glUniform4f(r->fill.src, 0, 0, 1, 1);
            glUniform2f(r->fill.screen, W, H);
            glUniform3f(r->fill.color, ((nd->color >> 16) & 255) / 255.f,
                        ((nd->color >> 8) & 255) / 255.f, (nd->color & 255) / 255.f);
            glUniform1f(r->fill.alpha, nd->alpha);
            glUniform1f(r->fill.radius, nd->radius);
        }
        fragments += draw_pieces(p, n, area);
    }
    return fragments;
}

void render_copy(struct renderer *r, const struct texture *from, float sx,
                 float sy, float sw, float sh, int width, int height,
                 float radius) {
    glViewport(0, 0, width, height);
    glDisable(GL_BLEND);
    int scaled = (int)sw != width || (int)sh != height;
    if (set_image(r, from, sx, sy, sw, sh, 0, 0, (float)width, (float)height, 1, 1,
                  radius, scaled, (float)width, (float)height))
        glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}
