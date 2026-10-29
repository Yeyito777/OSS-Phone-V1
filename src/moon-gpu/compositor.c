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
 * SIGHUP turns the planes tier off or on. SIGWINCH writes what the display
 * planes make of the current frame (planes off: the composed frame through
 * a spare plane), read back through the writeback connector, to wb.ppm next
 * to the socket (the panel itself cannot be read).
 * MOON_GPU_PROFILE=1 (or SIGUSR2, a toggle) logs every flip, and a count of
 * flips by path every 10 s. SIGUSR1 writes the current frame,
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
#include <limits.h>
#include <math.h>
#include <poll.h>
#include <setjmp.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
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
/* The loop wakes at least this often when idle, to re-arm its watchdog,
 * which ends the process when the loop has not turned for WATCHDOG_S. */
#define IDLE_WAKE_MS 20000
#define WATCHDOG_S 60

static volatile sig_atomic_t quit, shot, toggle_profile, toggle_planes, wb_shot, probe;
static void on_signal(int s) {
    if (s == SIGUSR1)
        shot = 1;
    else if (s == SIGUSR2)
        toggle_profile = 1;
    else if (s == SIGHUP)
        toggle_planes = 1;
    else if (s == SIGWINCH)
        wb_shot = 1;
    else if (s == SIGURG)
        probe = 1;
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
    uint8_t *map;    /* CPU view: a memfd, or a cached dmabuf (at offset) */
    size_t map_len;
    void *map_base;  /* the mapping itself, from the page below offset */
    size_t map_base_len;
    uint32_t offset; /* of the pixels in the fd (protocol v3) */
    int memfd;       /* GPU texture is a copy of map, refreshed on damage */
    /* A client's memory that may shrink (v4 MG_SURFACE_FOREIGN, unsealed):
     * read only into this copy, under the SIGBUS guard; cpu.px points here. */
    uint8_t *shadow;
    int report_idle; /* v4 MG_SURFACE_REPORT_IDLE */
    int in_use;      /* named by a frame since its last MG_SURFACE_IDLE */
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
    /* The frame on screen (the last flip's): while a flip is in flight it
     * is not `cur`, and its surfaces are still being scanned out. */
    struct frame shown;
    int shown_valid;

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
    /* Vblank to MG_PRESENTED sent, and the fence query within it (profile). */
    uint64_t present_ns, present_max_ns, fence_ns, presents;
    /* Per-frame costs for the profile summary: GPU/CPU render (submit to
     * drawing done), damage uploads (wall time, pixels), pixels shaded. */
    uint64_t render_sum_ns, render_max_ns, renders;
    uint64_t upload_sum_ns, upload_max_ns, uploads, upload_px;
    uint64_t shaded_px, draws;
    const char *flight_miss;
    uint32_t client_version;
    unsigned busy_planes; /* plane indexes the panel's last commit used */
};

static void dmabuf_sync(int fd, uint64_t flags) {
    struct dma_buf_sync s = {.flags = flags};
    while (ioctl(fd, DMA_BUF_IOCTL_SYNC, &s) < 0 && (errno == EINTR || errno == EAGAIN))
        ;
}

/* After the CPU wrote a cached buffer: clean its cache lines for the GPU and
 * the display. The end of a CPU access alone does that (arm64: a clean to
 * the point of coherency for every attachment); a start first only
 * invalidated the lines again, which doubled the cost of every mirror
 * update (10 MB: 2.9 against 1.5 ms on a little core, 1.0 against 0.84 on
 * a big one, measured on the phone with the buffer imported by the
 * display). Nothing here reads what a device wrote into these buffers. */
static void flush_for_device(int fd) {
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
    if (s->map_base)
        munmap(s->map_base, s->map_base_len);
    free(s->shadow);
    if (s->fd >= 0)
        close(s->fd);
    memset(s, 0, sizeof *s);
    s->fd = -1;
}

static void free_surface(struct comp *c, struct surface *s) {
    release(c, s);
    *s = c->surf[--c->nsurf];
}

static void settle(struct comp *c, uint32_t id);

static struct surface *new_surface(struct comp *c, uint32_t id) {
    struct surface *old = find_surface(c, id);
    if (old) {
        settle(c, id);
        free_surface(c, find_surface(c, id));
    }
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
                        s->cpu.px + (size_t)y0 * s->stride + (size_t)x0 * 4);
    }
    glPixelStorei(GL_UNPACK_ROW_LENGTH, 0);
}

/* The SIGBUS guard: a client may truncate its memfd while it is read. */
static sigjmp_buf guard_jmp;
static volatile sig_atomic_t guard_armed;

static void on_sigbus(int sig, siginfo_t *si, void *uc) {
    (void)si;
    (void)uc;
    if (guard_armed) {
        guard_armed = 0;
        siglongjmp(guard_jmp, 1);
    }
    signal(sig, SIG_DFL); /* a fault outside the guard: crash as usual */
    raise(sig);
}

/* Copies the damaged rows of a foreign memfd into the surface's own copy.
 * Returns -1 if the memory shrank under the copy. */
static int refresh_shadow(struct surface *s, const struct mg_rect *rects,
                          uint32_t count) {
    if (sigsetjmp(guard_jmp, 1))
        return -1;
    guard_armed = 1;
    for (uint32_t i = 0; i < count; i++) {
        int x0 = rects[i].x0 < 0 ? 0 : rects[i].x0;
        int y0 = rects[i].y0 < 0 ? 0 : rects[i].y0;
        int x1 = rects[i].x1 > s->width ? s->width : rects[i].x1;
        int y1 = rects[i].y1 > s->height ? s->height : rects[i].y1;
        for (int y = y0; y < y1 && x0 < x1; y++) {
            size_t at = (size_t)y * s->stride + (size_t)x0 * 4;
            memcpy(s->shadow + at, s->map + at, (size_t)(x1 - x0) * 4);
        }
    }
    guard_armed = 0;
    return 0;
}

