/* moon-gpu-poc: GPU-composited, vblank-paced frames on the A26 panel.
 *
 * Renders a Moon-like app-switcher scene (dimming full-screen wallpaper plus
 * scrolling rounded app cards with shadows: about 5 Mpx of fragments and a
 * full 2.53 Mpx frame every refresh) with the Mali GPU into dmabufs, and
 * page-flips them with atomic KMS, one frame per vblank. Reports the flip
 * intervals and GPU times measured from kernel timestamps.
 *
 *   moon-gpu-poc [--offscreen] [--seconds S] [--hz 120] [--scene switcher|zoom]
 *                [--heap system-uncached] [--brightness N] [--csv FILE]
 *                [--active-cycles N] [--hold S] [--dump FILE.ppm]
 *                [--offscreen [--internal] [--pipeline]] [--gpu-min KHZ]
 *
 * Scenes: switcher (default), zoom, and for cost breakdowns bg (the fading
 * wallpaper alone) and noshadow (switcher without card shadows).
 *
 * --active-cycles turns the CRTC off and on N times afterwards (as DPMS
 * does); --hold drops DRM master and keeps the last frame on screen for S
 * seconds, so another master can take the display over in place.
 *
 * KMS mode needs DRM master: nothing else (Xorg) may hold /dev/dri/card0.
 */
#define _GNU_SOURCE
#include "gpu.h"
#include "kms.h"

#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <drm/drm_fourcc.h>

#define NBUF 3

static volatile sig_atomic_t stop;
static void on_signal(int s) {
    (void)s;
    stop = 1;
}

static uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000000000ull + ts.tv_nsec;
}

/* ---- content: CPU-drawn once, like Moon's layers and app snapshots ---- */

static uint32_t hash(uint32_t x) {
    x ^= x >> 16;
    x *= 0x7feb352d;
    x ^= x >> 15;
    x *= 0x846ca68b;
    return x ^ (x >> 16);
}

static uint8_t clamp8(float v) {
    return v < 0 ? 0 : v > 255 ? 255 : (uint8_t)v;
}

/* RGBA bytes (GL_RGBA order). */
static uint8_t *make_wallpaper(int w, int h) {
    uint8_t *p = malloc((size_t)w * h * 4);
    for (int y = 0; y < h; y++)
        for (int x = 0; x < w; x++) {
            float fy = (float)y / h, fx = (float)x / w;
            float g1 = expf(-((fx - .25f) * (fx - .25f) + (fy - .3f) * (fy - .3f)) * 9);
            float g2 = expf(-((fx - .8f) * (fx - .8f) + (fy - .65f) * (fy - .65f)) * 7);
            float n = (hash((uint32_t)(y * w + x)) & 255) / 255.f - .5f;
            uint8_t *q = p + ((size_t)y * w + x) * 4;
            q[0] = clamp8(10 + 40 * fy + 90 * g2 + n * 2);
            q[1] = clamp8(14 + 20 * fy + 60 * g1 + n * 2);
            q[2] = clamp8(40 + 60 * (1 - fy) + 140 * g1 + 80 * g2 + n * 2);
            q[3] = 255;
        }
    return p;
}

/* An "app screenshot": header, rows of list cells, text-like dashes. */
static uint8_t *make_app(int w, int h, int seed) {
    uint8_t *p = malloc((size_t)w * h * 4);
    uint8_t base[3] = {(uint8_t)(20 + seed * 40), 24, (uint8_t)(30 + seed * 20)};
    uint8_t accent[3] = {(uint8_t)(seed == 1 ? 255 : 10),
                         (uint8_t)(seed == 2 ? 209 : 132),
                         (uint8_t)(seed == 0 ? 255 : 60)};
    for (int y = 0; y < h; y++) {
        int row = (y - 300) / 150, in_row = (y - 300) % 150;
        for (int x = 0; x < w; x++) {
            uint8_t *q = p + ((size_t)y * w + x) * 4;
            uint8_t r = base[0], g = base[1], b = base[2];
            if (y < 300) {
                r = accent[0] / 2;
                g = accent[1] / 2;
                b = accent[2] / 2;
                if (y > 170 && y < 230 && x > 60 && x < 60 + 400 + seed * 80)
                    r = g = b = 240;
            } else if (in_row > 10 && in_row < 140 && x > 40 && x < w - 40) {
                r = g = b = 44;
                int tx = x - 160;
                uint32_t word = hash((uint32_t)(row * 977 + seed * 31 + tx / 90));
                if (x > 60 && x < 140 && in_row > 35 && in_row < 115) {
                    r = accent[0];
                    g = accent[1];
                    b = accent[2];
                } else if (tx > 0 && in_row > 52 && in_row < 92 &&
                           (tx % 90) < 60 + (int)(word % 25) && tx < 600 + (int)(word % 300)) {
                    r = g = b = 210;
                }
            }
            q[0] = r;
            q[1] = g;
            q[2] = b;
            q[3] = 255;
        }
    }
    return p;
}

