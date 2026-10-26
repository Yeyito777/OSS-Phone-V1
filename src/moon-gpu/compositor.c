/* moon-compositor: Moon's GPU compositor.
 *
 * The only DRM master on the phone in GPU mode. Moon (in the Alpine chroot)
 * connects over a Unix SOCK_SEQPACKET socket (proto.h), shares its CPU-drawn
 * layers as memfds and describes each frame as a base surface plus nodes;
 * this process renders them with the Mali GPU into dmabufs and page-flips
 * one frame per vblank with atomic KMS, reporting each flip back. It also
 * owns the "X mirror", a dmabuf that Xorg (on its headless dummy driver)
 * writes the visible root into, sampled as a texture with no copy.
 *
 *   moon-compositor --socket PATH [--card /dev/dri/card0] [--hz 120]
 *                   [--heap system-uncached] [--ready-file PATH]
 *
 * MOON_GPU_PROFILE=1 logs every flip. Exits on SIGTERM; closing the card
 * turns the CRTC off.
 */
#define _GNU_SOURCE
#include "gpu.h"
#include "kms.h"
#include "proto.h"
#include "render.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <signal.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>
#include <drm/drm_fourcc.h>
#include <GLES2/gl2ext.h>

#define NBUF 3
#define MAX_FDS 4
/* A flip that has not completed after this long is presumed lost. */
#define FLIP_TIMEOUT_NS 250000000ull