/* The bytes an fd holds, or -1 if it cannot tell. */
static off_t fd_size(int fd) {
    struct stat st;
    if (!fstat(fd, &st) && S_ISREG(st.st_mode))
        return st.st_size;
    off_t end = lseek(fd, 0, SEEK_END); /* dmabufs report their size here */
    lseek(fd, 0, SEEK_SET);
    return end;
}

/* Maps a surface's pixels: `s->offset` bytes into `fd`, from the page
 * below (mmap offsets must be page-aligned). Writable when possible. */
static int map_pixels(struct surface *s, int fd, int writable) {
    size_t page = (size_t)sysconf(_SC_PAGESIZE);
    size_t lead = s->offset % page;
    s->map_len = (size_t)s->stride * s->height;
    s->map_base_len = lead + s->map_len;
    void *m = MAP_FAILED;
    if (writable)
        m = mmap(NULL, s->map_base_len, PROT_READ | PROT_WRITE, MAP_SHARED, fd,
                 (off_t)(s->offset - lead));
    if (m == MAP_FAILED)
        m = mmap(NULL, s->map_base_len, PROT_READ, MAP_SHARED, fd,
                 (off_t)(s->offset - lead));
    if (m == MAP_FAILED)
        return -1;
    s->map_base = m;
    s->map = (uint8_t *)m + lead;
    return 0;
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
        if (map_pixels(s, fd, 1))
            return -1;
        s->cpu = (struct cpu_image){s->map, width, height, stride, opaque};
    }
    if (c->tier == TIER_GPU) {
        s->tex.target = gpu_import_texture_at(&c->g, fd, (uint32_t)width,
                                              (uint32_t)height, (uint32_t)stride,
                                              s->offset, fourcc, &s->image,
                                              &s->tex.id);
        if (!s->tex.target)
            return -1;
        s->tex.width = width;
        s->tex.height = height;
        s->tex.opaque = opaque;
    }
    /* Display planes only take the layout seen right on the panel: rows a
     * multiple of 64 bytes, pixels at the start of the buffer, and the
     * framebuffer declared as wide as its rows, shown `width` wide from the
     * left (a DPP walked a framebuffer narrower than its pitch sheared; see
     * outputs_init and notes/gpu/DESIGN.md). Any other buffer is composed
     * by the GPU, so a client cannot garble the panel. */
    if (stride % 64 || s->offset) {
        s->fb = 0;
        LOG("surface %u: pitch %d, offset %u: GPU composition only (planes need 64-byte rows "
            "at offset 0)", s->id, stride, s->offset);
    } else if (kms_add_fb_offset(&c->k, fd, (uint32_t)stride / 4, (uint32_t)height,
                                 (uint32_t)stride, 0, fourcc, &s->fb)) {
        s->fb = 0; /* drawable, just not on a plane */
    }
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

static void wait_flip(struct comp *c);

/* The frame now on screen is `cur` (its flip completed, or it lit the
 * panel). */
static void shown_is_cur(struct comp *c) {
    struct mg_frame f = {.serial = c->cur.serial, .flags = c->cur.flags,
                         .base = c->cur.base, .count = c->cur.count};
    c->shown_valid = c->cur_valid && !frame_copy(&c->shown, &f, c->cur.nodes);
}

/* Whether the queued, current or on-screen frame names surface `id`. */
static int referenced(const struct comp *c, uint32_t id) {
    return (c->have_pending && frame_uses(&c->pending, id)) ||
           (c->cur_valid && frame_uses(&c->cur, id)) ||
           (c->shown_valid && frame_uses(&c->shown, id));
}

/* v4: MG_SURFACE_IDLE for every surface that asked and is no longer
 * referenced. */
static void send_idle(struct comp *c) {
    for (size_t i = 0; i < c->nsurf; i++) {
        struct surface *s = &c->surf[i];
        if (!s->report_idle || !s->in_use || referenced(c, s->id))
            continue;
        s->in_use = 0;
        struct mg_id m = {.id = s->id};
        send_msg(c, MG_SURFACE_IDLE, &m, sizeof m, -1);
    }
}

/* A new frame names these surfaces: they are in use until it is gone. */
static void mark_in_use(struct comp *c, const struct frame *f) {
    for (size_t i = 0; i < c->nsurf; i++)
        if (c->surf[i].report_idle && frame_uses(f, c->surf[i].id))
            c->surf[i].in_use = 1;
}

/* v4: tells the client surface `id` is gone. */
static void surface_failed(struct comp *c, uint32_t id, uint32_t reason) {
    if (c->client_version < 4)
        return;
    struct mg_failed m = {.id = id, .reason = reason};
    send_msg(c, MG_SURFACE_FAILED, &m, sizeof m, -1);
}

/* Before surface `id`'s buffer goes: a plane may scan it out until the flip
 * in flight completes (removing a framebuffer that is on screen turns its
 * plane off). */
static void settle(struct comp *c, uint32_t id) {
    if (c->flip_pending && ((c->cur_valid && frame_uses(&c->cur, id)) ||
                            (c->shown_valid && frame_uses(&c->shown, id))))
        wait_flip(c);
}

