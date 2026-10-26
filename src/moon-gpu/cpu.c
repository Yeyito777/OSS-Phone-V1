#include "cpu.h"

#include <math.h>
#include <stdint.h>
#include <pthread.h>
#include <string.h>

#define BAND 16
#define MAX_THREADS 8

/* ---- pixels: 0xAARRGGBB premultiplied ---- */

static inline uint32_t mul8(uint32_t c, uint32_t a) { /* c * a / 255 per channel */
    uint32_t rb = (c & 0x00ff00ff) * a + 0x00800080;
    rb = ((rb + ((rb >> 8) & 0x00ff00ff)) >> 8) & 0x00ff00ff;
    uint32_t ag = ((c >> 8) & 0x00ff00ff) * a + 0x00800080;
    ag = (ag + ((ag >> 8) & 0x00ff00ff)) & 0xff00ff00;
    return rb | ag;
}

static inline uint32_t over(uint32_t src, uint32_t dst) {
    return src + mul8(dst, 255 - (src >> 24));
}

/* Bilinear sample at 16.16 fixed-point source coordinates (pixel centres at
 * +0.5), clamped to the image edge. */
static inline uint32_t bilinear(const struct cpu_image *im, int32_t u, int32_t v) {
    u -= 0x8000;
    v -= 0x8000;
    int x0 = u >> 16, y0 = v >> 16;
    uint32_t fx = (uint32_t)(u >> 8) & 0xff, fy = (uint32_t)(v >> 8) & 0xff;
    int x1 = x0 + 1, y1 = y0 + 1;
    if (x0 < 0)
        x0 = x1 = 0, fx = 0;
    if (y0 < 0)
        y0 = y1 = 0, fy = 0;
    if (x1 >= im->width)
        x1 = im->width - 1, x0 = x0 > x1 ? x1 : x0;
    if (y1 >= im->height)
        y1 = im->height - 1, y0 = y0 > y1 ? y1 : y0;
    const uint32_t *r0 = (const uint32_t *)(im->px + (size_t)y0 * im->stride);
    const uint32_t *r1 = (const uint32_t *)(im->px + (size_t)y1 * im->stride);
    uint32_t a = mul8(r0[x0], 255 - fx) + mul8(r0[x1], fx);
    uint32_t b = mul8(r1[x0], 255 - fx) + mul8(r1[x1], fx);
    return mul8(a, 255 - fy) + mul8(b, fy);
}

static inline uint32_t nearest(const struct cpu_image *im, int32_t u, int32_t v) {
    int x = u >> 16, y = v >> 16;
    x = x < 0 ? 0 : x >= im->width ? im->width - 1 : x;
    y = y < 0 ? 0 : y >= im->height ? im->height - 1 : y;
    return ((const uint32_t *)(im->px + (size_t)y * im->stride))[x];
}

/* Coverage (0..255) of a rounded rectangle of size w x h and radius r at
 * local pixel centre (lx, ly). */
static inline uint32_t corner_coverage(float lx, float ly, float w, float h, float r) {
    float cx = lx < r ? r - lx : lx > w - r ? lx - (w - r) : 0;
    float cy = ly < r ? r - ly : ly > h - r ? ly - (h - r) : 0;
    if (cx <= 0 || cy <= 0)
        return 255;
    float d = sqrtf(cx * cx + cy * cy) - r; /* > 0 outside */
    float c = 0.5f - d;
    return c <= 0 ? 0 : c >= 1 ? 255 : (uint32_t)(c * 255.0f + 0.5f);
}

/* ---- one row of a node ---- */

static void node_row(uint32_t *row, int width, int y, const struct mg_node *n,
                     const struct cpu_image *im) {
    float dy0 = n->dst_y, dy1 = n->dst_y + n->dst_h;
    if (y + 0.5f < dy0 || y + 0.5f >= dy1)
        return;
    int x0 = (int)ceilf(n->dst_x - 0.5f), x1 = (int)ceilf(n->dst_x + n->dst_w - 0.5f);
    if (x0 < 0)
        x0 = 0;
    if (x1 > width)
        x1 = width;
    if (x1 <= x0)
        return;
    uint32_t alpha = (uint32_t)(n->alpha * 255.0f + 0.5f);
    if (alpha > 255)
        alpha = 255;
    if (!alpha)
        return;
    float ly = y + 0.5f - n->dst_y;
    int rounded = n->radius > 0.5f &&
                  (ly < n->radius || ly > n->dst_h - n->radius);
    if (n->kind == MG_NODE_FILL) {
        uint32_t c = 0xff000000u | (n->color & 0xffffff);
        for (int x = x0; x < x1; x++) {
            uint32_t a = alpha;
            if (rounded)
                a = a * corner_coverage(x + 0.5f - n->dst_x, ly, n->dst_w, n->dst_h,
                                        n->radius) / 255;
            row[x] = a == 255 ? c : over(mul8(c, a), row[x]);
        }
        return;
    }
    if (!im)
        return;
    float scale_x = n->src_w / n->dst_w, scale_y = n->src_h / n->dst_h;
    int32_t v = (int32_t)((n->src_y + ly * scale_y) * 65536.0f);
    int32_t u0 = (int32_t)((n->src_x + (x0 + 0.5f - n->dst_x) * scale_x) * 65536.0f);
    int32_t du = (int32_t)(scale_x * 65536.0f);
    int smooth = (n->flags & MG_NODE_SMOOTH) &&
                 (fabsf(scale_x - 1) > 0.001f || fabsf(scale_y - 1) > 0.001f);
    uint32_t dim = (uint32_t)(n->dim * 255.0f + 0.5f);
    if (dim > 255)
        dim = 255;
    uint32_t opaque_mask = im->opaque ? 0xff000000u : 0;
    /* 1:1 opaque rows: a copy (with dim). */
    if (!smooth && !rounded && alpha == 255 && im->opaque && du == 0x10000 &&
        n->radius <= 0.5f) {
        int sy = v >> 16, sx = u0 >> 16;
        if (sy >= 0 && sy < im->height && sx >= 0 && sx + (x1 - x0) <= im->width) {
            const uint32_t *s = (const uint32_t *)(im->px + (size_t)sy * im->stride) + sx;
            if (dim == 255)
                for (int x = x0; x < x1; x++)
                    row[x] = s[x - x0] | 0xff000000u;
            else
                for (int x = x0; x < x1; x++)
                    row[x] = mul8(s[x - x0] & 0xffffff, dim) | 0xff000000u;
            return;
        }
    }
    int32_t u = u0;
    for (int x = x0; x < x1; x++, u += du) {
        uint32_t c = (smooth ? bilinear(im, u, v) : nearest(im, u, v)) | opaque_mask;
        if (dim != 255)
            c = (mul8(c & 0xffffff, dim) & 0xffffff) | (c & 0xff000000u);
        uint32_t a = alpha;
        if (rounded)
            a = a * corner_coverage(x + 0.5f - n->dst_x, ly, n->dst_w, n->dst_h,
                                    n->radius) / 255;
        if (a != 255)
            c = mul8(c, a);
        row[x] = (c >> 24) == 255 ? c : over(c, row[x]);
    }
}

