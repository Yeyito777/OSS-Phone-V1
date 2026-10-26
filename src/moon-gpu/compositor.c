/* moon-compositor: Moon's display compositor.
 *
 * The only DRM master on the phone in GPU mode. Moon (in the Alpine chroot)
 * connects over a Unix SOCK_SEQPACKET socket (proto.h), shares its CPU-drawn
 * layers as dmabufs (or memfds) and describes each frame as a base surface
 * plus nodes; this process shows them one frame per vblank with atomic KMS
 * and reports each flip back. Every frame takes the cheapest path that can
 * show it (notes/gpu/DESIGN.md):
 *
 *   1. planes: the base and each node on its own display plane (DPP), with
 *      plane alpha and scaling; nothing is drawn;
 *   2. GPU: the Mali composites the frame into a dmabuf (GLES);
 *   3. CPU: without a usable GPU, threads composite it into cached memory.
 *
 * It also owns the "X mirror", a cached dmabuf that Xorg (on its headless
 * dummy driver) copies the visible root into on Moon's request; it can be
 * scanned out directly or sampled as a texture.
 *
 *   moon-compositor --socket PATH [--card /dev/dri/card0] [--hz 120]
 *                   [--ready-file PATH] [--gpu-floor KHZ] [--cpu]
 *                   [--no-planes] [--threads N]
 *
 * MOON_GPU_PROFILE=1 logs every flip. SIGUSR1 writes the current frame,
 * drawn by the GPU or CPU tier whatever path shows it, to shot.ppm next to
 * the socket. Exits on SIGTERM.
 */
#define _GNU_SOURCE
#include "cpu.h"
#include "gpu.h"
#include "kms.h"
#include "proto.h"
#include "render.h"

#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>
#include <drm/drm_fourcc.h>
#include <linux/dma-buf.h>
#include <GLES2/gl2ext.h>

#define NBUF 3
#define MAX_FDS 4
/* A flip that has not completed after this long is presumed lost. */
#define FLIP_TIMEOUT_NS 250000000ull

static volatile sig_atomic_t quit, shot;
static void on_signal(int s) {
    if (s == SIGUSR1)
        shot = 1;
    else
        quit = 1;
}

static uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000000000ull + ts.tv_nsec;
}

static void logf_(const char *fmt, ...) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    fprintf(stderr, "[%5lld.%03ld] ", (long long)ts.tv_sec, ts.tv_nsec / 1000000);
    va_list ap;
    va_start(ap, fmt);
    vfprintf(stderr, fmt, ap);
    va_end(ap);
    fputc('\n', stderr);
}
#define LOG(...) logf_(__VA_ARGS__)

enum tier { TIER_GPU, TIER_CPU };

/* A texture/image the compositor can draw or scan out. */
struct surface {
    uint32_t id;
    int width, height, stride;
    uint32_t fourcc;
    int opaque;
    int fd;          /* dmabuf, or -1 */
    uint8_t *map;    /* CPU view: a memfd, or a cached dmabuf */
    size_t map_len;
    int memfd;       /* GPU texture is a copy of map, refreshed on damage */
    struct texture tex;
    EGLImageKHR image;
    GLuint fbo;      /* GPU copies are drawn through this */
    uint32_t fb;     /* KMS framebuffer, 0 if it cannot be scanned out */
    float baked_radius;
    struct cpu_image cpu;
};

struct frame {
    uint32_t serial, flags, base, count;
    struct mg_node *nodes;
    uint32_t cap;
};

/* An output buffer for the GPU and CPU paths. */
struct output {
    int fd;
    uint8_t *map; /* CPU tier */
    uint32_t fb;
    struct gpu_target gpu;
};

struct comp {
    struct kms k;
    struct gpu g;
    struct renderer r;
    enum tier tier;
    int planes;
    struct output out[NBUF];
    int next_buf;
    int width, height, pitch;

    struct surface *surf;
    size_t nsurf, capsurf;
    struct surface mirror;

    struct frame cur, pending;
    int have_pending, cur_valid, dirty;

    /* the commit waiting for its flip */
    int flip_pending;
    uint32_t flight_serial, flight_path;
    uint64_t flight_tag;
    int flight_fence;
    uint64_t flight_t0, flight_since, flight_render_ns;
    uint64_t tags;

    int powered;
    int listen_fd, client_fd;
    /* GPU clock floor while frames flow (Samsung's DVFS settles too low for
     * a frame-paced load); 0 disables. */
    int floor_khz, floor_fd, floored;
    char floor_restore[32];
    uint64_t last_submit_ns;
    int profile;
    uint64_t last_flip_ns;
    uint32_t last_seq;
    uint64_t path_counts[4];
};

static void dmabuf_sync(int fd, uint64_t flags) {
    struct dma_buf_sync s = {.flags = flags};
    while (ioctl(fd, DMA_BUF_IOCTL_SYNC, &s) < 0 && (errno == EINTR || errno == EAGAIN))
        ;
}

/* After the CPU wrote a cached buffer: clean its cache lines for the GPU and
 * the display. */
static void flush_for_device(int fd) {
    dmabuf_sync(fd, DMA_BUF_SYNC_START | DMA_BUF_SYNC_RW);
    dmabuf_sync(fd, DMA_BUF_SYNC_END | DMA_BUF_SYNC_RW);
}

/* ---- surfaces ---- */

static struct surface *find_surface(struct comp *c, uint32_t id) {
    if (id == MG_MIRROR_ID)
        return c->mirror.id ? &c->mirror : NULL;
    for (size_t i = 0; i < c->nsurf; i++)
        if (c->surf[i].id == id)
            return &c->surf[i];
    return NULL;
}

static const struct texture *lookup_tex(void *ctx, uint32_t id) {
    struct surface *s = find_surface(ctx, id);
    return s && s->tex.id ? &s->tex : NULL;
}

static const struct cpu_image *lookup_cpu(void *ctx, uint32_t id) {
    struct surface *s = find_surface(ctx, id);
    return s && s->cpu.px ? &s->cpu : NULL;
}