static void power(struct comp *c, int on);

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
        if (m->version < MG_PROTO_MIN_VERSION || m->version > MG_PROTO_VERSION) {
            LOG("client speaks version %u, not %u-%u", m->version,
                MG_PROTO_MIN_VERSION, MG_PROTO_VERSION);
            drop_client(c, "version");
            return;
        }
        c->client_version = m->version;
        /* A new client brings its own surfaces; the old ones may still be
         * on a plane until the flip in flight completes. */
        wait_flip(c);
        free_all_surfaces(c);
        c->shown_valid = 0;
        uint64_t mhz = (uint64_t)c->k.mode.clock * 1000000ull /
                       ((uint64_t)c->k.mode.htotal * c->k.mode.vtotal);
        struct mg_welcome w = {
            .version = m->version,
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
        if (blen < MG_SURFACE_SHM_V2_SIZE) {
            LOG("short message %u", h->type);
            return;
        }
        /* v2 messages end before `offset`. */
        struct mg_surface_shm shm = {0};
        memcpy(&shm, body, blen < sizeof shm ? blen : sizeof shm);
        const struct mg_surface_shm *m = &shm;
        uint32_t flags = m->flags;
        if (c->client_version < 4)
            flags &= MG_SURFACE_DMABUF;
        uint32_t fourcc;
        int opaque;
        if (nfds < 1 || !m->id || m->id == MG_MIRROR_ID || !m->width ||
            !m->height || m->width > 8192 || m->height > 8192 ||
            m->stride < m->width * 4 || m->stride > 65536 || (m->offset & 3) ||
            surface_format(m->format, &fourcc, &opaque)) {
            LOG("bad surface %u", m->id);
            surface_failed(c, m->id, MG_FAIL_INVALID);
            break;
        }
        /* The pixels must lie inside the fd. */
        uint64_t need = (uint64_t)m->offset + (uint64_t)m->stride * (m->height - 1) +
                        (uint64_t)m->width * 4;
        off_t have = fd_size(fds[0]);
        if (have >= 0 && (uint64_t)have < need) {
            LOG("surface %u: %llu bytes in its fd, %llu needed", m->id,
                (unsigned long long)have, (unsigned long long)need);
            surface_failed(c, m->id, MG_FAIL_INVALID);
            break;
        }
        struct surface *s = new_surface(c, m->id);
        if (!s)
            break;
        s->offset = m->offset;
        s->report_idle = (flags & MG_SURFACE_REPORT_IDLE) != 0;
        if (flags & MG_SURFACE_DMABUF) {
            int fd = dup(fds[0]);
            if (adopt_dmabuf(c, s, fd, (int)m->width, (int)m->height,
                             (int)m->stride, fourcc, opaque, c->tier == TIER_CPU)) {
                LOG("surface %u: dmabuf import failed", m->id);
                free_surface(c, s);
                surface_failed(c, m->id, MG_FAIL_IMPORT);
            }
            break;
        }
        /* A memfd: a texture copied from it, or read in place by the CPU;
         * a foreign one that may shrink, only through a guarded copy. */
        s->width = (int)m->width;
        s->height = (int)m->height;
        s->stride = (int)m->stride;
        s->fourcc = fourcc;
        s->opaque = opaque;
        s->memfd = 1;
        int seals = fcntl(fds[0], F_GET_SEALS);
        int sealed = seals >= 0 && (seals & F_SEAL_SHRINK);
        if (map_pixels(s, fds[0], 0) ||
            ((flags & MG_SURFACE_FOREIGN) && !sealed &&
             !(s->shadow = malloc(s->map_len)))) {
            LOG("surface %u: %s", m->id, s->map ? "no memory for a copy" : strerror(errno));
            free_surface(c, s);
            surface_failed(c, m->id, MG_FAIL_IMPORT);
            break;
        }
        struct mg_rect all = {0, 0, s->width, s->height};
        if (s->shadow && refresh_shadow(s, &all, 1)) {
            LOG("surface %u: its memory shrank", m->id);
            free_surface(c, s);
            surface_failed(c, m->id, MG_FAIL_FAULT);
            break;
        }
        s->cpu = (struct cpu_image){s->shadow ? s->shadow : s->map, s->width,
                                    s->height, s->stride, opaque};
        if (c->tier == TIER_GPU) {
            s->tex = (struct texture){.target = GL_TEXTURE_2D, .width = s->width,
                                      .height = s->height, .swizzle = 1,
                                      .opaque = opaque};
            glGenTextures(1, &s->tex.id);
            glBindTexture(GL_TEXTURE_2D, s->tex.id);
            glTexStorage2D(GL_TEXTURE_2D, 1, GL_RGBA8, s->width, s->height);
            upload_rects(s, &all, 1);
        }
        break;
    }
    case MG_SURFACE_DAMAGE: {
        NEED(struct mg_damage);
        const struct mg_damage *m = (const void *)body;
        if (blen < sizeof *m + (size_t)m->count * sizeof(struct mg_rect))
            break;
        const struct mg_rect *rects = (const void *)(m + 1);
        uint64_t u0 = now_ns();
        struct surface *s = find_surface(c, m->id);
        if (s && s->shadow && refresh_shadow(s, rects, m->count)) {
            LOG("surface %u: its memory shrank; dropped", m->id);
            if (c->cur_valid && frame_uses(&c->cur, m->id))
                c->dirty = 1;
            free_surface(c, s);
            s = NULL;
            surface_failed(c, m->id, MG_FAIL_FAULT);
        }
        if (s && s->memfd && c->tier == TIER_GPU)
            upload_rects(s, rects, m->count);
        if (s && c->cur_valid && frame_uses(&c->cur, m->id))
            c->dirty = 1;
        if (s && s->memfd) {
            uint64_t dt = now_ns() - u0;
            c->upload_sum_ns += dt;
            if (dt > c->upload_max_ns)
                c->upload_max_ns = dt;
            c->uploads++;
            for (uint32_t i = 0; i < m->count; i++) {
                int w = rects[i].x1 - rects[i].x0, h = rects[i].y1 - rects[i].y0;
                if (w > 0 && h > 0)
                    c->upload_px += (uint64_t)w * (uint64_t)h;
            }
        }
        struct mg_uploaded u = {.id = m->id, .serial = m->serial};
        send_msg(c, MG_UPLOADED, &u, sizeof u, -1);
        break;
    }
    case MG_SURFACE_COPY: {
        NEED(struct mg_surface_copy);
        const struct mg_surface_copy *m = (const void *)body;
        int known = find_surface(c, m->source) != NULL;
        if (make_copy(c, m)) {
            LOG("copy %u failed", m->id);
            surface_failed(c, m->id, known ? MG_FAIL_IMPORT : MG_FAIL_INVALID);
        }
        break;
    }
    case MG_SURFACE_FREE: {
        NEED(struct mg_id);
        uint32_t id = ((const struct mg_id *)body)->id;
        if (id != MG_MIRROR_ID && find_surface(c, id)) {
            settle(c, id);
            struct surface *s = find_surface(c, id);
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
        if (!frame_copy(&c->pending, m, (const void *)(m + 1))) {
            c->have_pending = 1;
            mark_in_use(c, &c->pending);
        }
        /* A queued frame it replaced may have been the last to name some. */
        send_idle(c);
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

static const char *plane_miss; /* why the last frame was drawn (profile) */

/* Maps the current frame onto planes, bottom first. Returns the layer
 * count, or -1 if some part needs drawing (plane_miss says what). */
static int plan_planes(struct comp *c, struct kms_layer *layers) {
#define MISS(why)                                                              \
    do {                                                                       \
        plane_miss = why;                                                      \
        return -1;                                                             \
    } while (0)
    const struct frame *f = &c->cur;
    int n = 0, scaled = 0;
    if (f->flags & MG_FRAME_HAS_BASE) {
        struct surface *s = find_surface(c, f->base);
        if (!s || !s->fb || !s->opaque || s->width < c->width || s->height < c->height)
            MISS("base");
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
            MISS("fill");
        }
        struct surface *s = find_surface(c, nd->surface);
        if (!s || !s->fb)
            MISS("no scanout buffer");
        if (n == KMS_MAX_PLANES)
            MISS("too many layers");
        if (nd->src_w <= 0 || nd->src_h <= 0)
            MISS("empty source");
        float kx = nd->dst_w / nd->src_w, ky = nd->dst_h / nd->src_h;
        /* Rounded corners only where the buffer has them, at this scale. */
        if (fabsf(nd->radius - s->baked_radius * kx) > 1.5f)
            MISS("corner radius");
        struct kms_layer *l = &layers[n];
        if (!clip_layer(c, l, nd->src_x, nd->src_y, nd->src_w, nd->src_h,
                        nd->dst_x, nd->dst_y, nd->dst_w, nd->dst_h))
            continue;
        /* DPP limits: 2x down to 8x up, at least 64x16 on screen. */
        int scaling = fabsf(kx - 1) > 0.002f || fabsf(ky - 1) > 0.002f;
        if (scaling && (kx < 0.5f || ky < 0.5f || kx > 8 || ky > 8))
            MISS("scale out of range");
        if (scaling && ++scaled > c->k.nscalers)
            MISS("too many scaled layers");
        if (l->dw < 64 || l->dh < 16)
            MISS("too small");
        /* The source must lie inside the buffer (the kernel refuses even a
         * fraction of a pixel outside), and a 1:1 layer must be exactly
         * 1:1: non-scaling planes reject any other ratio. */
        if (!scaling) {
            l->sx = roundf(l->sx);
            l->sy = roundf(l->sy);
            l->sw = (float)l->dw;
            l->sh = (float)l->dh;
        }
        if (l->sw > s->width || l->sh > s->height)
            MISS("source outside the buffer");
        if (l->sx < 0)
            l->sx = 0;
        if (l->sy < 0)
            l->sy = 0;
        if (l->sx + l->sw > s->width)
            l->sx = s->width - l->sw;
        if (l->sy + l->sh > s->height)
            l->sy = s->height - l->sh;
        float a = nd->alpha;
        /* A dim is plane alpha over black, so only at the very bottom. */
        if (nd->dim < 0.998f) {
            if (n != 0 || !s->opaque)
                MISS("dim above the bottom");
            a *= nd->dim;
        }
        l->fb = s->fb;
        l->alpha = (uint16_t)lroundf(fminf(a, 1.0f) * 65535.0f);
        l->premultiplied = !s->opaque;
        l->in_fence = -1;
        n++;
    }
    return n;
#undef MISS
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
        c->shaded_px += render_frame(&c->r, base, f->nodes, f->count, lookup_tex, c);
        c->draws++;
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
    plane_miss = c->planes ? NULL : "planes off";
    int n = c->planes ? plan_planes(c, layers) : -1;
    int tr = 0;
    if (n > 0 && kms_assign(&c->k, layers, n))
        plane_miss = "no plane fits";
    else if (n > 0 && (tr = kms_commit_layers(&c->k, layers, n, KMS_TEST, 0))) {
        plane_miss = "rejected by TEST_ONLY";
        static int told;
        if (c->profile && told++ < 6) {
            LOG("TEST_ONLY: %s, %d layers:", strerror(-tr), n);
            for (int i = 0; i < n; i++)
                LOG("  plane %u src %.2f,%.2f %.2fx%.2f dst %d,%d %dx%d alpha %u%s",
                    c->k.planes[layers[i].plane].id, layers[i].sx, layers[i].sy,
                    layers[i].sw, layers[i].sh, layers[i].dx, layers[i].dy,
                    layers[i].dw, layers[i].dh, layers[i].alpha,
                    layers[i].premultiplied ? " premultiplied" : "");
        }
    }
    else if (n > 0 && !(r = commit_retry(c, layers, n, tag))) {
        c->busy_planes = 0;
        for (int i = 0; i < n; i++)
            c->busy_planes |= 1u << layers[i].plane;
    }
    else if (!n)
        plane_miss = "nothing visible";
    c->flight_miss = plane_miss;
    if (r) {
        path = c->tier == TIER_GPU ? MG_PATH_GPU : MG_PATH_CPU;
        int buf = c->next_buf;
        fence = draw(c, buf);
        struct kms_layer l = {.fb = c->out[buf].fb, .sw = (float)c->width,
                              .sh = (float)c->height, .dw = c->width,
                              .dh = c->height, .alpha = 0xffff,
                              .in_fence = fence, .plane = c->k.primary};
        r = commit_retry(c, &l, 1, tag);
        if (!r) {
            c->next_buf = (buf + 1) % NBUF;
            c->busy_planes = 1u << c->k.primary;
        }
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
    uint64_t q0 = now_ns();
    uint64_t done = c->flight_fence >= 0 ? gpu_fence_time(c->flight_fence) : 0;
    uint64_t q1 = now_ns();
    struct mg_presented p = {
        .serial = c->flight_serial,
        .sequence = ev->sequence,
        .time_ns = ev->time_ns,
        .render_ns = done > c->flight_t0 ? done - c->flight_t0 : c->flight_render_ns,
        .path = c->flight_path,
    };
    c->path_counts[c->flight_path & 3]++;
    if (c->profile)
        LOG("flip serial %u seq %u interval %.3f ms (vblanks %u) %s %.3f ms%s%s",
            p.serial, ev->sequence,
            c->last_flip_ns ? (ev->time_ns - c->last_flip_ns) / 1e6 : 0.0,
            ev->sequence - c->last_seq,
            p.path == MG_PATH_PLANES ? "planes" : p.path == MG_PATH_GPU ? "gpu" : "cpu",
            p.render_ns / 1e6, p.path != MG_PATH_PLANES && c->flight_miss ? ": " : "",
            p.path != MG_PATH_PLANES && c->flight_miss ? c->flight_miss : "");
    c->last_flip_ns = ev->time_ns;
    c->last_seq = ev->sequence;
    if (c->flight_fence >= 0)
        close(c->flight_fence);
    c->flight_fence = -1;
    c->flip_pending = 0;
    shown_is_cur(c);
    if (p.serial)
        send_msg(c, MG_PRESENTED, &p, sizeof p, -1);
    if (p.path != MG_PATH_PLANES && p.render_ns) {
        c->render_sum_ns += p.render_ns;
        if (p.render_ns > c->render_max_ns)
            c->render_max_ns = p.render_ns;
        c->renders++;
    }
    uint64_t sent = now_ns();
    if (sent > ev->time_ns && sent - ev->time_ns < 100000000ull) {
        c->present_ns += sent - ev->time_ns;
        if (sent - ev->time_ns > c->present_max_ns)
            c->present_max_ns = sent - ev->time_ns;
        c->fence_ns += q1 - q0;
        c->presents++;
    }
    send_idle(c);
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
    /* A flip that never completed: its fence would leak with it. */
    if (c->flip_pending && c->flight_fence >= 0) {
        close(c->flight_fence);
        c->flight_fence = -1;
    }
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
        if (on)
            shown_is_cur(c);
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

/* Composes the current frame on spare planes of the writeback CRTC, as the
 * planes path would show it, and writes the result out (debugging: the
 * panel's own output cannot be read back). */
static void write_wb(struct comp *c, const char *path) {
    if (!c->cur_valid || !c->k.wb_conn) {
        LOG("writeback: %s", c->k.wb_conn ? "no frame" : "no writeback connector");
        return;
    }
    wait_flip(c);
    struct kms_layer layers[KMS_MAX_PLANES];
    plane_miss = NULL;
    int n;
    if (c->planes)
        n = plan_planes(c, layers);
    else { /* the composed frame on the panel, through another DPP */
        n = 1;
        layers[0] = (struct kms_layer){.fb = c->out[(c->next_buf + NBUF - 1) % NBUF].fb,
                                       .sw = (float)c->width, .sh = (float)c->height,
                                       .dw = c->width, .dh = c->height,
                                       .alpha = 0xffff, .in_fence = -1};
    }
    if (n <= 0 || kms_assign_except(&c->k, layers, n, c->busy_planes)) {
        LOG("writeback: the frame does not fit the spare planes (%s)",
            plane_miss ? plane_miss : "plane count");
        return;
    }
    size_t len = (size_t)c->pitch * c->height;
    int fd = gpu_dmabuf_alloc("system", len);
    uint32_t fb = 0;
    if (fd < 0 || kms_add_fb(&c->k, fd, (uint32_t)c->width, (uint32_t)c->height,
                             (uint32_t)c->pitch, DRM_FORMAT_ABGR8888, &fb)) {
        LOG("writeback: no output buffer");
        if (fd >= 0)
            close(fd);
        return;
    }
    char planes[64] = "";
    for (int i = 0; i < n; i++)
        snprintf(planes + strlen(planes), sizeof planes - strlen(planes), " %u",
                 c->k.planes[layers[i].plane].id);
    int r = kms_writeback(&c->k, layers, n, fb);
    LOG("writeback of %d layers on planes%s: %s", n, planes, r ? strerror(-r) : "ok");
    if (!r) {
        uint8_t *m = mmap(NULL, len, PROT_READ, MAP_SHARED, fd, 0);
        FILE *f = m != MAP_FAILED ? fopen(path, "wb") : NULL;
        if (f) {
            dmabuf_sync(fd, DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ);
            fprintf(f, "P6 %d %d 255\n", c->width, c->height);
            uint8_t *row = malloc((size_t)c->width * 3);
            for (int y = 0; y < c->height && row; y++) {
                const uint8_t *s = m + (size_t)y * c->pitch;
                for (int x = 0; x < c->width; x++)
                    memcpy(row + x * 3, s + x * 4, 3); /* ABGR8888: R, G, B bytes */
                fwrite(row, 1, (size_t)c->width * 3, f);
            }
            free(row);
            fclose(f);
            dmabuf_sync(fd, DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ);
            LOG("writeback: written to %s", path);
        }
        if (m != MAP_FAILED)
            munmap(m, len);
    }
    kms_rm_fb(&c->k, fb);
    close(fd);
}

/* Shows a test layer on a spare plane of the writeback CRTC and finds where
 * the plane really fetched it from: every pixel of the test buffer (padding
 * included) holds its own linear index, so each pixel read back names the
 * address it came from. The layer is fb_w x (src_y + h) with `pitch`, shown
 * from row src_y at (0, dst_y), 1080 wide; `scaler` picks a scaling plane.
 * The writeback buffer has `wb_pitch`. Logs the byte shift of the first,
 * middle and last rows (0 = right) and returns the first, or LONG_MIN. */
static long plane_probe(struct comp *c, int fb_w, int pitch, int src_w, int src_y,
                        int dst_y, int h, int scaler, int force) {
    int w = c->width, wb_pitch = (w * 4 + 63) & ~63;
    size_t len = (size_t)pitch * (size_t)(src_y + h);
    int fd = gpu_dmabuf_alloc("system", len);
    size_t wlen = (size_t)wb_pitch * c->height;
    int wfd = gpu_dmabuf_alloc("system", wlen);
    uint32_t fb = 0, wfb = 0;
    long base = LONG_MIN, stride = 0, col = 0;
    int plane = -1;
    uint32_t *px = MAP_FAILED;
    uint8_t *out = MAP_FAILED;
    if (fd < 0 || wfd < 0)
        goto done;
    px = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    out = mmap(NULL, wlen, PROT_READ | PROT_WRITE, MAP_SHARED, wfd, 0);
    if (px == MAP_FAILED || out == MAP_FAILED)
        goto done;
    dmabuf_sync(fd, DMA_BUF_SYNC_START | DMA_BUF_SYNC_WRITE);
    for (size_t i = 0; i < len / 4; i++)
        px[i] = (uint32_t)(i & 0xffffff);
    dmabuf_sync(fd, DMA_BUF_SYNC_END | DMA_BUF_SYNC_WRITE);
    dmabuf_sync(wfd, DMA_BUF_SYNC_START | DMA_BUF_SYNC_WRITE);
    memset(out, 0xee, wlen);
    dmabuf_sync(wfd, DMA_BUF_SYNC_END | DMA_BUF_SYNC_WRITE);
    if (kms_add_fb(&c->k, fd, (uint32_t)fb_w, (uint32_t)(src_y + h), (uint32_t)pitch,
                   DRM_FORMAT_XRGB8888, &fb) ||
        kms_add_fb(&c->k, wfd, (uint32_t)w, (uint32_t)c->height, (uint32_t)wb_pitch,
                   DRM_FORMAT_ABGR8888, &wfb))
        goto done;
    for (int p = 0; p < c->k.nplanes && plane < 0; p++)
        if (!((c->busy_planes >> p) & 1) && p != c->k.primary &&
            c->k.planes[p].scaler == scaler)
            plane = p;
    if (force >= 0)
        plane = force;
    if (plane < 0)
        goto done;
    struct kms_layer l = {.fb = fb, .sy = (float)src_y, .sw = (float)src_w,
                          .sh = (float)h, .dy = dst_y, .dw = src_w, .dh = h,
                          .alpha = 0xffff, .in_fence = -1, .plane = plane};
    int r = kms_writeback(&c->k, &l, 1, wfb);
    if (r) {
        LOG("probe: writeback: %s", strerror(-r));
        goto done;
    }
    dmabuf_sync(wfd, DMA_BUF_SYNC_START | DMA_BUF_SYNC_READ);
    /* ABGR8888 bytes R, G, B, A; the index was 0x00RRGGBB. */
#define AT(x, y)                                                               \
    ((long)out[(size_t)(y) * wb_pitch + (x) * 4] << 16 |                       \
     (long)out[(size_t)(y) * wb_pitch + (x) * 4 + 1] << 8 |                    \
     out[(size_t)(y) * wb_pitch + (x) * 4 + 2])
    long i0 = AT(0, dst_y), i8 = AT(0, dst_y + 8), ix = AT(src_w / 2, dst_y);
#undef AT
    base = (i0 - (long)src_y * (pitch / 4)) * 4;
    stride = (i8 - i0) / 8 * 4;
    col = ix - i0;
    dmabuf_sync(wfd, DMA_BUF_SYNC_END | DMA_BUF_SYNC_READ);
done:
    LOG("probe: fb %dx%d pitch %d, src %dx%d+0+%d -> y %d, plane %u%s: "
        "start %+ld bytes, row stride %ld, half row %ld px",
        fb_w, src_y + h, pitch, src_w, h, src_y, dst_y,
        plane >= 0 ? c->k.planes[plane].id : 0, scaler ? " (scaler)" : "",
        base, stride, col);
    if (fb)
        kms_rm_fb(&c->k, fb);
    if (wfb)
        kms_rm_fb(&c->k, wfb);
    if (px != MAP_FAILED)
        munmap(px, len);
    if (out != MAP_FAILED)
        munmap(out, wlen);
    if (fd >= 0)
        close(fd);
    if (wfd >= 0)
        close(wfd);
    return base;
}

/* The panel's primary plane, through the writeback: the panel shows the
 * current output on another plane meanwhile. */
static void probe_primary(struct comp *c) {
    wait_flip(c);
    int alt = -1;
    for (int p = 0; p < c->k.nplanes && alt < 0; p++)
        if (p != c->k.primary && !c->k.planes[p].scaler)
            alt = p;
    struct kms_layer l = {.fb = c->out[(c->next_buf + NBUF - 1) % NBUF].fb,
                          .sw = (float)c->width, .sh = (float)c->height,
                          .dw = c->width, .dh = c->height, .alpha = 0xffff,
                          .in_fence = -1, .plane = alt};
    int r = kms_commit_layers(&c->k, &l, 1, 0, 0);
    if (r) {
        LOG("probe: moving the panel to plane %u: %s", c->k.planes[alt].id, strerror(-r));
        return;
    }
    unsigned busy = c->busy_planes;
    c->busy_planes = 1u << alt;
    int pr = c->k.primary, w = c->width, p64 = (w * 4 + 63) & ~63;
    static const int t[][5] = {
        {1080, 4352, 1080, 0, 0}, {1080, 4352, 1080, 0, 860},
        {1080, 4320, 1080, 0, 0}, {1080, 4320, 1080, 0, 860},
        {1088, 4352, 1080, 0, 0}, {1088, 4352, 1080, 0, 860},
        {1088, 4352, 1088, 0, 0},
    };
    for (size_t i = 0; i < sizeof t / sizeof t[0]; i++)
        plane_probe(c, t[i][0], t[i][1], t[i][2], t[i][3], t[i][4], 64, 0, pr);
    plane_probe(c, w, p64, w, 0, 0, c->height, 0, pr);
    plane_probe(c, p64 / 4, p64, w, 0, 0, c->height, 0, pr);
    plane_probe(c, w, w * 4, w, 0, 0, c->height, 0, pr);
    c->busy_planes = busy;
    c->dirty = 1;
    c->cur_valid = c->cur_valid; /* the next submit returns to the primary */
}

static void probe_planes(struct comp *c) {
    probe_primary(c);
    wait_flip(c);
    static const int t[][5] = {
        /* fb_w, pitch, src_w, src_y, dst_y */
        {1080, 4352, 1080, 0, 0},   {1080, 4352, 1080, 0, 860},
        {1080, 4320, 1080, 0, 0},   {1080, 4320, 1080, 0, 860},
        {1088, 4352, 1088, 0, 0},   {1088, 4352, 1088, 0, 860},
        {1088, 4352, 1080, 0, 0},   {1088, 4352, 1080, 0, 860},
        {1024, 4096, 1024, 0, 0},   {1024, 4096, 1024, 0, 860},
        {1024, 4352, 1024, 0, 0},   {1024, 4352, 1024, 0, 860},
        {1072, 4288, 1072, 0, 0},   {1072, 4288, 1072, 0, 860},
        {1080, 4608, 1080, 0, 0},   {1080, 4608, 1080, 0, 860},
        {1080, 4352, 1080, 100, 0}, {1080, 4352, 1080, 100, 860},
        {1080, 4352, 540, 0, 860},
    };
    for (size_t i = 0; i < sizeof t / sizeof t[0]; i++)
        plane_probe(c, t[i][0], t[i][1], t[i][2], t[i][3], t[i][4], 64, 0, -1);
}

/* ---- setup ---- */

static int outputs_init(struct comp *c) {
    /* The panel showed a 64-byte-aligned pitch sheared on a framebuffer as
     * wide as the screen (notes/gpu/DESIGN.md), and the Mali renders only
     * into such rows: its framebuffer is as wide as the pitch instead, shown
     * from the left. The CPU tier's rows are as wide as the screen, like
     * Xorg's framebuffer. */
    c->pitch = c->tier == TIER_GPU ? (c->width * 4 + 63) & ~63 : c->width * 4;
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
        if (kms_add_fb(&c->k, o->fd, (uint32_t)c->pitch / 4, (uint32_t)c->height,
                       (uint32_t)c->pitch, fourcc, &o->fb))
            return -1;
    }
    return 0;
}

/* Without the framework nothing boosts the display path, so the scheduler
 * ran this loop's short bursts (a 10 MB cache clean, the commits) on slow
 * cores at low clocks. Like Moon's thread and Xorg's group, a 60 %
 * utilization floor for this thread while it runs (not for the threads it
 * starts, SCHED_FLAG_RESET_ON_FORK). Best effort. */
static void boost_self(void) {
    struct {
        uint32_t size, policy;
        uint64_t flags;
        int32_t nice;
        uint32_t priority;
        uint64_t runtime, deadline, period;
        uint32_t util_min, util_max;
    } attr = {.size = sizeof attr,
              .policy = 0, /* SCHED_OTHER */
              .flags = 0x01 | 0x20, /* RESET_ON_FORK | UTIL_CLAMP_MIN */
              .util_min = 614}; /* 60 % of 1024 */
    if (syscall(__NR_sched_setattr, 0, &attr, 0))
        LOG("utilization floor: %s", strerror(errno));
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
    int hz = 120, floor_khz = 702000, force_cpu = 0, planes = 1, threads = 4;
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
    signal(SIGUSR2, on_signal);
    signal(SIGHUP, on_signal);
    signal(SIGWINCH, on_signal);
    signal(SIGURG, on_signal);
    struct sigaction bus = {.sa_sigaction = on_sigbus, .sa_flags = SA_SIGINFO};
    sigaction(SIGBUS, &bus, NULL);

    static struct comp c;
    c.client_fd = c.listen_fd = c.flight_fence = c.floor_fd = -1;
    c.planes = planes;
    c.floor_khz = floor_khz;
    c.profile = getenv("MOON_GPU_PROFILE") && *getenv("MOON_GPU_PROFILE") == '1';
    uint64_t t_start = now_ns();
    alarm(WATCHDOG_S); /* startup, too, must not hang */
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
    /* A full modeset, whatever the display was left in: the CRTC off, then
     * on with the mode (the panel showed garbage when an inherited state
     * was kept; notes/gpu/DESIGN.md). */
    int r, was_active = c.k.crtc_active;
    if (was_active && (r = kms_set_active(&c.k, 0, 0)))
        LOG("modeset: CRTC off: %s", strerror(-r));
    r = kms_commit(&c.k, c.out[0].fb, -1, 1, 0);
    if (r) {
        LOG("first frame (modeset, was %s): %s", was_active ? "on" : "off",
            strerror(-r));
        return 14;
    }
    c.powered = 1;
    c.next_buf = 1;

    c.listen_fd = listen_on(socket_path);
    if (c.listen_fd < 0)
        return 15;
    boost_self();
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
    int lost_in_a_row = 0;
    while (!quit) {
        /* A watchdog that costs nothing while idle: if the loop stops
         * turning (a GPU or display hang), SIGALRM ends the process and
         * moon-gpu-session.sh restarts it. */
        alarm(WATCHDOG_S);
        struct pollfd p[3] = {{.fd = c.k.fd, .events = POLLIN},
                              {.fd = c.listen_fd, .events = POLLIN},
                              {.fd = c.client_fd, .events = POLLIN}};
        int timeout = c.flip_pending || c.floored ? 50 : IDLE_WAKE_MS;
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
            /* The display stopped flipping: let the session restart us. */
            if (++lost_in_a_row >= 5) {
                LOG("display not flipping; exiting");
                break;
            }
        }
        if (c.last_flip_ns && now_ns() - c.last_flip_ns < FLIP_TIMEOUT_NS)
            lost_in_a_row = 0;
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
        /* Before the submit, so the switch shows at once. */
        if (toggle_planes) {
            toggle_planes = 0;
            c.planes = !c.planes;
            c.dirty = c.cur_valid;
            LOG("planes %s", c.planes ? "on" : "off");
        }
        maybe_submit(&c);
        if (wb_shot) {
            wb_shot = 0;
            char path[256];
            snprintf(path, sizeof path, "%s", socket_path);
            char *slash = strrchr(path, '/');
            snprintf(slash ? slash + 1 : path,
                     sizeof path - (size_t)(slash ? slash + 1 - path : 0), "wb.ppm");
            write_wb(&c, path);
        }
        if (probe) {
            probe = 0;
            probe_planes(&c);
        }
        if (toggle_profile) {
            toggle_profile = 0;
            c.profile = !c.profile;
            LOG("profile %s", c.profile ? "on" : "off");
            /* The summaries cover the profiled time only. */
            last_report = now_ns();
            c.present_ns = c.present_max_ns = c.fence_ns = c.presents = 0;
            c.render_sum_ns = c.render_max_ns = c.renders = 0;
            c.upload_sum_ns = c.upload_max_ns = c.uploads = c.upload_px = 0;
            c.shaded_px = c.draws = 0;
            memset(c.path_counts, 0, sizeof c.path_counts);
        }
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
            LOG("flips by path: planes %llu, gpu %llu, cpu %llu; vblank to MG_PRESENTED "
                "mean %.3f ms, max %.3f ms (fence query %.3f ms)",
                (unsigned long long)c.path_counts[MG_PATH_PLANES],
                (unsigned long long)c.path_counts[MG_PATH_GPU],
                (unsigned long long)c.path_counts[MG_PATH_CPU],
                c.presents ? c.present_ns / 1e6 / c.presents : 0.0, c.present_max_ns / 1e6,
                c.presents ? c.fence_ns / 1e6 / c.presents : 0.0);
            c.present_ns = c.present_max_ns = c.fence_ns = c.presents = 0;
            LOG("per frame: render %.2f ms mean, %.2f max (%llu frames); shaded %.2f MP; "
                "uploads %.2f ms mean, %.2f max, %.2f MP each (%llu)",
                c.renders ? c.render_sum_ns / 1e6 / c.renders : 0.0, c.render_max_ns / 1e6,
                (unsigned long long)c.renders,
                c.draws ? c.shaded_px / 1e6 / c.draws : 0.0,
                c.uploads ? c.upload_sum_ns / 1e6 / c.uploads : 0.0, c.upload_max_ns / 1e6,
                c.uploads ? c.upload_px / 1e6 / c.uploads : 0.0, (unsigned long long)c.uploads);
            c.render_sum_ns = c.render_max_ns = c.renders = 0;
            c.upload_sum_ns = c.upload_max_ns = c.uploads = c.upload_px = 0;
            c.shaded_px = c.draws = 0;
        }
    }
    alarm(0);
    set_floor(&c, 0);
    LOG("exiting");
    unlink(socket_path);
    if (ready_file)
        unlink(ready_file);
    return 0;
}
