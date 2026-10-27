/* Off-device check of render.c on any EGL/GLES 3 (Mesa on a desktop): each
 * frame is drawn with and without occlusion culling and the two must match
 * pixel for pixel.  cc -o render-test render-test.c ../render.c -lEGL -lGLESv2 -lm */
#include "../render.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define W 1080
#define H 2340

GLuint gpu_program(const char *vs, const char *fs) {
    GLuint p = glCreateProgram();
    const char *src[2] = {vs, fs};
    GLenum type[2] = {GL_VERTEX_SHADER, GL_FRAGMENT_SHADER};
    for (int i = 0; i < 2; i++) {
        GLuint s = glCreateShader(type[i]);
        glShaderSource(s, 1, &src[i], NULL);
        glCompileShader(s);
        GLint ok;
        glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
        if (!ok) /* the external-image program needs an extension Mesa may lack */
            return 0;
        glAttachShader(p, s);
    }
    glLinkProgram(p);
    return p;
}

static struct texture tex[4];

static const struct texture *lookup(void *ctx, uint32_t id) {
    (void)ctx;
    return id >= 1 && id <= 4 && tex[id - 1].id ? &tex[id - 1] : NULL;
}

static void make_tex(int i, int w, int h, int opaque, unsigned seed) {
    unsigned char *px = malloc((size_t)w * h * 4);
    for (int y = 0; y < h; y++)
        for (int x = 0; x < w; x++) {
            unsigned char *q = px + ((size_t)y * w + x) * 4;
            q[0] = (unsigned char)(x * 7 + seed);
            q[1] = (unsigned char)(y * 3 + seed * 5);
            q[2] = (unsigned char)((x ^ y) + seed);
            q[3] = opaque ? 255 : (unsigned char)(x + y);
            if (!opaque) /* premultiplied */
                for (int c = 0; c < 3; c++)
                    q[c] = (unsigned char)(q[c] * q[3] / 255);
        }
    tex[i] = (struct texture){.target = GL_TEXTURE_2D, .width = w, .height = h,
                              .opaque = opaque};
    glGenTextures(1, &tex[i].id);
    glBindTexture(GL_TEXTURE_2D, tex[i].id);
    glTexImage2D(GL_TEXTURE_2D, 0, GL_RGBA8, w, h, 0, GL_RGBA, GL_UNSIGNED_BYTE, px);
    free(px);
}

static struct mg_node image(uint32_t id, float x, float y, float w, float h, float sw,
                            float sh) {
    return (struct mg_node){.kind = MG_NODE_IMAGE, .surface = id, .src_w = sw, .src_h = sh,
                            .dst_x = x, .dst_y = y, .dst_w = w, .dst_h = h, .alpha = 1,
                            .dim = 1};
}

static uint64_t shaded;

static unsigned char *draw(struct renderer *r, const struct texture *base,
                           const struct mg_node *n, uint32_t count, int cull) {
    r->no_culling = !cull;
    shaded = render_frame(r, base, n, count, lookup, NULL);
    unsigned char *px = malloc((size_t)W * H * 4);
    glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, px);
    return px;
}

int main(void) {
    EGLDisplay d = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    eglInitialize(d, NULL, NULL);
    eglBindAPI(EGL_OPENGL_ES_API);
    EGLint ca[] = {EGL_CONTEXT_MAJOR_VERSION, 3, EGL_NONE};
    EGLContext ctx = eglCreateContext(d, EGL_NO_CONFIG_KHR, EGL_NO_CONTEXT, ca);
    if (!ctx || !eglMakeCurrent(d, EGL_NO_SURFACE, EGL_NO_SURFACE, ctx)) {
        printf("no surfaceless GLES 3 context\n");
        return 2;
    }
    GLuint fb, rb;
    glGenRenderbuffers(1, &rb);
    glBindRenderbuffer(GL_RENDERBUFFER, rb);
    glRenderbufferStorage(GL_RENDERBUFFER, GL_RGBA8, W, H);
    glGenFramebuffers(1, &fb);
    glBindFramebuffer(GL_FRAMEBUFFER, fb);
    glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_RENDERBUFFER, rb);
    struct renderer r;
    if (render_init(&r, W, H)) {
        printf("render_init failed\n");
        return 2;
    }
    make_tex(0, W, H, 1, 1);       /* 1: the X mirror */
    make_tex(1, W, 2216, 1, 2);    /* 2: an opaque app */
    make_tex(2, 400, 120, 0, 3);   /* 3: a translucent HUD */
    make_tex(3, 900, 600, 0, 4);   /* 4: a translucent popup */

    struct mg_node f1[] = {image(1, 0, 0, W, H, W, H), image(2, 0, 124, W, 2216, W, 2216),
                           image(3, 340, 200, 400, 120, 400, 120)};
    struct mg_node f2[] = {image(1, 0, 0, W, H, W, H), image(2, 0, 124, W, 2216, W, 2216),
                           image(4, 90, 800, 900, 600, 900, 600),
                           {.kind = MG_NODE_FILL, .dst_x = 100.5f, .dst_y = 1700.25f,
                            .dst_w = 300, .dst_h = 200, .alpha = 1, .dim = 1, .color = 0x3366cc},
                           image(1, 0, 0, W, 124, W, 124)};
    struct mg_node f3[] = {image(2, 10.5f, 130.3f, 1000, 2000, W, 2216),
                           image(3, 0, 0, W, H, 400, 120)};
    f3[1].flags = MG_NODE_SMOOTH;
    struct mg_node f4[] = {image(2, 0, 124, W, 2216, W, 2216)};
    f4[0].radius = 40; /* rounded: not an occluder */
    struct {
        const char *name;
        const struct texture *base;
        struct mg_node *n;
        uint32_t count;
    } frames[] = {
        {"mirror, app, HUD", NULL, f1, 3},
        {"mirror, app, popup, fill at fractions, mirror strip", NULL, f2, 5},
        {"app scaled at fractions, HUD scaled over all", NULL, f3, 2},
        {"base under a rounded app", &tex[0], f4, 1},
        {"base alone", &tex[0], f4, 0},
    };
    int bad = 0;
    for (size_t i = 0; i < sizeof frames / sizeof *frames; i++) {
        unsigned char *a = draw(&r, frames[i].base, frames[i].n, frames[i].count, 0);
        uint64_t all = shaded;
        unsigned char *b = draw(&r, frames[i].base, frames[i].n, frames[i].count, 1);
        size_t diff = 0;
        for (size_t k = 0; k < (size_t)W * H * 4; k++)
            diff += a[k] != b[k];
        printf("%s: %s (%zu bytes differ; %.2f MP shaded, %.2f without culling)\n",
               diff ? "FAIL" : "ok  ", frames[i].name, diff, shaded / 1e6, all / 1e6);
        bad += diff != 0;
        free(a);
        free(b);
    }
    return bad;
}