static volatile sig_atomic_t quit;
static void on_signal(int s) {
    (void)s;
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

struct surface {
    uint32_t id;
    struct texture tex;
    uint8_t *map; /* shared memory (memfd) surfaces */
    size_t map_len;
    uint32_t stride;
};

struct frame {
    uint32_t serial, flags, base, count;
    struct mg_node *nodes;
    uint32_t cap;
};

struct comp {
    struct kms k;
    struct gpu g;
    struct renderer r;
    struct gpu_target tgt[NBUF];
    uint32_t fb[NBUF];
    int next_buf;
    int width, height;

    struct surface *surf;
    size_t nsurf, capsurf;

    /* X mirror */
    int mirror_fd;
    uint32_t mirror_stride;
    EGLImageKHR mirror_image;
    struct texture mirror;

    struct frame cur, pending;
    int have_pending, cur_valid, dirty;

    /* the commit waiting for its flip */
    int flip_pending;
    uint32_t flight_serial;
    int flight_fence;
    uint64_t flight_t0, flight_since;

    int powered;
    int listen_fd, client_fd;
    int profile;
    uint64_t last_flip_ns;
    uint32_t last_seq;
};

/* ---- surfaces ---- */

static struct surface *find_surface(struct comp *c, uint32_t id) {
    for (size_t i = 0; i < c->nsurf; i++)
        if (c->surf[i].id == id)
            return &c->surf[i];
    return NULL;
}

static const struct texture *lookup(void *ctx, uint32_t id) {
    struct comp *c = ctx;
    if (id == MG_MIRROR_ID)
        return c->mirror.id ? &c->mirror : NULL;
    struct surface *s = find_surface(c, id);
    return s && s->tex.id ? &s->tex : NULL;
}

static void free_surface(struct comp *c, struct surface *s) {
    if (s->tex.id)
        glDeleteTextures(1, &s->tex.id);
    if (s->map)
        munmap(s->map, s->map_len);
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
    glPixelStorei(GL_UNPACK_ROW_LENGTH, (GLint)(s->stride / 4));
    for (uint32_t i = 0; i < count; i++) {
        int x0 = rects[i].x0 < 0 ? 0 : rects[i].x0;
        int y0 = rects[i].y0 < 0 ? 0 : rects[i].y0;
        int x1 = rects[i].x1 > s->tex.width ? s->tex.width : rects[i].x1;
        int y1 = rects[i].y1 > s->tex.height ? s->tex.height : rects[i].y1;
        if (x1 <= x0 || y1 <= y0)
            continue;
        glTexSubImage2D(GL_TEXTURE_2D, 0, x0, y0, x1 - x0, y1 - y0, GL_RGBA,
                        GL_UNSIGNED_BYTE,
                        s->map + (size_t)y0 * s->stride + (size_t)x0 * 4);
    }
    glPixelStorei(GL_UNPACK_ROW_LENGTH, 0);
}

/* ---- the X mirror ---- */

static int mirror_init(struct comp *c, const char *heap) {
    c->mirror_stride = ((uint32_t)c->width * 4 + 63) & ~63u;
    size_t size = (size_t)c->mirror_stride * c->height;
    c->mirror_fd = gpu_dmabuf_alloc(heap, size);
    if (c->mirror_fd < 0) {
        LOG("mirror: dma_heap %s: %s", heap, strerror(-c->mirror_fd));
        return -1;
    }
    void *m = mmap(NULL, size, PROT_READ | PROT_WRITE, MAP_SHARED, c->mirror_fd, 0);
    if (m != MAP_FAILED) {
        memset(m, 0, size);
        munmap(m, size);
    }
    EGLint attr[] = {EGL_WIDTH, c->width, EGL_HEIGHT, c->height,
                     EGL_LINUX_DRM_FOURCC_EXT, DRM_FORMAT_XRGB8888,
                     EGL_DMA_BUF_PLANE0_FD_EXT, c->mirror_fd,
                     EGL_DMA_BUF_PLANE0_OFFSET_EXT, 0,
                     EGL_DMA_BUF_PLANE0_PITCH_EXT, (EGLint)c->mirror_stride,
                     EGL_NONE};
    c->mirror_image = c->g.create_image(c->g.dpy, EGL_NO_CONTEXT,
                                        EGL_LINUX_DMA_BUF_EXT, NULL, attr);
    if (c->mirror_image == EGL_NO_IMAGE_KHR) {
        LOG("mirror: EGLImage: 0x%x", eglGetError());
        return -1;
    }
    GLenum targets[2] = {GL_TEXTURE_2D, GL_TEXTURE_EXTERNAL_OES};
    for (int i = 0; i < 2; i++) {
        GLuint t;
        glGenTextures(1, &t);
        glBindTexture(targets[i], t);
        while (glGetError())
            ;
        c->g.image_target_tex(targets[i], c->mirror_image);
        if (!glGetError()) {
            c->mirror = (struct texture){.id = t, .target = targets[i],
                                         .width = c->width,
                                         .height = c->height, .opaque = 1};
            LOG("mirror: %dx%d stride %u as %s", c->width, c->height,
                c->mirror_stride, i ? "external texture" : "2D texture");
            return 0;
        }
        glDeleteTextures(1, &t);
    }
    LOG("mirror: the dmabuf cannot be sampled");
    return -1;
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
    /* The last frame stays on screen; its surfaces go with the client. A new
     * client starts with a fresh frame. */
    c->have_pending = 0;
    c->cur_valid = 0;
    free_all_surfaces(c);
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
        uint64_t period = 1000000000000ull / ((uint64_t)c->k.mode.clock * 1000000ull /
                                              ((uint64_t)c->k.mode.htotal * c->k.mode.vtotal) *
                                              1000ull);
        struct mg_welcome w = {
            .version = MG_PROTO_VERSION,
            .width = (uint32_t)c->width,
            .height = (uint32_t)c->height,
            .refresh_mhz = (uint32_t)((uint64_t)c->k.mode.clock * 1000000ull /
                                      ((uint64_t)c->k.mode.htotal * c->k.mode.vtotal)),
            .period_ns = period,
        };
        send_msg(c, MG_WELCOME, &w, sizeof w, -1);
        struct mg_surface_shm mi = {.width = (uint32_t)c->width,
                                    .height = (uint32_t)c->height,
                                    .stride = c->mirror_stride,
                                    .format = MG_XRGB8888};
        send_msg(c, MG_MIRROR, &mi, sizeof mi, c->mirror_fd);
        LOG("client hello");
        break;
    }
    case MG_SURFACE_SHM: {
        NEED(struct mg_surface_shm);
        const struct mg_surface_shm *m = (const void *)body;
        if (nfds < 1 || !m->id || m->id == MG_MIRROR_ID || !m->width ||
            !m->height || m->width > 8192 || m->height > 8192 ||
            m->stride < m->width * 4) {
            LOG("bad surface %u", m->id);
            break;
        }
        struct surface *s = new_surface(c, m->id);
        if (!s)
            break;
        s->map_len = (size_t)m->stride * m->height;
        s->stride = m->stride;
        s->map = mmap(NULL, s->map_len, PROT_READ, MAP_SHARED, fds[0], 0);
        if (s->map == MAP_FAILED) {
            LOG("surface %u: mmap: %s", m->id, strerror(errno));
            s->map = NULL;
            free_surface(c, s);
            break;
        }
        s->tex = (struct texture){.target = GL_TEXTURE_2D,
                                  .width = (int)m->width,
                                  .height = (int)m->height,
                                  .swizzle = 1,
                                  .opaque = m->format == MG_XRGB8888};
        glGenTextures(1, &s->tex.id);
        glBindTexture(GL_TEXTURE_2D, s->tex.id);
        glTexStorage2D(GL_TEXTURE_2D, 1, GL_RGBA8, (GLsizei)m->width,
                       (GLsizei)m->height);
        struct mg_rect all = {0, 0, (int32_t)m->width, (int32_t)m->height};
        upload_rects(s, &all, 1);
        break;
    }
    case MG_SURFACE_DAMAGE: {
        NEED(struct mg_damage);
        const struct mg_damage *m = (const void *)body;
        if (blen < sizeof *m + (size_t)m->count * sizeof(struct mg_rect))
            break;
        struct surface *s = find_surface(c, m->id);
        if (s && s->map) {
            upload_rects(s, (const void *)(m + 1), m->count);
            if (c->cur_valid && frame_uses(&c->cur, m->id))
                c->dirty = 1;
        }
        struct mg_uploaded u = {.id = m->id, .serial = m->serial};
        send_msg(c, MG_UPLOADED, &u, sizeof u, -1);
        break;
    }
    case MG_SURFACE_COPY: {
        NEED(struct mg_surface_copy);
        const struct mg_surface_copy *m = (const void *)body;
        const struct texture *from = lookup(c, m->source);
        if (!from || !m->id || m->id == MG_MIRROR_ID || !m->width ||
            !m->height || m->width > 8192 || m->height > 8192)
            break;
        struct texture t;
        if (render_copy(&c->r, from, m->src_x, m->src_y, m->src_w, m->src_h,
                        (int)m->width, (int)m->height, &t))
            break;
        struct surface *s = new_surface(c, m->id);
        if (s)
            s->tex = t;
        break;
    }
    case MG_SURFACE_FREE: {
        NEED(struct mg_id);
        struct surface *s = find_surface(c, ((const struct mg_id *)body)->id);
        if (s)
            free_surface(c, s);
        break;
    }
    case MG_MIRROR_DAMAGE:
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

/* ---- frames ---- */

static int draw(struct comp *c, int buf) {
    const struct frame *f = &c->cur;
    const struct texture *base =
        (f->flags & MG_FRAME_HAS_BASE) ? lookup(c, f->base) : NULL;
    glBindFramebuffer(GL_FRAMEBUFFER, c->tgt[buf].fbo);
    render_frame(&c->r, base, f->nodes, f->count, lookup, c);
    return gpu_fence(&c->g);
}

/* Renders the current frame and queues its flip for the next vblank. */
static void submit(struct comp *c, uint32_t serial) {
    int buf = c->next_buf;
    uint64_t t0 = now_ns();
    int fence = draw(c, buf);
    /* EBUSY can come back for a moment after the previous flip's event
     * while the driver's commit work finishes; retry with the same frame. */
    int r, tries = 0;
    while ((r = kms_commit(&c->k, c->fb[buf], fence, 0, serial)) == -EBUSY &&
           tries++ < 40)
        usleep(250);
    if (r) {
        LOG("commit: %s", strerror(-r));
        if (fence >= 0)
            close(fence);
        return;
    }
    c->flip_pending = 1;
    c->flight_serial = serial;
    c->flight_fence = fence;
    c->flight_t0 = t0;
    c->flight_since = now_ns();
    c->next_buf = (buf + 1) % NBUF;
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
    uint64_t gpu_done = c->flight_fence >= 0 ? gpu_fence_time(c->flight_fence) : 0;
    struct mg_presented p = {
        .serial = c->flight_serial,
        .sequence = ev->sequence,
        .time_ns = ev->time_ns,
        .render_ns = gpu_done > c->flight_t0 ? gpu_done - c->flight_t0 : 0,
    };
    if (c->profile)
        LOG("flip serial %u seq %u interval %.3f ms (vblanks %u) render %.3f ms",
            p.serial, ev->sequence,
            c->last_flip_ns ? (ev->time_ns - c->last_flip_ns) / 1e6 : 0.0,
            ev->sequence - c->last_seq, p.render_ns / 1e6);
    c->last_flip_ns = ev->time_ns;
    c->last_seq = ev->sequence;
    if (c->flight_fence >= 0)
        close(c->flight_fence);
    c->flight_fence = -1;
    c->flip_pending = 0;
    if (p.serial)
        send_msg(c, MG_PRESENTED, &p, sizeof p, -1);
}

static void wait_flip(struct comp *c) {
    struct kms_flip ev;
    if (c->flip_pending && kms_wait_flip(&c->k, 200, &ev) > 0)
        flip_done(c, &ev);
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
        /* Light the panel with the newest frame, rendered while dark. */
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
        } else {
            glBindFramebuffer(GL_FRAMEBUFFER, c->tgt[buf].fbo);
            glClearColor(0, 0, 0, 1);
            glClear(GL_COLOR_BUFFER_BIT);
            glFinish();
        }
        r = kms_set_active(&c->k, 1, c->fb[buf]);
        c->next_buf = (buf + 1) % NBUF;
    } else {
        r = kms_set_active(&c->k, 0, 0);
    }
    LOG("panel %s: %s in %.1f ms", on ? "on" : "off", r ? strerror(-r) : "ok",
        (now_ns() - t0) / 1e6);
    if (!r) {
        c->powered = on;
        if (on && c->cur_valid && c->cur.serial) {
            /* The frame that lit the panel counts as presented. */
            struct mg_presented p = {.serial = c->cur.serial,
                                     .time_ns = now_ns()};
            send_msg(c, MG_PRESENTED, &p, sizeof p, -1);
        }
    }
    reply.on = (uint32_t)c->powered;
    send_msg(c, MG_POWERED, &reply, sizeof reply, -1);
}