static void release(struct comp *c, struct surface *s) {
    if (s->fbo)
        glDeleteFramebuffers(1, &s->fbo);
    if (s->tex.id)
        glDeleteTextures(1, &s->tex.id);
    if (s->image)
        c->g.destroy_image(c->g.dpy, s->image);
    if (s->fb)
        kms_rm_fb(&c->k, s->fb);
    if (s->map)
        munmap(s->map, s->map_len);
    if (s->fd >= 0)
        close(s->fd);
    memset(s, 0, sizeof *s);
    s->fd = -1;
}

static void free_surface(struct comp *c, struct surface *s) {
    release(c, s);
    *s = c->surf[--c->nsurf];
}

static struct surface *new_surface(struct comp *c, uint32_t id) {
    struct surface *old = find_surface(c, id);
    if (old)
        free_surface(c, old);
    if (c->nsurf == c->capsurf) {
        size_t cap = c->capsurf ? c->capsurf * 2 : 32;
        struct surface *n = realloc(c->surf, cap * sizeof *n);
        if (!n)
            return NULL;
        c->surf = n;
        c->capsurf = cap;
    }
    struct surface *s = &c->surf[c->nsurf++];
    memset(s, 0, sizeof *s);
    s->fd = -1;
    s->id = id;
    return s;
}

static void free_all_surfaces(struct comp *c) {
    while (c->nsurf)
        free_surface(c, &c->surf[0]);
}

static int frame_uses(const struct frame *f, uint32_t id) {
    if ((f->flags & MG_FRAME_HAS_BASE) && f->base == id)
        return 1;
    for (uint32_t i = 0; i < f->count; i++)
        if (f->nodes[i].kind == MG_NODE_IMAGE && f->nodes[i].surface == id)
            return 1;
    return 0;
}

static void upload_rects(struct surface *s, const struct mg_rect *rects,
                         uint32_t count) {
    glBindTexture(GL_TEXTURE_2D, s->tex.id);
    glPixelStorei(GL_UNPACK_ALIGNMENT, 4);
    glPixelStorei(GL_UNPACK_ROW_LENGTH, s->stride / 4);
    for (uint32_t i = 0; i < count; i++) {
        int x0 = rects[i].x0 < 0 ? 0 : rects[i].x0;
        int y0 = rects[i].y0 < 0 ? 0 : rects[i].y0;
        int x1 = rects[i].x1 > s->width ? s->width : rects[i].x1;
        int y1 = rects[i].y1 > s->height ? s->height : rects[i].y1;
        if (x1 <= x0 || y1 <= y0)
            continue;
        glTexSubImage2D(GL_TEXTURE_2D, 0, x0, y0, x1 - x0, y1 - y0, GL_RGBA,
                        GL_UNSIGNED_BYTE,
                        s->map + (size_t)y0 * s->stride + (size_t)x0 * 4);
    }
    glPixelStorei(GL_UNPACK_ROW_LENGTH, 0);
}

/* Sets up a dmabuf-backed surface: CPU view, GPU texture, scanout. */
static int adopt_dmabuf(struct comp *c, struct surface *s, int fd, int width,
                        int height, int stride, uint32_t fourcc, int opaque,
                        int need_map) {
    s->fd = fd;
    s->width = width;
    s->height = height;
    s->stride = stride;
    s->fourcc = fourcc;
    s->opaque = opaque;
    s->map_len = (size_t)stride * height;
    if (need_map) {
        s->map = mmap(NULL, s->map_len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
        if (s->map == MAP_FAILED)
            s->map = mmap(NULL, s->map_len, PROT_READ, MAP_SHARED, fd, 0);
        if (s->map == MAP_FAILED) {
            s->map = NULL;
            return -1;
        }
        s->cpu = (struct cpu_image){s->map, width, height, stride, opaque};
    }
    if (c->tier == TIER_GPU) {
        s->tex.target = gpu_import_texture(&c->g, fd, (uint32_t)width,
                                           (uint32_t)height, (uint32_t)stride,
                                           fourcc, &s->image, &s->tex.id);
        if (!s->tex.target)
            return -1;
        s->tex.width = width;
        s->tex.height = height;
        s->tex.opaque = opaque;
    }
    if (kms_add_fb(&c->k, fd, (uint32_t)width, (uint32_t)height,
                   (uint32_t)stride, fourcc, &s->fb))
        s->fb = 0; /* drawable, just not on a plane */
    return 0;
}

/* ---- the X mirror ---- */

static int mirror_init(struct comp *c) {
    struct surface *m = &c->mirror;
    memset(m, 0, sizeof *m);
    m->fd = -1;
    int stride = (c->width * 4 + 63) & ~63;
    /* Cached: Xorg writes it with the CPU (fast), the compositor reads it in
     * the CPU tier, and cleans the caches on every damage for the GPU and
     * the display. */
    int fd = gpu_dmabuf_alloc("system", (size_t)stride * c->height);
    if (fd < 0) {
        LOG("mirror: dma_heap system: %s", strerror(-fd));
        return -1;
    }
    if (adopt_dmabuf(c, m, fd, c->width, c->height, stride, DRM_FORMAT_XRGB8888,
                     1, 1)) {
        LOG("mirror: the dmabuf cannot be used");
        return -1;
    }
    memset(m->map, 0, m->map_len);
    flush_for_device(fd);
    m->id = MG_MIRROR_ID;
    LOG("mirror: %dx%d stride %d%s%s", c->width, c->height, stride,
        m->tex.id ? ", GPU texture" : "", m->fb ? ", scanout" : "");
    return 0;
}

/* ---- snapshots ---- */

static int make_copy(struct comp *c, const struct mg_surface_copy *m) {
    struct surface *from = find_surface(c, m->source);
    if (!from || !m->id || m->id == MG_MIRROR_ID || !m->width || !m->height ||
        m->width > 8192 || m->height > 8192)
        return -1;
    int width = (int)m->width, height = (int)m->height;
    int stride = (width * 4 + 63) & ~63;
    int rounded = m->radius > 0.5f;
    /* GPU tier: drawn by the GPU, read by the GPU and the display only.
     * CPU tier: drawn and read by the CPU. */
    int fd = gpu_dmabuf_alloc(c->tier == TIER_GPU ? "system-uncached" : "system",
                              (size_t)stride * height);
    if (fd < 0)
        return -1;
    /* Keep the source: `from` may move when the table grows. */
    struct surface src = *from;
    struct surface *s = new_surface(c, m->id);
    if (!s) {
        close(fd);
        return -1;
    }
    int opaque = src.opaque && !rounded;
    uint32_t fourcc = c->tier == TIER_GPU
                          ? (opaque ? DRM_FORMAT_XBGR8888 : DRM_FORMAT_ABGR8888)
                          : (opaque ? DRM_FORMAT_XRGB8888 : DRM_FORMAT_ARGB8888);
    if (adopt_dmabuf(c, s, fd, width, height, stride, fourcc, opaque,
                     c->tier == TIER_CPU)) {
        free_surface(c, s);
        return -1;
    }
    s->baked_radius = rounded ? m->radius : 0;
    if (c->tier == TIER_GPU) {
        glGenFramebuffers(1, &s->fbo);
        glBindFramebuffer(GL_FRAMEBUFFER, s->fbo);
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, s->tex.target,
                               s->tex.id, 0);
        if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
            LOG("copy %u: not renderable", m->id);
            free_surface(c, s);
            return -1;
        }
        render_copy(&c->r, &src.tex, m->src_x, m->src_y, m->src_w, m->src_h,
                    width, height, s->baked_radius);
        /* Planes and later frames read it: finish now (copies are rare). */
        glFinish();
    } else {
        struct cpu_target t = {s->map, width, height, stride};
        cpu_copy(&t, &src.cpu, m->src_x, m->src_y, m->src_w, m->src_h,
                 s->baked_radius);
        flush_for_device(s->fd);
    }
    return 0;
}

