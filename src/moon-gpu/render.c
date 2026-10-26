#include "render.h"

#include <math.h>
#include <stdio.h>
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

static void draw_image(struct renderer *r, const struct texture *t, float sx,
                       float sy, float sw, float sh, float dx, float dy,
                       float dw, float dh, float alpha, float dim,
                       float radius, int smooth, float screen_w,
                       float screen_h) {
    const struct image_prog *p =
        t->target == GL_TEXTURE_EXTERNAL_OES ? &r->image_ext : &r->image;
    if (!p->id)
        return;
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
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
}

uint64_t render_frame(struct renderer *r, const struct texture *base,
                      const struct mg_node *nodes, uint32_t count,
                      texture_lookup lookup, void *ctx) {
    float W = (float)r->width, H = (float)r->height;
    uint64_t fragments = 0;
    glViewport(0, 0, r->width, r->height);
    glDisable(GL_BLEND);
    if (base) {
        draw_image(r, base, 0, 0, (float)base->width, (float)base->height, 0,
                   0, W, H, 1, 1, 0, 0, W, H);
        fragments += (uint64_t)(W * H);
    } else {
        glClearColor(0, 0, 0, 1);
        glClear(GL_COLOR_BUFFER_BIT);
    }
    glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
    int blending = 0;
    for (uint32_t i = 0; i < count; i++) {
        const struct mg_node *n = &nodes[i];
        if (n->alpha <= 0 || n->dst_w <= 0 || n->dst_h <= 0)
            continue;
        uint64_t area = visible(r, n->dst_x, n->dst_y, n->dst_w, n->dst_h);
        if (!area)
            continue;
        if (n->kind == MG_NODE_IMAGE) {
            const struct texture *t = lookup(ctx, n->surface);
            if (!t)
                continue;
            int blend = n->alpha < 1 || n->radius > 0 || !t->opaque;
            if (blend != blending) {
                (blend ? glEnable : glDisable)(GL_BLEND);
                blending = blend;
            }
            draw_image(r, t, n->src_x, n->src_y, n->src_w, n->src_h, n->dst_x,
                       n->dst_y, n->dst_w, n->dst_h, n->alpha, n->dim,
                       n->radius, n->flags & MG_NODE_SMOOTH, W, H);
        } else if (n->kind == MG_NODE_FILL) {
            int blend = n->alpha < 1 || n->radius > 0;
            if (blend != blending) {
                (blend ? glEnable : glDisable)(GL_BLEND);
                blending = blend;
            }
            glUseProgram(r->fill.id);
            glUniform4f(r->fill.dst, n->dst_x, n->dst_y, n->dst_w, n->dst_h);
            glUniform4f(r->fill.src, 0, 0, 1, 1);
            glUniform2f(r->fill.screen, W, H);
            glUniform3f(r->fill.color, ((n->color >> 16) & 255) / 255.f,
                        ((n->color >> 8) & 255) / 255.f,
                        (n->color & 255) / 255.f);
            glUniform1f(r->fill.alpha, n->alpha);
            glUniform1f(r->fill.radius, n->radius);
            glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
        } else {
            continue;
        }
        fragments += area;
    }
    return fragments;
}

int render_copy(struct renderer *r, const struct texture *from, float sx,
                float sy, float sw, float sh, int width, int height,
                struct texture *out) {
    memset(out, 0, sizeof *out);
    out->target = GL_TEXTURE_2D;
    out->width = width;
    out->height = height;
    out->opaque = from->opaque;
    glGenTextures(1, &out->id);
    glBindTexture(GL_TEXTURE_2D, out->id);
    glTexStorage2D(GL_TEXTURE_2D, 1, GL_RGBA8, width, height);
    GLuint fbo;
    glGenFramebuffers(1, &fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D,
                           out->id, 0);
    int ok = glCheckFramebufferStatus(GL_FRAMEBUFFER) == GL_FRAMEBUFFER_COMPLETE;
    if (ok) {
        glViewport(0, 0, width, height);
        glDisable(GL_BLEND);
        int scaled = (int)sw != width || (int)sh != height;
        draw_image(r, from, sx, sy, sw, sh, 0, 0, (float)width, (float)height,
                   1, 1, 0, scaled, (float)width, (float)height);
    }
    glBindFramebuffer(GL_FRAMEBUFFER, 0);
    glDeleteFramebuffers(1, &fbo);
    if (!ok) {
        glDeleteTextures(1, &out->id);
        out->id = 0;
        return -1;
    }
    return 0;
}