static GLuint upload(const uint8_t *rgba, int w, int h) {
    GLuint t;
    glGenTextures(1, &t);
    glBindTexture(GL_TEXTURE_2D, t);
    glTexStorage2D(GL_TEXTURE_2D, 1, GL_RGBA8, w, h);
    glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, w, h, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
    glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    return t;
}

/* ---- the compositor's two programs ---- */

static const char *VS =
    "#version 300 es\n"
    "layout(location=0) in vec2 a_pos;\n"
    "uniform vec4 u_dst; uniform vec4 u_src; uniform vec2 u_screen;\n"
    "out vec2 v_uv; out vec2 v_local;\n"
    "void main() {\n"
    "  vec2 p = u_dst.xy + a_pos * u_dst.zw;\n"
    "  v_uv = mix(u_src.xy, u_src.zw, a_pos);\n"
    "  v_local = a_pos * u_dst.zw;\n"
    "  gl_Position = vec4(p / u_screen * 2.0 - 1.0, 0.0, 1.0);\n"
    "}\n";

static const char *FS_IMAGE =
    "#version 300 es\n"
    "precision highp float;\n"
    "uniform sampler2D u_tex; uniform float u_alpha; uniform float u_dim;\n"
    "uniform float u_radius; uniform vec4 u_dst;\n"
    "in vec2 v_uv; in vec2 v_local; out vec4 o;\n"
    "float rbox(vec2 p, vec2 b, float r) {\n"
    "  vec2 q = abs(p) - b + r;\n"
    "  return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;\n"
    "}\n"
    "void main() {\n"
    "  vec4 c = texture(u_tex, v_uv);\n"
    "  float a = u_alpha;\n"
    "  if (u_radius > 0.0)\n"
    "    a *= clamp(0.5 - rbox(v_local - u_dst.zw * 0.5, u_dst.zw * 0.5, u_radius), 0.0, 1.0);\n"
    "  o = vec4(c.rgb * u_dim * a, a);\n"
    "}\n";

/* A soft shadow of a rounded rect inset by u_blur within the quad. */
static const char *FS_SHADOW =
    "#version 300 es\n"
    "precision highp float;\n"
    "uniform float u_alpha; uniform float u_radius; uniform float u_blur;\n"
    "uniform vec4 u_dst;\n"
    "in vec2 v_uv; in vec2 v_local; out vec4 o;\n"
    "float rbox(vec2 p, vec2 b, float r) {\n"
    "  vec2 q = abs(p) - b + r;\n"
    "  return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;\n"
    "}\n"
    "void main() {\n"
    "  vec2 half_ = u_dst.zw * 0.5 - vec2(u_blur);\n"
    "  float d = rbox(v_local - u_dst.zw * 0.5, half_, u_radius);\n"
    "  float a = u_alpha * (1.0 - smoothstep(-u_blur, u_blur, d));\n"
    "  o = vec4(0.0, 0.0, 0.0, a);\n"
    "}\n";

struct prog {
    GLuint id;
    GLint dst, src, screen, tex, alpha, dim, radius, blur;
};