/* ---- client messages ---- */

static void send_msg(struct comp *c, uint16_t type, const void *body,
                     size_t len, int fd) {
    if (c->client_fd < 0)
        return;
    char buf[256];
    struct mg_header h = {.type = type, .length = (uint32_t)(sizeof h + len)};
    if (sizeof h + len > sizeof buf)
        return;
    memcpy(buf, &h, sizeof h);
    memcpy(buf + sizeof h, body, len);
    struct iovec iov = {.iov_base = buf, .iov_len = sizeof h + len};
    union {
        char b[CMSG_SPACE(sizeof(int))];
        struct cmsghdr align;
    } u;
    struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1};
    if (fd >= 0) {
        m.msg_control = u.b;
        m.msg_controllen = sizeof u.b;
        struct cmsghdr *cm = CMSG_FIRSTHDR(&m);
        cm->cmsg_level = SOL_SOCKET;
        cm->cmsg_type = SCM_RIGHTS;
        cm->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(cm), &fd, sizeof fd);
    }
    if (sendmsg(c->client_fd, &m, MSG_DONTWAIT | MSG_NOSIGNAL) < 0 &&
        errno != EAGAIN)
        LOG("send %u: %s", type, strerror(errno));
}

static void drop_client(struct comp *c, const char *why) {
    if (c->client_fd < 0)
        return;
    LOG("client gone (%s); keeping the last frame", why);
    close(c->client_fd);
    c->client_fd = -1;
    /* The last frame stays on screen (its surfaces too, until a new client
     * says hello). A new client starts with a fresh frame. */
    c->have_pending = 0;
    c->cur_valid = 0;
}

static int frame_copy(struct frame *dst, const struct mg_frame *f,
                      const struct mg_node *nodes) {
    if (f->count > dst->cap) {
        struct mg_node *n = realloc(dst->nodes, f->count * sizeof *n);
        if (!n)
            return -1;
        dst->nodes = n;
        dst->cap = f->count;
    }
    dst->serial = f->serial;
    dst->flags = f->flags;
    dst->base = f->base;
    dst->count = f->count;
    memcpy(dst->nodes, nodes, f->count * sizeof *nodes);
    return 0;
}

static void power(struct comp *c, int on);
static void wait_flip(struct comp *c);

static int surface_format(uint32_t format, uint32_t *fourcc, int *opaque) {
    if (format == MG_XRGB8888) {
        *fourcc = DRM_FORMAT_XRGB8888;
        *opaque = 1;
    } else if (format == MG_ARGB8888) {
        *fourcc = DRM_FORMAT_ARGB8888;
        *opaque = 0;
    } else {
        return -1;
    }
    return 0;
}