/* ---- setup ---- */

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
    const char *socket_path = NULL, *card = "/dev/dri/card0",
               *heap = "system-uncached", *ready_file = NULL;
    int hz = 120;
    for (int i = 1; i < argc; i++) {
        const char *a = argv[i];
        int more = i + 1 < argc;
        if (!strcmp(a, "--socket") && more)
            socket_path = argv[++i];
        else if (!strcmp(a, "--card") && more)
            card = argv[++i];
        else if (!strcmp(a, "--hz") && more)
            hz = atoi(argv[++i]);
        else if (!strcmp(a, "--heap") && more)
            heap = argv[++i];
        else if (!strcmp(a, "--ready-file") && more)
            ready_file = argv[++i];
        else {
            fprintf(stderr, "usage: moon-compositor --socket PATH [--card DEV] "
                            "[--hz N] [--heap NAME] [--ready-file PATH]\n");
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

    static struct comp c;
    c.client_fd = c.listen_fd = c.mirror_fd = c.flight_fence = -1;
    c.profile = getenv("MOON_GPU_PROFILE") && *getenv("MOON_GPU_PROFILE") == '1';
    uint64_t t_start = now_ns();
    if (kms_open(&c.k, card, hz))
        return 10;
    c.width = c.k.mode.hdisplay;
    c.height = c.k.mode.vdisplay;
    if (gpu_init(&c.g))
        return 11;
    uint32_t pitch = ((uint32_t)c.width * 4 + 63) & ~63u;
    for (int i = 0; i < NBUF; i++) {
        int fd = gpu_dmabuf_alloc(heap, (size_t)pitch * c.height);
        if (fd < 0) {
            LOG("dma_heap %s: %s", heap, strerror(-fd));
            return 12;
        }
        if (gpu_target_init(&c.g, &c.tgt[i], fd, (uint32_t)c.width,
                            (uint32_t)c.height, pitch, DRM_FORMAT_ABGR8888) ||
            kms_add_fb(&c.k, fd, (uint32_t)c.width, (uint32_t)c.height, pitch,
                       DRM_FORMAT_ABGR8888, &c.fb[i]))
            return 12;
    }
    if (render_init(&c.r, c.width, c.height) || mirror_init(&c, heap))
        return 13;

    /* Light the panel with a black frame: proves the whole display path. */
    glBindFramebuffer(GL_FRAMEBUFFER, c.tgt[0].fbo);
    glClearColor(0, 0, 0, 1);
    glClear(GL_COLOR_BUFFER_BIT);
    glFinish();
    int r = c.k.crtc_active ? kms_commit_blocking(&c.k, c.fb[0])
                            : kms_commit(&c.k, c.fb[0], -1, 1, 0);
    if (r) {
        LOG("first frame (modeset %d): %s", !c.k.crtc_active, strerror(-r));
        return 14;
    }
    c.powered = 1;
    c.next_buf = 1;

    c.listen_fd = listen_on(socket_path);
    if (c.listen_fd < 0)
        return 15;
    LOG("ready in %.0f ms: %dx%d, socket %s", (now_ns() - t_start) / 1e6,
        c.width, c.height, socket_path);
    if (ready_file) {
        int fd = open(ready_file, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
        if (fd >= 0) {
            dprintf(fd, "READY=1\nPID=%d\n", getpid());
            close(fd);
        }
    }

    while (!quit) {
        struct pollfd p[3] = {{.fd = c.k.fd, .events = POLLIN},
                              {.fd = c.listen_fd, .events = POLLIN},
                              {.fd = c.client_fd, .events = POLLIN}};
        int timeout = c.flip_pending ? 50 : -1;
        int n = poll(p, c.client_fd >= 0 ? 3 : 2, timeout);
        if (n < 0 && errno != EINTR) {
            LOG("poll: %s", strerror(errno));
            break;
        }
        if (p[0].revents & POLLIN) {
            struct kms_flip ev;
            if (kms_wait_flip(&c.k, 0, &ev) > 0)
                flip_done(&c, &ev);
        }
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
    }
    LOG("exiting");
    unlink(socket_path);
    if (ready_file)
        unlink(ready_file);
    return 0;
}