static void prog_init(struct prog *p, GLuint id) {
    p->id = id;
    p->dst = glGetUniformLocation(id, "u_dst");
    p->src = glGetUniformLocation(id, "u_src");
    p->screen = glGetUniformLocation(id, "u_screen");
    p->tex = glGetUniformLocation(id, "u_tex");
    p->alpha = glGetUniformLocation(id, "u_alpha");
    p->dim = glGetUniformLocation(id, "u_dim");
    p->radius = glGetUniformLocation(id, "u_radius");
    p->blur = glGetUniformLocation(id, "u_blur");
}

struct scene {
    int w, h;
    struct prog image, shadow;
    GLuint wallpaper, apps[3];
    GLuint vbo, vao;
    const char *name;
    uint64_t fragments; /* last frame's shaded pixels (estimate from quads) */
};

static void quad_image(struct scene *s, GLuint tex, float x, float y, float w,
                       float h, float alpha, float dim, float radius) {
    glUseProgram(s->image.id);
    glUniform4f(s->image.dst, x, y, w, h);
    glUniform4f(s->image.src, 0, 0, 1, 1);
    glUniform2f(s->image.screen, (float)s->w, (float)s->h);
    glUniform1f(s->image.alpha, alpha);
    glUniform1f(s->image.dim, dim);
    glUniform1f(s->image.radius, radius);
    glBindTexture(GL_TEXTURE_2D, tex);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    float cx0 = fmaxf(x, 0), cy0 = fmaxf(y, 0);
    float cx1 = fminf(x + w, (float)s->w), cy1 = fminf(y + h, (float)s->h);
    if (cx1 > cx0 && cy1 > cy0)
        s->fragments += (uint64_t)((cx1 - cx0) * (cy1 - cy0));
}

static void quad_shadow(struct scene *s, float x, float y, float w, float h,
                        float alpha, float radius, float blur) {
    glUseProgram(s->shadow.id);
    glUniform4f(s->shadow.dst, x - blur, y - blur + blur * .5f, w + 2 * blur,
                h + 2 * blur);
    glUniform2f(s->shadow.screen, (float)s->w, (float)s->h);
    glUniform1f(s->shadow.alpha, alpha);
    glUniform1f(s->shadow.radius, radius);
    glUniform1f(s->shadow.blur, blur);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    s->fragments += (uint64_t)((w + 2 * blur) * (h + 2 * blur));
}

static int scene_init(struct scene *s, int w, int h, const char *name) {
    memset(s, 0, sizeof *s);
    s->w = w;
    s->h = h;
    s->name = name;
    GLuint pi = gpu_program(VS, FS_IMAGE), ps = gpu_program(VS, FS_SHADOW);
    if (!pi || !ps)
        return -1;
    prog_init(&s->image, pi);
    prog_init(&s->shadow, ps);
    glUseProgram(pi);
    glUniform1i(s->image.tex, 0);
    uint64_t t0 = now_ns();
    uint8_t *px = make_wallpaper(w, h);
    s->wallpaper = upload(px, w, h);
    free(px);
    for (int i = 0; i < 3; i++) {
        px = make_app(w, h, i);
        s->apps[i] = upload(px, w, h);
        free(px);
    }
    glFinish();
    fprintf(stderr, "poc: content drawn and uploaded in %.1f ms\n",
            (now_ns() - t0) / 1e6);
    static const float quad[] = {0, 0, 1, 0, 0, 1, 1, 1};
    glGenVertexArrays(1, &s->vao);
    glBindVertexArray(s->vao);
    glGenBuffers(1, &s->vbo);
    glBindBuffer(GL_ARRAY_BUFFER, s->vbo);
    glBufferData(GL_ARRAY_BUFFER, sizeof quad, quad, GL_STATIC_DRAW);
    glEnableVertexAttribArray(0);
    glVertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 0, 0);
    return 0;
}

static float ease(float t) { /* smoothstep */
    return t * t * (3 - 2 * t);
}