static void handle(struct comp *c, const char *p, size_t len, int *fds,
                   int nfds) {
    const struct mg_header *h = (const void *)p;
    const char *body = p + sizeof *h;
    size_t blen = len - sizeof *h;
#define NEED(T)                                                                \
    if (blen < sizeof(T)) {                                                    \
        LOG("short message %u", h->type);                                      \
        return;                                                                \
    }
    switch (h->type) {
    case MG_HELLO: {
        NEED(struct mg_hello);
        const struct mg_hello *m = (const void *)body;
        if (m->version != MG_PROTO_VERSION) {
            LOG("client speaks version %u, not %u", m->version, MG_PROTO_VERSION);
            drop_client(c, "version");
            return;
        }
        /* A new client brings its own surfaces; the old ones may still be
         * on a plane until the flip in flight completes. */
        wait_flip(c);
        free_all_surfaces(c);
        uint64_t mhz = (uint64_t)c->k.mode.clock * 1000000ull /
                       ((uint64_t)c->k.mode.htotal * c->k.mode.vtotal);
        struct mg_welcome w = {
            .version = MG_PROTO_VERSION,
            .width = (uint32_t)c->width,
            .height = (uint32_t)c->height,
            .refresh_mhz = (uint32_t)mhz,
            .period_ns = mhz ? 1000000000000ull / mhz : 8333333,
            .caps = (c->tier == TIER_GPU ? MG_CAP_GPU : 0) |
                    (c->planes ? MG_CAP_PLANES : 0),
        };
        send_msg(c, MG_WELCOME, &w, sizeof w, -1);
        struct mg_surface_shm mi = {.width = (uint32_t)c->width,
                                    .height = (uint32_t)c->height,
                                    .stride = (uint32_t)c->mirror.stride,
                                    .format = MG_XRGB8888,
                                    .flags = MG_SURFACE_DMABUF};
        send_msg(c, MG_MIRROR, &mi, sizeof mi, c->mirror.fd);
        LOG("client hello");
        break;
    }
    case MG_SURFACE_SHM: {
        NEED(struct mg_surface_shm);
        const struct mg_surface_shm *m = (const void *)body;
        uint32_t fourcc;
        int opaque;
        if (nfds < 1 || !m->id || m->id == MG_MIRROR_ID || !m->width ||
            !m->height || m->width > 8192 || m->height > 8192 ||
            m->stride < m->width * 4 || surface_format(m->format, &fourcc, &opaque)) {
            LOG("bad surface %u", m->id);
            break;
        }
        struct surface *s = new_surface(c, m->id);
        if (!s)
            break;
        if (m->flags & MG_SURFACE_DMABUF) {
            int fd = dup(fds[0]);
            if (adopt_dmabuf(c, s, fd, (int)m->width, (int)m->height,
                             (int)m->stride, fourcc, opaque, c->tier == TIER_CPU)) {
                LOG("surface %u: dmabuf import failed", m->id);
                free_surface(c, s);
            }
            break;
        }
        /* A memfd: a texture copied from it, or read in place by the CPU. */
        s->width = (int)m->width;
        s->height = (int)m->height;
        s->stride = (int)m->stride;
        s->fourcc = fourcc;
        s->opaque = opaque;
        s->memfd = 1;
        s->map_len = (size_t)m->stride * m->height;
        s->map = mmap(NULL, s->map_len, PROT_READ, MAP_SHARED, fds[0], 0);
        if (s->map == MAP_FAILED) {
            LOG("surface %u: mmap: %s", m->id, strerror(errno));
            s->map = NULL;
            free_surface(c, s);
            break;
        }
        s->cpu = (struct cpu_image){s->map, s->width, s->height, s->stride, opaque};
        if (c->tier == TIER_GPU) {
            s->tex = (struct texture){.target = GL_TEXTURE_2D, .width = s->width,
                                      .height = s->height, .swizzle = 1,
                                      .opaque = opaque};
            glGenTextures(1, &s->tex.id);
            glBindTexture(GL_TEXTURE_2D, s->tex.id);
            glTexStorage2D(GL_TEXTURE_2D, 1, GL_RGBA8, s->width, s->height);
            struct mg_rect all = {0, 0, s->width, s->height};
            upload_rects(s, &all, 1);
        }
        break;
    }
    case MG_SURFACE_DAMAGE: {
        NEED(struct mg_damage);
        const struct mg_damage *m = (const void *)body;
        if (blen < sizeof *m + (size_t)m->count * sizeof(struct mg_rect))
            break;
        struct surface *s = find_surface(c, m->id);
        if (s && s->memfd && c->tier == TIER_GPU)
            upload_rects(s, (const void *)(m + 1), m->count);
        if (s && c->cur_valid && frame_uses(&c->cur, m->id))
            c->dirty = 1;
        struct mg_uploaded u = {.id = m->id, .serial = m->serial};
        send_msg(c, MG_UPLOADED, &u, sizeof u, -1);
        break;
    }
    case MG_SURFACE_COPY: {
        NEED(struct mg_surface_copy);
        if (make_copy(c, (const void *)body))
            LOG("copy %u failed", ((const struct mg_surface_copy *)body)->id);
        break;
    }
    case MG_SURFACE_FREE: {
        NEED(struct mg_id);
        struct surface *s = find_surface(c, ((const struct mg_id *)body)->id);
        if (s && s != &c->mirror) {
            /* A plane may still scan it out until the flip in flight. */
            if (c->flip_pending && frame_uses(&c->cur, s->id))
                wait_flip(c);
            s = find_surface(c, ((const struct mg_id *)body)->id);
            if (s)
                free_surface(c, s);
        }
        break;
    }
    case MG_MIRROR_DAMAGE:
        /* Xorg wrote it with the CPU: clean the caches for the GPU and the
         * display before either reads it. */
        flush_for_device(c->mirror.fd);
        if (c->cur_valid && frame_uses(&c->cur, MG_MIRROR_ID))
            c->dirty = 1;
        break;
    case MG_FRAME: {
        NEED(struct mg_frame);
        const struct mg_frame *m = (const void *)body;
        if (blen < sizeof *m + (size_t)m->count * sizeof(struct mg_node)) {
            LOG("frame %u truncated", m->serial);
            break;
        }
        if (!frame_copy(&c->pending, m, (const void *)(m + 1)))
            c->have_pending = 1;
        break;
    }
    case MG_POWER: {
        NEED(struct mg_power);
        power(c, ((const struct mg_power *)body)->on != 0);
        break;
    }
    default:
        LOG("unknown message %u", h->type);
    }
#undef NEED
}

static void read_client(struct comp *c) {
    static char buf[MG_MAX_PACKET];
    for (;;) {
        union {
            char b[CMSG_SPACE(MAX_FDS * sizeof(int))];
            struct cmsghdr align;
        } u;
        struct iovec iov = {.iov_base = buf, .iov_len = sizeof buf};
        struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1,
                           .msg_control = u.b, .msg_controllen = sizeof u.b};
        ssize_t n = recvmsg(c->client_fd, &m, MSG_DONTWAIT | MSG_CMSG_CLOEXEC);
        if (n < 0 && (errno == EAGAIN || errno == EINTR))
            return;
        if (n <= 0) {
            drop_client(c, n ? strerror(errno) : "closed");
            return;
        }
        int fds[MAX_FDS], nfds = 0;
        for (struct cmsghdr *cm = CMSG_FIRSTHDR(&m); cm; cm = CMSG_NXTHDR(&m, cm))
            if (cm->cmsg_level == SOL_SOCKET && cm->cmsg_type == SCM_RIGHTS) {
                int k = (int)((cm->cmsg_len - CMSG_LEN(0)) / sizeof(int));
                for (int i = 0; i < k && nfds < MAX_FDS; i++)
                    memcpy(&fds[nfds++], CMSG_DATA(cm) + i * sizeof(int), sizeof(int));
            }
        const struct mg_header *h = (const void *)buf;
        if ((m.msg_flags & MSG_TRUNC) || n < (ssize_t)sizeof *h ||
            h->length != (uint32_t)n)
            LOG("malformed packet (%zd bytes)", n);
        else
            handle(c, buf, (size_t)n, fds, nfds);
        for (int i = 0; i < nfds; i++)
            close(fds[i]);
        if (c->client_fd < 0)
            return;
    }
}