/* ---- the worker pool ---- */

struct job {
    const struct cpu_target *t;
    const struct cpu_image *base;
    const struct mg_node *nodes;
    uint32_t count;
    cpu_lookup lookup;
    void *ctx;
    /* cpu_copy */
    const struct cpu_image *from;
    float sx, sy, sw, sh, radius;
    int copy;
};

static int nthreads = 1;
static pthread_t workers[MAX_THREADS];
static pthread_barrier_t go, done;
static struct job current;

static void copy_rows(const struct job *j, int y0, int y1) {
    const struct cpu_target *t = j->t;
    struct mg_node n = {
        .kind = MG_NODE_IMAGE,
        .src_x = j->sx, .src_y = j->sy, .src_w = j->sw, .src_h = j->sh,
        .dst_w = (float)t->width, .dst_h = (float)t->height,
        .alpha = 1, .dim = 1, .radius = j->radius,
        .flags = MG_NODE_SMOOTH,
    };
    for (int y = y0; y < y1; y++) {
        uint32_t *row = (uint32_t *)(t->px + (size_t)y * t->stride);
        memset(row, 0, (size_t)t->width * 4);
        node_row(row, t->width, y, &n, j->from);
    }
}

static void render_rows(const struct job *j, int y0, int y1) {
    const struct cpu_target *t = j->t;
    for (int y = y0; y < y1; y++) {
        uint32_t *row = (uint32_t *)(t->px + (size_t)y * t->stride);
        if (j->base && y < j->base->height) {
            const uint8_t *s = j->base->px + (size_t)y * j->base->stride;
            int w = j->base->width < t->width ? j->base->width : t->width;
            memcpy(row, s, (size_t)w * 4);
            for (int x = w; x < t->width; x++)
                row[x] = 0xff000000u;
        } else {
            for (int x = 0; x < t->width; x++)
                row[x] = 0xff000000u;
        }
        for (uint32_t i = 0; i < j->count; i++) {
            const struct mg_node *n = &j->nodes[i];
            node_row(row, t->width, y, n,
                     n->kind == MG_NODE_IMAGE ? j->lookup(j->ctx, n->surface) : NULL);
        }
    }
}

/* Interleaved bands keep the threads' shares even whatever the scene. */
static void run_share(int id) {
    const struct job *j = &current;
    int h = j->t->height;
    for (int y = id * BAND; y < h; y += nthreads * BAND) {
        int y1 = y + BAND < h ? y + BAND : h;
        if (j->copy)
            copy_rows(j, y, y1);
        else
            render_rows(j, y, y1);
    }
}

static void *worker(void *arg) {
    int id = (int)(intptr_t)arg;
    for (;;) {
        pthread_barrier_wait(&go);
        run_share(id);
        pthread_barrier_wait(&done);
    }
    return NULL;
}

void cpu_init(int threads) {
    nthreads = threads < 1 ? 1 : threads > MAX_THREADS ? MAX_THREADS : threads;
    pthread_barrier_init(&go, NULL, (unsigned)nthreads);
    pthread_barrier_init(&done, NULL, (unsigned)nthreads);
    for (int i = 1; i < nthreads; i++)
        pthread_create(&workers[i], NULL, worker, (void *)(intptr_t)i);
}

static void run(void) {
    pthread_barrier_wait(&go);
    run_share(0);
    pthread_barrier_wait(&done);
}

void cpu_render(const struct cpu_target *t, const struct cpu_image *base,
                const struct mg_node *nodes, uint32_t count, cpu_lookup lookup,
                void *ctx) {
    current = (struct job){.t = t, .base = base, .nodes = nodes, .count = count,
                           .lookup = lookup, .ctx = ctx};
    run();
}

void cpu_copy(const struct cpu_target *to, const struct cpu_image *from,
              float sx, float sy, float sw, float sh, float radius) {
    current = (struct job){.t = to, .from = from, .sx = sx, .sy = sy, .sw = sw,
                           .sh = sh, .radius = radius, .copy = 1};
    run();
}