/* Draws the scene at time t (seconds). */
static void scene_draw(struct scene *s, double t) {
    float W = (float)s->w, H = (float)s->h;
    s->fragments = 0;
    glViewport(0, 0, s->w, s->h);
    glDisable(GL_BLEND);
    if (!strcmp(s->name, "zoom")) {
        /* Launch zoom: an app card grows from a dock icon to full screen and
         * back over a dimming home screen, 0.4 s each way. */
        double ph = fmod(t, 1.2);
        float k = ph < .4 ? ease((float)(ph / .4)) : ph < .6 ? 1.f
                  : ph < 1.0 ? 1 - ease((float)((ph - .6) / .4)) : 0.f;
        quad_image(s, s->wallpaper, 0, 0, W, H, 1, 1 - .45f * k, 0);
        glEnable(GL_BLEND);
        glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
        float ix = 160, iy = H - 330, is = 170;
        float x = ix + (0 - ix) * k, y = iy + (0 - iy) * k;
        float w = is + (W - is) * k, h = is + (H - is) * k;
        quad_image(s, s->apps[0], x, y, w, h, 1, 1, 40 * (1 - k) + 6);
        return;
    }
    /* App switcher: the wallpaper fades between full and 45 % brightness
     * (Moon's background fade), and three cards at 70 % scale scroll
     * sideways with shadows and rounded corners. */
    float dim = .725f + .275f * (float)cos(t * 2.2);
    quad_image(s, s->wallpaper, 0, 0, W, H, 1, dim, 0);
    if (!strcmp(s->name, "bg"))
        return;
    glEnable(GL_BLEND);
    glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA);
    float cw = W * .70f, ch = H * .70f, gap = W * .06f;
    float pitch = cw + gap, span = pitch * 3;
    float off = (float)fmod(t * 700.0, span);
    float y = (H - ch) * .45f;
    for (int i = 0; i < 4; i++) {
        float x = (W - cw) * .5f + i * pitch - off;
        if (x > W || x + cw < 0)
            continue;
        if (strcmp(s->name, "noshadow"))
            quad_shadow(s, x, y, cw, ch, .55f, 48, 36);
        quad_image(s, s->apps[i % 3], x, y, cw, ch, 1, 1, 48);
    }
}

/* ---- statistics ---- */

static int cmp_u64(const void *a, const void *b) {
    uint64_t x = *(const uint64_t *)a, y = *(const uint64_t *)b;
    return x < y ? -1 : x > y;
}

static void summary(const char *what, uint64_t *v, size_t n) {
    if (!n) {
        printf("%s: no samples\n", what);
        return;
    }
    uint64_t *c = malloc(n * sizeof *c);
    memcpy(c, v, n * sizeof *c);
    qsort(c, n, sizeof *c, cmp_u64);
    double sum = 0;
    for (size_t i = 0; i < n; i++)
        sum += c[i];
    printf("%s: n=%zu mean=%.3f p50=%.3f p95=%.3f p99=%.3f max=%.3f ms\n",
           what, n, sum / n / 1e6, c[n / 2] / 1e6, c[n * 95 / 100] / 1e6,
           c[n * 99 / 100] / 1e6, c[n - 1] / 1e6);
    free(c);
}

struct run {
    struct kms *k;
    struct gpu *g;
    struct scene *s;
    uint32_t *fb;
    struct gpu_target *tgt;
    uint64_t period, start;
    FILE *csv;
    uint64_t *interval, *gpu_ns, *cpu_ns, *ready_ns, *frags;
    uint32_t *seq_delta;
    size_t cap;
    int cur, hz;
    uint64_t busy_retries;
};