/* ---- tier 1: display planes ---- */

/* Clips a node's destination to the screen, moving its source window with
 * it. Returns 0 if nothing is visible. */
static int clip_layer(struct comp *c, struct kms_layer *l, float sx, float sy,
                      float sw, float sh, float dx, float dy, float dw, float dh) {
    float x0 = roundf(dx), y0 = roundf(dy), x1 = roundf(dx + dw), y1 = roundf(dy + dh);
    float kx = sw / dw, ky = sh / dh;
    if (x0 < 0) {
        sx += -x0 * kx;
        x0 = 0;
    }
    if (y0 < 0) {
        sy += -y0 * ky;
        y0 = 0;
    }
    if (x1 > c->width)
        x1 = (float)c->width;
    if (y1 > c->height)
        y1 = (float)c->height;
    if (x1 <= x0 || y1 <= y0)
        return 0;
    l->sx = sx;
    l->sy = sy;
    l->sw = (x1 - x0) * kx;
    l->sh = (y1 - y0) * ky;
    l->dx = (int32_t)x0;
    l->dy = (int32_t)y0;
    l->dw = (int32_t)(x1 - x0);
    l->dh = (int32_t)(y1 - y0);
    return 1;
}

/* Maps the current frame onto planes, bottom first. Returns the layer
 * count, or -1 if some part needs drawing. */
static int plan_planes(struct comp *c, struct kms_layer *layers) {
    const struct frame *f = &c->cur;
    int n = 0, scaled = 0;
    if (f->flags & MG_FRAME_HAS_BASE) {
        struct surface *s = find_surface(c, f->base);
        if (!s || !s->fb || !s->opaque || s->width < c->width || s->height < c->height)
            return -1;
        layers[n++] = (struct kms_layer){.fb = s->fb, .sw = (float)c->width,
                                         .sh = (float)c->height, .dw = c->width,
                                         .dh = c->height, .alpha = 0xffff,
                                         .in_fence = -1};
    }
    for (uint32_t i = 0; i < f->count; i++) {
        const struct mg_node *nd = &f->nodes[i];
        if (nd->alpha <= 0.002f || nd->dst_w <= 0 || nd->dst_h <= 0)
            continue;
        if (nd->kind == MG_NODE_FILL) {
            /* Black on the black background at the very bottom: nothing. */
            if (n == 0 && (nd->color & 0xffffff) == 0)
                continue;
            return -1;
        }
        struct surface *s = find_surface(c, nd->surface);
        if (!s || !s->fb || n == KMS_MAX_PLANES || nd->src_w <= 0 || nd->src_h <= 0)
            return -1;
        float kx = nd->dst_w / nd->src_w, ky = nd->dst_h / nd->src_h;
        /* Rounded corners only where the buffer has them, at this scale. */
        if (fabsf(nd->radius - s->baked_radius * kx) > 1.5f)
            return -1;
        struct kms_layer *l = &layers[n];
        if (!clip_layer(c, l, nd->src_x, nd->src_y, nd->src_w, nd->src_h,
                        nd->dst_x, nd->dst_y, nd->dst_w, nd->dst_h))
            continue;
        /* DPP limits: 2x down to 8x up, at least 64x16 on screen. */
        int scaling = fabsf(kx - 1) > 0.002f || fabsf(ky - 1) > 0.002f;
        if (scaling && (kx < 0.5f || ky < 0.5f || kx > 8 || ky > 8 ||
                        ++scaled > c->k.nscalers))
            return -1;
        if (l->dw < 64 || l->dh < 16)
            return -1;
        float a = nd->alpha;
        /* A dim is plane alpha over black, so only at the very bottom. */
        if (nd->dim < 0.998f) {
            if (n != 0 || !s->opaque)
                return -1;
            a *= nd->dim;
        }
        l->fb = s->fb;
        l->alpha = (uint16_t)lroundf(fminf(a, 1.0f) * 65535.0f);
        l->premultiplied = !s->opaque;
        l->in_fence = -1;
        n++;
    }
    return n;
}

/* ---- frames ---- */

static void set_floor(struct comp *c, int on) {
    if (c->floor_fd < 0 || on == c->floored)
        return;
    char v[32];
    int n = on ? snprintf(v, sizeof v, "%d\n", c->floor_khz)
               : snprintf(v, sizeof v, "%s", c->floor_restore);
    if (pwrite(c->floor_fd, v, (size_t)n, 0) < 0)
        LOG("gpu clock floor: %s", strerror(errno));
    c->floored = on;
}

/* Draws the current frame into output `buf` (GPU or CPU). Returns the fence
 * fd of the GPU work, or -1. */
static int draw(struct comp *c, int buf) {
    const struct frame *f = &c->cur;
    if (c->tier == TIER_GPU) {
        set_floor(c, 1);
        const struct texture *base =
            (f->flags & MG_FRAME_HAS_BASE) ? lookup_tex(c, f->base) : NULL;
        glBindFramebuffer(GL_FRAMEBUFFER, c->out[buf].gpu.fbo);
        render_frame(&c->r, base, f->nodes, f->count, lookup_tex, c);
        return gpu_fence(&c->g);
    }
    const struct cpu_image *base =
        (f->flags & MG_FRAME_HAS_BASE) ? lookup_cpu(c, f->base) : NULL;
    struct cpu_target t = {c->out[buf].map, c->width, c->height, c->pitch};
    dmabuf_sync(c->out[buf].fd, DMA_BUF_SYNC_START | DMA_BUF_SYNC_WRITE);
    cpu_render(&t, base, f->nodes, f->count, lookup_cpu, c);
    dmabuf_sync(c->out[buf].fd, DMA_BUF_SYNC_END | DMA_BUF_SYNC_WRITE);
    return -1;
}