/* Flips one GPU frame per vblank for `seconds`, then prints statistics. */
static void run_flips(struct run *c, double seconds, const char *label) {
    struct kms *k = c->k;
    size_t n = 0;
    int failures = 0;
    uint64_t begin = now_ns(), end = begin + (uint64_t)(seconds * 1e9);
    c->busy_retries = 0;
    uint64_t last_flip = begin;
    uint32_t last_seq = 0;
    int have_flip = 0;
    for (uint64_t f = 1; !stop && now_ns() < end && n < c->cap; f++) {
        /* Render for the vblank this frame will reach: the one after the
         * last flip. */
        uint64_t t0 = now_ns();
        uint64_t target = last_flip + c->period;
        while (target < t0)
            target += c->period;
        glBindFramebuffer(GL_FRAMEBUFFER, c->tgt[c->cur].fbo);
        scene_draw(c->s, (target - c->start) / 1e9);
        int fence = gpu_fence(c->g);
        uint64_t t1 = now_ns();
        /* The flip event of the previous frame completes its commit's
         * flip_done, but EBUSY can still come back while the driver's commit
         * work finishes; retry briefly with the same frame. */
        int r, tries = 0;
        while ((r = kms_commit(k, c->fb[c->cur], fence, 0, f)) == -EBUSY &&
               tries++ < 40)
            usleep(250);
        if (tries)
            c->busy_retries += (uint64_t)tries;
        if (r) {
            fprintf(stderr, "poc: commit %llu: %s\n", (unsigned long long)f,
                    strerror(-r));
            if (fence >= 0)
                close(fence);
            if (++failures > 10)
                break;
            continue;
        }
        struct kms_flip ev;
        int got;
        /* Only this frame's event counts. */
        while ((got = kms_wait_flip(k, 200, &ev)) > 0 && ev.user_data != f)
            ;
        if (got <= 0) {
            fprintf(stderr, "poc: no flip event (%d)\n", got);
            if (fence >= 0)
                close(fence);
            break;
        }
        uint64_t gdone = fence >= 0 ? gpu_fence_time(fence) : t1;
        if (fence >= 0)
            close(fence);
        if (have_flip) {
            c->interval[n] = ev.time_ns - last_flip;
            c->seq_delta[n] = ev.sequence - last_seq;
            c->cpu_ns[n] = t1 - t0;
            c->gpu_ns[n] = gdone > t0 ? gdone - t0 : 0;
            /* How far into the refresh the GPU finished, from the vblank
             * that started this frame. */
            c->ready_ns[n] = gdone > last_flip ? gdone - last_flip : 0;
            c->frags[n] = c->s->fragments;
            if (c->csv)
                fprintf(c->csv, "%s,%zu,%llu,%u,%llu,%llu,%llu,%llu,%llu\n",
                        label, n, (unsigned long long)ev.time_ns, ev.sequence,
                        (unsigned long long)c->interval[n],
                        (unsigned long long)c->cpu_ns[n],
                        (unsigned long long)c->gpu_ns[n],
                        (unsigned long long)c->ready_ns[n],
                        (unsigned long long)c->frags[n]);
            n++;
        }
        have_flip = 1;
        last_flip = ev.time_ns;
        last_seq = ev.sequence;
        c->cur = (c->cur + 1) % NBUF;
    }
    uint64_t missed = 0, late = 0;
    double fr = 0;
    for (size_t i = 0; i < n; i++) {
        if (c->seq_delta[i] > 1)
            missed += c->seq_delta[i] - 1;
        if (c->interval[i] > c->period * 3 / 2)
            late++;
        fr += c->frags[i];
    }
    printf("[%s] %s %ux%u@%d, %zu flips in %.2f s\n", label, c->s->name,
           k->mode.hdisplay, k->mode.vdisplay, c->hz, n,
           (now_ns() - begin) / 1e9);
    summary("flip interval", c->interval, n);
    summary("cpu submit", c->cpu_ns, n);
    summary("gpu frame (submit start to fence signal)", c->gpu_ns, n);
    summary("vblank to gpu done", c->ready_ns, n);
    printf("missed vblanks: %llu (intervals > 1.5 periods: %llu), EBUSY retries %llu\n",
           (unsigned long long)missed, (unsigned long long)late,
           (unsigned long long)c->busy_retries);
    printf("fragments per frame: %.2f Mpx (output %.2f Mpx)\n",
           n ? fr / n / 1e6 : 0, k->mode.hdisplay * k->mode.vdisplay / 1e6);
    fflush(stdout);
}

/* ---- main ---- */