static int commit_retry(struct comp *c, struct kms_layer *layers, int n,
                        uint64_t tag) {
    /* EBUSY can come back for a moment after the previous flip's event
     * while the driver's commit work finishes; retry with the same frame. */
    int r, tries = 0;
    while ((r = kms_commit_layers(&c->k, layers, n, KMS_FLIP, tag)) == -EBUSY &&
           tries++ < 40)
        usleep(250);
    return r;
}

/* Shows the current frame at the next vblank, by the cheapest path. */
static void submit(struct comp *c, uint32_t serial) {
    uint64_t t0 = now_ns();
    uint64_t tag = ++c->tags;
    struct kms_layer layers[KMS_MAX_PLANES];
    int path = MG_PATH_PLANES, fence = -1, r = -1;
    int n = c->planes ? plan_planes(c, layers) : -1;
    if (n > 0 && !kms_assign(&c->k, layers, n) &&
        !kms_commit_layers(&c->k, layers, n, KMS_TEST, 0))
        r = commit_retry(c, layers, n, tag);
    if (r) {
        path = c->tier == TIER_GPU ? MG_PATH_GPU : MG_PATH_CPU;
        int buf = c->next_buf;
        fence = draw(c, buf);
        struct kms_layer l = {.fb = c->out[buf].fb, .sw = (float)c->width,
                              .sh = (float)c->height, .dw = c->width,
                              .dh = c->height, .alpha = 0xffff,
                              .in_fence = fence, .plane = c->k.primary};
        r = commit_retry(c, &l, 1, tag);
        if (!r)
            c->next_buf = (buf + 1) % NBUF;
    }
    if (r) {
        LOG("commit: %s", strerror(-r));
        if (fence >= 0)
            close(fence);
        return;
    }
    c->flip_pending = 1;
    c->flight_serial = serial;
    c->flight_path = (uint32_t)path;
    c->flight_tag = tag;
    c->flight_fence = fence;
    c->flight_t0 = t0;
    c->flight_render_ns = path == MG_PATH_CPU ? now_ns() - t0 : 0;
    c->flight_since = now_ns();
    c->last_submit_ns = c->flight_since;
    c->dirty = 0;
}

static void maybe_submit(struct comp *c) {
    if (c->flip_pending || !c->powered)
        return;
    if (c->have_pending) {
        struct frame t = c->cur;
        c->cur = c->pending;
        c->pending = t;
        c->have_pending = 0;
        c->cur_valid = 1;
        submit(c, c->cur.serial);
    } else if (c->dirty && c->cur_valid) {
        submit(c, 0);
    }
}

static void flip_done(struct comp *c, const struct kms_flip *ev) {
    uint64_t done = c->flight_fence >= 0 ? gpu_fence_time(c->flight_fence) : 0;
    struct mg_presented p = {
        .serial = c->flight_serial,
        .sequence = ev->sequence,
        .time_ns = ev->time_ns,
        .render_ns = done > c->flight_t0 ? done - c->flight_t0 : c->flight_render_ns,
        .path = c->flight_path,
    };
    c->path_counts[c->flight_path & 3]++;
    if (c->profile)
        LOG("flip serial %u seq %u interval %.3f ms (vblanks %u) %s %.3f ms",
            p.serial, ev->sequence,
            c->last_flip_ns ? (ev->time_ns - c->last_flip_ns) / 1e6 : 0.0,
            ev->sequence - c->last_seq,
            p.path == MG_PATH_PLANES ? "planes" : p.path == MG_PATH_GPU ? "gpu" : "cpu",
            p.render_ns / 1e6);
    c->last_flip_ns = ev->time_ns;
    c->last_seq = ev->sequence;
    if (c->flight_fence >= 0)
        close(c->flight_fence);
    c->flight_fence = -1;
    c->flip_pending = 0;
    if (p.serial)
        send_msg(c, MG_PRESENTED, &p, sizeof p, -1);
}

static void read_flips(struct comp *c, int timeout_ms) {
    struct kms_flip ev;
    while (kms_wait_flip(&c->k, timeout_ms, &ev) > 0) {
        if (c->flip_pending && ev.user_data == c->flight_tag)
            flip_done(c, &ev);
        timeout_ms = 0;
    }
}

static void wait_flip(struct comp *c) {
    for (int i = 0; i < 4 && c->flip_pending; i++)
        read_flips(c, 50);
    c->flip_pending = 0;
}

static void power(struct comp *c, int on) {
    struct mg_power reply = {.on = (uint32_t)on};
    if (on == c->powered) {
        send_msg(c, MG_POWERED, &reply, sizeof reply, -1);
        return;
    }
    wait_flip(c);
    uint64_t t0 = now_ns();
    int r;
    if (on) {
        /* Light the panel with the newest frame, drawn while dark. */
        if (c->have_pending) {
            struct frame t = c->cur;
            c->cur = c->pending;
            c->pending = t;
            c->have_pending = 0;
            c->cur_valid = 1;
        }
        int buf = c->next_buf;
        if (c->cur_valid) {
            int fence = draw(c, buf);
            if (fence >= 0) {
                struct pollfd p = {.fd = fence, .events = POLLIN};
                poll(&p, 1, 100);
                close(fence);
            }
        } else if (c->tier == TIER_GPU) {
            glBindFramebuffer(GL_FRAMEBUFFER, c->out[buf].gpu.fbo);
            glClearColor(0, 0, 0, 1);
            glClear(GL_COLOR_BUFFER_BIT);
            glFinish();
        } else {
            memset(c->out[buf].map, 0, (size_t)c->pitch * c->height);
            flush_for_device(c->out[buf].fd);
        }
        r = kms_set_active(&c->k, 1, c->out[buf].fb);
        c->next_buf = (buf + 1) % NBUF;
    } else {
        r = kms_set_active(&c->k, 0, 0);
        set_floor(c, 0);
    }
    LOG("panel %s: %s in %.1f ms", on ? "on" : "off", r ? strerror(-r) : "ok",
        (now_ns() - t0) / 1e6);
    if (!r) {
        c->powered = on;
        if (on && c->cur_valid && c->cur.serial) {
            /* The frame that lit the panel counts as presented. */
            struct mg_presented p = {.serial = c->cur.serial,
                                     .time_ns = now_ns(),
                                     .path = c->tier == TIER_GPU ? MG_PATH_GPU : MG_PATH_CPU};
            send_msg(c, MG_PRESENTED, &p, sizeof p, -1);
        }
    }
    reply.on = (uint32_t)c->powered;
    send_msg(c, MG_POWERED, &reply, sizeof reply, -1);
}

/* Draws the current frame into a spare output buffer and writes it out
 * (debugging: the planes path has no composed image to read back). */
static void write_shot(struct comp *c, const char *path) {
    if (!c->cur_valid)
        return;
    wait_flip(c);
    int buf = c->next_buf; /* not on screen and not in flight */
    int fence = draw(c, buf);
    if (fence >= 0) {
        struct pollfd p = {.fd = fence, .events = POLLIN};
        poll(&p, 1, 500);
        close(fence);
    }
    size_t len = (size_t)c->pitch * c->height;
    uint8_t *m = c->out[buf].map;
    int mapped = 0;
    if (!m) {
        m = mmap(NULL, len, PROT_READ, MAP_SHARED, c->out[buf].fd, 0);
        if (m == MAP_FAILED)
            return;
        mapped = 1;
    }
    dmabuf_sync(c->out[buf].fd, DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ);
    FILE *f = fopen(path, "wb");
    if (f) {
        fprintf(f, "P6 %d %d 255\n", c->width, c->height);
        uint8_t *row = malloc((size_t)c->width * 3);
        for (int y = 0; y < c->height && row; y++) {
            const uint8_t *s = m + (size_t)y * c->pitch;
            for (int x = 0; x < c->width; x++) {
                /* GPU output is RGBA bytes, CPU output 0xAARRGGBB (BGRA). */
                int gpu = c->tier == TIER_GPU;
                row[x * 3 + 0] = s[x * 4 + (gpu ? 0 : 2)];
                row[x * 3 + 1] = s[x * 4 + 1];
                row[x * 3 + 2] = s[x * 4 + (gpu ? 2 : 0)];
            }
            fwrite(row, 1, (size_t)c->width * 3, f);
        }
        free(row);
        fclose(f);
        LOG("shot: frame %u written to %s", c->cur.serial, path);
    }
    dmabuf_sync(c->out[buf].fd, DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ);
    if (mapped)
        munmap(m, len);
}

/* ---- setup ---- */

static int outputs_init(struct comp *c) {
    c->pitch = (c->width * 4 + 63) & ~63;
    for (int i = 0; i < NBUF; i++) {
        struct output *o = &c->out[i];
        /* GPU: written by the GPU, read by the display. CPU: written by
         * the CPU through its caches, flushed before each flip. */
        o->fd = gpu_dmabuf_alloc(c->tier == TIER_GPU ? "system-uncached" : "system",
                                 (size_t)c->pitch * c->height);
        if (o->fd < 0) {
            LOG("output dma_heap: %s", strerror(-o->fd));
            return -1;
        }
        uint32_t fourcc = c->tier == TIER_GPU ? DRM_FORMAT_ABGR8888 : DRM_FORMAT_XRGB8888;
        if (c->tier == TIER_GPU) {
            if (gpu_target_init(&c->g, &o->gpu, o->fd, (uint32_t)c->width,
                                (uint32_t)c->height, (uint32_t)c->pitch, fourcc))
                return -1;
        } else {
            o->map = mmap(NULL, (size_t)c->pitch * c->height,
                          PROT_READ | PROT_WRITE, MAP_SHARED, o->fd, 0);
            if (o->map == MAP_FAILED) {
                o->map = NULL;
                return -1;
            }
        }
        if (kms_add_fb(&c->k, o->fd, (uint32_t)c->width, (uint32_t)c->height,
                       (uint32_t)c->pitch, fourcc, &o->fb))
            return -1;
    }
    return 0;
}

static int listen_on(const char *path) {
    int fd = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC | SOCK_NONBLOCK, 0);
    if (fd < 0)
        return -1;
    struct sockaddr_un a = {.sun_family = AF_UNIX};
    if (strlen(path) >= sizeof a.sun_path) {
        close(fd);
        return -1;
    }
    strcpy(a.sun_path, path);
    unlink(path);
    mode_t old = umask(077);
    int r = bind(fd, (struct sockaddr *)&a, sizeof a);
    umask(old);
    if (r < 0 || listen(fd, 2) < 0) {
        LOG("socket %s: %s", path, strerror(errno));
        close(fd);
        return -1;
    }
    return fd;
}