int main(int argc, char **argv) {
    int offscreen = 0, hz = 120, brightness = -1, active_cycles = 0,
        hold_seconds = 0, internal = 0, pipeline = 0;
    const char *dump_path = NULL;
    int gpu_min_khz = 0;
    double seconds = 10;
    const char *heap = "system-uncached", *scene_name = "switcher",
               *csv_path = NULL, *card = "/dev/dri/card0";
    for (int i = 1; i < argc; i++) {
        const char *a = argv[i], *v = i + 1 < argc ? argv[i + 1] : NULL;
        if (!strcmp(a, "--offscreen"))
            offscreen = 1;
        else if (!strcmp(a, "--seconds") && v)
            seconds = atof(argv[++i]);
        else if (!strcmp(a, "--hz") && v)
            hz = atoi(argv[++i]);
        else if (!strcmp(a, "--heap") && v)
            heap = argv[++i];
        else if (!strcmp(a, "--scene") && v)
            scene_name = argv[++i];
        else if (!strcmp(a, "--brightness") && v)
            brightness = atoi(argv[++i]);
        else if (!strcmp(a, "--csv") && v)
            csv_path = argv[++i];
        else if (!strcmp(a, "--active-cycles") && v)
            active_cycles = atoi(argv[++i]);
        else if (!strcmp(a, "--hold") && v)
            hold_seconds = atoi(argv[++i]);
        else if (!strcmp(a, "--gpu-min") && v)
            gpu_min_khz = atoi(argv[++i]);
        else if (!strcmp(a, "--dump") && v)
            dump_path = argv[++i];
        else if (!strcmp(a, "--internal"))
            internal = 1;
        else if (!strcmp(a, "--pipeline"))
            pipeline = 1;
        else if (!strcmp(a, "--card") && v)
            card = argv[++i];
        else {
            fprintf(stderr, "usage: see the comment at the top of poc.c\n");
            return 2;
        }
    }
    signal(SIGINT, on_signal);
    signal(SIGTERM, on_signal);

    struct kms k = {.fd = -1};
    uint32_t W = 1080, H = 2340;
    if (!offscreen) {
        if (kms_open(&k, card, hz))
            return 1;
        W = k.mode.hdisplay;
        H = k.mode.vdisplay;
    }
    struct gpu g;
    if (gpu_init(&g))
        return 1;

    uint32_t pitch = (W * 4 + 63) & ~63u;
    struct gpu_target tgt[NBUF];
    uint32_t fb[NBUF] = {0};
    for (int i = 0; i < NBUF; i++) {
        int fd = gpu_dmabuf_alloc(heap, (size_t)pitch * H);
        if (fd < 0) {
            fprintf(stderr, "poc: dma_heap %s: %s\n", heap, strerror(-fd));
            return 1;
        }
        if (gpu_target_init(&g, &tgt[i], fd, W, H, pitch, DRM_FORMAT_ABGR8888))
            return 1;
        if (!offscreen &&
            kms_add_fb(&k, fd, W, H, pitch, DRM_FORMAT_ABGR8888, &fb[i]))
            return 1;
    }
    fprintf(stderr, "poc: %d %ux%u dmabufs from %s, pitch %u\n", NBUF, W, H,
            heap, pitch);

    struct scene s;
    if (scene_init(&s, (int)W, (int)H, scene_name))
        return 1;

    /* A GPU clock floor while animating (Samsung's DVFS otherwise settles
     * low for a frame-paced load); restored at exit. */
    char old_min[32] = "";
    const char *minf = "/sys/kernel/gpu/gpu_min_clock";
    if (gpu_min_khz > 0) {
        int fd = open(minf, O_RDWR);
        if (fd >= 0) {
            ssize_t n = read(fd, old_min, sizeof old_min - 1);
            old_min[n > 0 ? n : 0] = 0;
            dprintf(fd, "%d\n", gpu_min_khz);
            close(fd);
            fprintf(stderr, "poc: gpu min clock %d kHz (was %s)\n", gpu_min_khz, old_min);
        }
    }
    char old_brightness[32] = "";
    const char *bl = "/sys/class/backlight/panel/brightness";
    if (brightness >= 0) {
        int fd = open(bl, O_RDWR);
        if (fd >= 0) {
            ssize_t n = read(fd, old_brightness, sizeof old_brightness - 1);
            old_brightness[n > 0 ? n : 0] = 0;
            dprintf(fd, "%d\n", brightness);
            close(fd);
        }
    }

    size_t cap = (size_t)(seconds * (offscreen ? 2000 : hz + 10)) + 64, n = 0;
    uint64_t *interval = calloc(cap, 8), *gpu_ns = calloc(cap, 8),
             *cpu_ns = calloc(cap, 8), *ready_ns = calloc(cap, 8);
    uint32_t *seq_delta = calloc(cap, 4);
    uint64_t *frags = calloc(cap, 8);
    FILE *csv = csv_path ? fopen(csv_path, "w") : NULL;
    if (csv)
        fprintf(csv, "phase,frame,flip_ns,seq,interval_ns,cpu_ns,gpu_ns,"
                     "vblank_to_gpu_done_ns,fragments\n");

    uint64_t period = 1000000000ull / hz;
    uint64_t start = now_ns(), end = start + (uint64_t)(seconds * 1e9);
    int cur = 0;

    if (offscreen) {
        /* --internal renders into GL-allocated textures (the driver picks
         * its own layout, e.g. AFBC) instead of the linear dmabufs. */
        GLuint itex[NBUF], ifbo[NBUF];
        if (internal) {
            glGenTextures(NBUF, itex);
            glGenFramebuffers(NBUF, ifbo);
            for (int i = 0; i < NBUF; i++) {
                glBindTexture(GL_TEXTURE_2D, itex[i]);
                glTexStorage2D(GL_TEXTURE_2D, 1, GL_RGBA8, (GLsizei)W, (GLsizei)H);
                glBindFramebuffer(GL_FRAMEBUFFER, ifbo[i]);
                glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
                                       GL_TEXTURE_2D, itex[i], 0);
            }
        }
        /* Back-to-back frames. Each is waited on (pure GPU latency per
         * frame), or with --pipeline the fence two frames back is (GPU
         * throughput, the GPU never idles). */
        int pend[NBUF] = {-1, -1, -1};
        uint64_t pend_t0[NBUF] = {0};
        while (!stop && now_ns() < end && n < cap) {
            uint64_t t0 = now_ns();
            glBindFramebuffer(GL_FRAMEBUFFER, internal ? ifbo[cur] : tgt[cur].fbo);
            scene_draw(&s, (t0 - start) / 1e9);
            uint64_t t1 = now_ns();
            int fence = gpu_fence(&g);
            int wait_slot = pipeline ? (cur + 1) % NBUF : cur;
            pend[cur] = fence;
            pend_t0[cur] = t0;
            int wf = pend[wait_slot];
            if (wf >= 0) {
                struct pollfd p = {.fd = wf, .events = POLLIN};
                poll(&p, 1, 1000);
                uint64_t done = gpu_fence_time(wf);
                close(wf);
                pend[wait_slot] = -1;
                cpu_ns[n] = t1 - t0;
                gpu_ns[n] = done > pend_t0[wait_slot] ? done - pend_t0[wait_slot] : 0;
                frags[n] = s.fragments;
                n++;
            }
            cur = (cur + 1) % NBUF;
        }
        double secs = (now_ns() - start) / 1e9;
        printf("throughput: %.1f frames/s\n", n / secs);
        printf("offscreen %s %ux%u, %zu frames\n", scene_name, W, H, n);
        summary("cpu submit", cpu_ns, n);
        summary("gpu frame (submit start to fence signal)", gpu_ns, n);
        double fr = 0;
        for (size_t i = 0; i < n; i++)
            fr += frags[i];
        printf("fragments per frame: %.2f Mpx (output %.2f Mpx)\n",
               n ? fr / n / 1e6 : 0, W * H / 1e6);
        goto out;
    }

    struct run ctx = {.k = &k, .g = &g, .s = &s, .fb = fb, .tgt = tgt,
                      .period = period, .start = start, .csv = csv,
                      .interval = interval, .gpu_ns = gpu_ns, .cpu_ns = cpu_ns,
                      .ready_ns = ready_ns, .seq_delta = seq_delta,
                      .frags = frags, .cap = cap, .hz = hz};

    /* First frame: blocking commit, a modeset if the CRTC is off. */
    glBindFramebuffer(GL_FRAMEBUFFER, tgt[0].fbo);
    scene_draw(&s, 0);
    glFinish();
    uint64_t t_first = now_ns();
    int r = k.crtc_active ? kms_commit_blocking(&k, fb[0])
                          : kms_commit(&k, fb[0], -1, 1, 0);
    if (r) {
        fprintf(stderr, "poc: first commit (modeset=%d): %s\n", !k.crtc_active,
                strerror(-r));
        goto out;
    }
    fprintf(stderr, "poc: first frame on screen (commit %.1f ms)\n",
            (now_ns() - t_first) / 1e6);
    ctx.cur = 1;
    run_flips(&ctx, seconds, "kms");

    for (int c = 0; c < active_cycles && !stop; c++) {
        /* DPMS-like: CRTC off and on again, keeping our mode and buffers. */
        uint64_t t0 = now_ns();
        r = kms_set_active(&k, 0, 0);
        uint64_t t1 = now_ns();
        printf("active-cycle %d: off %s in %.1f ms\n", c, r ? strerror(-r) : "ok",
               (t1 - t0) / 1e6);
        usleep(1000000);
        t0 = now_ns();
        r = kms_set_active(&k, 1, fb[ctx.cur]);
        t1 = now_ns();
        printf("active-cycle %d: on %s in %.1f ms\n", c, r ? strerror(-r) : "ok",
               (t1 - t0) / 1e6);
        if (r)
            break;
        run_flips(&ctx, 2, "after on");
    }

    if (dump_path) {
        /* The buffer on screen now (the last flipped), as a PPM. */
        int last = (ctx.cur + NBUF - 1) % NBUF;
        uint8_t *m = mmap(NULL, (size_t)tgt[last].pitch * H, PROT_READ,
                          MAP_SHARED, tgt[last].fd, 0);
        FILE *f = fopen(dump_path, "wb");
        if (m != MAP_FAILED && f) {
            fprintf(f, "P6 %u %u 255\n", W, H);
            uint8_t *row = malloc(W * 3);
            for (uint32_t y = 0; y < H; y++) {
                const uint8_t *src = m + (size_t)y * tgt[last].pitch;
                for (uint32_t x = 0; x < W; x++)
                    memcpy(row + x * 3, src + x * 4, 3);
                fwrite(row, 1, W * 3, f);
            }
            free(row);
            printf("dump: scanout buffer %d written to %s\n", last, dump_path);
        }
        if (f)
            fclose(f);
        if (m != MAP_FAILED)
            munmap(m, (size_t)tgt[last].pitch * H);
    }

    if (hold_seconds > 0) {
        /* Keep the card (and so the framebuffer on screen) open without
         * master: another process (Xorg) may become master meanwhile. */
        int dr = ioctl(k.fd, DRM_IOCTL_DROP_MASTER, NULL);
        printf("hold: dropped master (%s), keeping the last frame for %d s\n",
               dr ? strerror(errno) : "ok", hold_seconds);
        fflush(stdout);
        for (int i = 0; i < hold_seconds * 10 && !stop; i++)
            usleep(100000);
    }

out:
    if (csv)
        fclose(csv);
    if (gpu_min_khz > 0 && old_min[0]) {
        int fd = open(minf, O_WRONLY);
        if (fd >= 0) {
            write(fd, old_min, strlen(old_min));
            close(fd);
        }
    }
    if (brightness >= 0 && old_brightness[0]) {
        int fd = open(bl, O_WRONLY);
        if (fd >= 0) {
            write(fd, old_brightness, strlen(old_brightness));
            close(fd);
        }
    }
    for (int i = 0; i < NBUF; i++)
        gpu_target_fini(&g, &tgt[i]);
    gpu_fini(&g);
    /* Closing the card removes our framebuffers; the kernel then turns the
     * CRTC off. */
    kms_close(&k);
    return 0;
}