int main(int argc, char **argv) {
    const char *socket_path = NULL, *card = "/dev/dri/card0", *ready_file = NULL;
    int hz = 120, floor_khz = 507000, force_cpu = 0, planes = 1, threads = 4;
    for (int i = 1; i < argc; i++) {
        const char *a = argv[i];
        int more = i + 1 < argc;
        if (!strcmp(a, "--socket") && more)
            socket_path = argv[++i];
        else if (!strcmp(a, "--card") && more)
            card = argv[++i];
        else if (!strcmp(a, "--hz") && more)
            hz = atoi(argv[++i]);
        else if (!strcmp(a, "--ready-file") && more)
            ready_file = argv[++i];
        else if (!strcmp(a, "--gpu-floor") && more)
            floor_khz = atoi(argv[++i]);
        else if (!strcmp(a, "--threads") && more)
            threads = atoi(argv[++i]);
        else if (!strcmp(a, "--cpu"))
            force_cpu = 1;
        else if (!strcmp(a, "--no-planes"))
            planes = 0;
        else {
            fprintf(stderr, "usage: moon-compositor --socket PATH [--card DEV] "
                            "[--hz N] [--ready-file PATH] [--gpu-floor KHZ] "
                            "[--cpu] [--no-planes] [--threads N]\n");
            return 2;
        }
    }
    if (!socket_path) {
        fprintf(stderr, "moon-compositor: --socket is required\n");
        return 2;
    }
    signal(SIGINT, on_signal);
    signal(SIGTERM, on_signal);
    signal(SIGPIPE, SIG_IGN);
    signal(SIGUSR1, on_signal);

    static struct comp c;
    c.client_fd = c.listen_fd = c.flight_fence = c.floor_fd = -1;
    c.planes = planes;
    c.floor_khz = floor_khz;
    c.profile = getenv("MOON_GPU_PROFILE") && *getenv("MOON_GPU_PROFILE") == '1';
    uint64_t t_start = now_ns();
    if (kms_open(&c.k, card, hz))
        return 10;
    c.width = c.k.mode.hdisplay;
    c.height = c.k.mode.vdisplay;

    /* The GPU tier needs EGL on the Mali and dmabuf render targets; anything
     * short of that runs the CPU tier. */
    c.tier = TIER_CPU;
    if (!force_cpu && !gpu_init(&c.g)) {
        c.tier = TIER_GPU;
        if (outputs_init(&c) || render_init(&c.r, c.width, c.height)) {
            LOG("GPU tier unavailable; CPU tier");
            for (int i = 0; i < NBUF; i++) {
                if (c.out[i].gpu.fbo || c.out[i].gpu.image)
                    gpu_target_fini(&c.g, &c.out[i].gpu);
                else if (c.out[i].fd > 0)
                    close(c.out[i].fd);
                if (c.out[i].fb)
                    kms_rm_fb(&c.k, c.out[i].fb);
                memset(&c.out[i], 0, sizeof c.out[i]);
            }
            gpu_fini(&c.g);
            c.tier = TIER_CPU;
        }
    }
    if (c.tier == TIER_CPU) {
        memset(&c.g, 0, sizeof c.g);
        if (outputs_init(&c))
            return 12;
        cpu_init(threads);
    }
    if (c.tier == TIER_GPU && floor_khz > 0) {
        c.floor_fd = open("/sys/kernel/gpu/gpu_min_clock", O_RDWR | O_CLOEXEC);
        ssize_t n = c.floor_fd >= 0 ? read(c.floor_fd, c.floor_restore,
                                          sizeof c.floor_restore - 1) : -1;
        if (n <= 0) {
            if (c.floor_fd >= 0)
                close(c.floor_fd);
            c.floor_fd = -1;
        } else {
            c.floor_restore[n] = 0;
        }
    }
    if (mirror_init(&c))
        return 13;
    kms_probe_scalers(&c.k, c.out[0].fb);

    /* Light the panel with a black frame: proves the whole display path. */
    if (c.tier == TIER_GPU) {
        glBindFramebuffer(GL_FRAMEBUFFER, c.out[0].gpu.fbo);
        glClearColor(0, 0, 0, 1);
        glClear(GL_COLOR_BUFFER_BIT);
        glFinish();
    } else {
        memset(c.out[0].map, 0, (size_t)c.pitch * c.height);
        flush_for_device(c.out[0].fd);
    }
    int r = c.k.crtc_active ? kms_commit_blocking(&c.k, c.out[0].fb)
                            : kms_commit(&c.k, c.out[0].fb, -1, 1, 0);
    if (r) {
        LOG("first frame (modeset %d): %s", !c.k.crtc_active, strerror(-r));
        return 14;
    }
    c.powered = 1;
    c.next_buf = 1;

    c.listen_fd = listen_on(socket_path);
    if (c.listen_fd < 0)
        return 15;
    LOG("ready in %.0f ms: %dx%d, %s tier, planes %s, socket %s",
        (now_ns() - t_start) / 1e6, c.width, c.height,
        c.tier == TIER_GPU ? "GPU" : "CPU", c.planes ? "on" : "off", socket_path);
    if (ready_file) {
        int fd = open(ready_file, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
        if (fd >= 0) {
            dprintf(fd, "READY=1\nPID=%d\nTIER=%s\n", getpid(),
                    c.tier == TIER_GPU ? "gpu" : "cpu");
            close(fd);
        }
    }

    uint64_t last_report = now_ns();
    while (!quit) {
        struct pollfd p[3] = {{.fd = c.k.fd, .events = POLLIN},
                              {.fd = c.listen_fd, .events = POLLIN},
                              {.fd = c.client_fd, .events = POLLIN}};
        int timeout = c.flip_pending || c.floored ? 50 : -1;
        int n = poll(p, c.client_fd >= 0 ? 3 : 2, timeout);
        if (n < 0 && errno != EINTR) {
            LOG("poll: %s", strerror(errno));
            break;
        }
        if (p[0].revents & POLLIN)
            read_flips(&c, 0);
        if (c.flip_pending && now_ns() - c.flight_since > FLIP_TIMEOUT_NS) {
            LOG("flip of serial %u lost", c.flight_serial);
            if (c.flight_fence >= 0)
                close(c.flight_fence);
            c.flight_fence = -1;
            c.flip_pending = 0;
            c.dirty = 1;
        }
        if (p[1].revents & POLLIN) {
            int fd = accept4(c.listen_fd, NULL, NULL, SOCK_CLOEXEC | SOCK_NONBLOCK);
            if (fd >= 0) {
                drop_client(&c, "replaced");
                c.client_fd = fd;
                LOG("client connected");
            }
        }
        if (c.client_fd >= 0 && n > 0 && (p[2].revents & (POLLIN | POLLHUP | POLLERR)))
            read_client(&c);
        maybe_submit(&c);
        if (shot) {
            shot = 0;
            char path[256];
            snprintf(path, sizeof path, "%s", socket_path);
            char *slash = strrchr(path, '/');
            snprintf(slash ? slash + 1 : path, sizeof path - (size_t)(slash ? slash + 1 - path : 0),
                     "shot.ppm");
            write_shot(&c, path);
        }
        /* Release the clock floor once frames stop. */
        if (c.floored && !c.flip_pending && now_ns() - c.last_submit_ns > 150000000ull)
            set_floor(&c, 0);
        if (c.profile && now_ns() - last_report > 10000000000ull) {
            last_report = now_ns();
            LOG("flips by path: planes %llu, gpu %llu, cpu %llu",
                (unsigned long long)c.path_counts[MG_PATH_PLANES],
                (unsigned long long)c.path_counts[MG_PATH_GPU],
                (unsigned long long)c.path_counts[MG_PATH_CPU]);
        }
    }
    set_floor(&c, 0);
    LOG("exiting");
    unlink(socket_path);
    if (ready_file)
        unlink(ready_file);
    return 0;
}
