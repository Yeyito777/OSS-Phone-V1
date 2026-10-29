/* moon-compositor-sim: moon-compositor's own code (compositor.c, cpu.c,
 * render.c) with the phone's display and GPU simulated, so Moon can be
 * tested against the real compositor logic off the phone (in a sandbox Xorg
 * or Xvfb): scripts/gpu/sim-build.sh builds it for the host.
 *
 * KMS: a 120 Hz panel. A nonblocking commit flips at the next vblank (one
 * at a time, EBUSY meanwhile) and its event arrives on a timerfd at that
 * vblank; every plane takes whatever layer it is given. GPU: none, so the
 * CPU tier runs, and buffers are memfds rather than dma-heap buffers.
 *
 * The panel also scans out, as a real one does: each refresh it reads the
 * rows of the buffers on its planes from top to bottom over the refresh. A
 * refresh is torn when a row changed after the refresh began (a writer
 * wrote the buffer the display was showing). MOON_SIM_STATS=PATH appends a
 * line per second: refreshes, refreshes showing a new frame, torn ones;
 * MOON_SIM_FRAMES=PATH a line per refresh that showed a new frame or tore:
 * its vblank time (CLOCK_MONOTONIC seconds), "new" and/or "torn". */
#define _GNU_SOURCE
#include "gpu.h"
#include "kms.h"

#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/timerfd.h>
#include <time.h>
#include <unistd.h>

/* ---- GPU: absent ---- */

int gpu_init(struct gpu *g) {
    (void)g;
    return -1;
}
void gpu_fini(struct gpu *g) { (void)g; }
int gpu_dmabuf_alloc(const char *heap, size_t size) {
    (void)heap;
    int fd = memfd_create("moon-sim-buffer", MFD_CLOEXEC);
    if (fd < 0)
        return -errno;
    if (ftruncate(fd, (off_t)size)) {
        int e = errno;
        close(fd);
        return -e;
    }
    return fd;
}
int gpu_target_init(struct gpu *g, struct gpu_target *t, int fd, uint32_t width,
                    uint32_t height, uint32_t pitch, uint32_t fourcc) {
    (void)g, (void)t, (void)fd, (void)width, (void)height, (void)pitch, (void)fourcc;
    return -1;
}
void gpu_target_fini(struct gpu *g, struct gpu_target *t) { (void)g, (void)t; }
GLenum gpu_import_texture_at(struct gpu *g, int fd, uint32_t width, uint32_t height,
                             uint32_t pitch, uint32_t offset, uint32_t fourcc,
                             EGLImageKHR *image, GLuint *texture) {
    (void)g, (void)fd, (void)width, (void)height, (void)pitch, (void)offset;
    (void)fourcc, (void)image, (void)texture;
    return 0;
}
int gpu_fence(struct gpu *g) {
    (void)g;
    return -1;
}
uint64_t gpu_fence_time(int fd) {
    (void)fd;
    return 0;
}
GLuint gpu_program(const char *vs, const char *fs) {
    (void)vs, (void)fs;
    return 0;
}

/* ---- KMS: a simulated 120 Hz panel ---- */

#define MAX_FBS 256
#define SAMPLE_EVERY 31 /* rows */

struct fb {
    uint32_t id;
    uint8_t *map;
    size_t len;
    uint32_t width, height, pitch, offset;
};

static struct {
    pthread_mutex_t lock;
    struct fb fbs[MAX_FBS];
    uint32_t next_fb;
    uint64_t t0, period;
    /* on screen, and committed for the next vblank */
    struct kms_layer shown[KMS_MAX_PLANES], queued[KMS_MAX_PLANES];
    int nshown, nqueued, flip_pending;
    uint64_t user_data, flip_at;
    uint32_t sequence;
    int active;
    uint64_t refreshes, new_frames, torn;
    int latched; /* the pending flip is on screen already (scanout saw it) */
} sim = {.lock = PTHREAD_MUTEX_INITIALIZER};

static uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000000000ull + ts.tv_nsec;
}

static void show(const struct kms_layer *layers, int n) {
    memcpy(sim.shown, layers, (size_t)n * sizeof *layers);
    sim.nshown = n;
}

static void sleep_until(uint64_t at) {
    struct timespec ts = {(time_t)(at / 1000000000ull), (long)(at % 1000000000ull)};
    while (clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &ts, NULL) == EINTR)
        ;
}

static struct fb *fb_find(uint32_t id) {
    for (int i = 0; i < MAX_FBS; i++)
        if (sim.fbs[i].id == id)
            return &sim.fbs[i];
    return NULL;
}

static uint64_t row_hash(const struct kms_layer *l, int screen_y) {
    struct fb *f = fb_find(l->fb);
    if (!f || screen_y < l->dy || screen_y >= l->dy + l->dh)
        return 0;
    int sy = (int)(l->sy + (screen_y - l->dy) * l->sh / l->dh);
    if (sy < 0 || (uint32_t)sy >= f->height)
        return 0;
    const uint8_t *row = f->map + f->offset + (size_t)sy * f->pitch;
    uint64_t h = 1469598103934665603ull;
    for (uint32_t x = 0; x < f->width * 4; x += 8) {
        uint64_t v;
        memcpy(&v, row + x, sizeof v);
        h = (h ^ v) * 1099511628211ull;
    }
    return h;
}

/* One refresh after another: what the planes show is read row by row at
 * the time the panel would scan that row. */
static void *scanout(void *arg) {
    (void)arg;
    enum { ROWS = 2340 / SAMPLE_EVERY + 1 };
    static uint64_t start[KMS_MAX_PLANES][ROWS], last[KMS_MAX_PLANES][ROWS];
    FILE *stats = getenv("MOON_SIM_STATS") ? fopen(getenv("MOON_SIM_STATS"), "a") : NULL;
    FILE *frames = getenv("MOON_SIM_FRAMES") ? fopen(getenv("MOON_SIM_FRAMES"), "a") : NULL;
    uint64_t next_report = now_ns() + 1000000000ull, r0 = 0, n0 = 0, t0 = 0;
    for (uint64_t n = (now_ns() - sim.t0) / sim.period + 1;; n++) {
        uint64_t vblank = sim.t0 + n * sim.period;
        sleep_until(vblank);
        pthread_mutex_lock(&sim.lock);
        /* A flip due by this vblank is what the hardware latched, even if
         * the compositor has not read its event yet. */
        if (sim.flip_pending && sim.flip_at <= vblank) {
            show(sim.queued, sim.nqueued);
            sim.latched = 1;
        }
        struct kms_layer layers[KMS_MAX_PLANES];
        int nl = sim.active ? sim.nshown : 0;
        memcpy(layers, sim.shown, sizeof layers);
        int changed = 0;
        for (int i = 0; i < nl; i++)
            for (int r = 0; r < ROWS; r++) {
                start[i][r] = row_hash(&layers[i], r * SAMPLE_EVERY);
                changed |= start[i][r] != last[i][r];
                last[i][r] = start[i][r];
            }
        pthread_mutex_unlock(&sim.lock);
        int torn = 0;
        for (int r = 0; r < ROWS && nl; r++) {
            sleep_until(vblank + sim.period * (uint64_t)(r * SAMPLE_EVERY) / 2340);
            pthread_mutex_lock(&sim.lock);
            for (int i = 0; i < nl; i++)
                if (row_hash(&layers[i], r * SAMPLE_EVERY) != start[i][r])
                    torn = 1;
            pthread_mutex_unlock(&sim.lock);
        }
        if (frames && nl && (changed || torn)) {
            fprintf(frames, "%.6f%s%s\n", vblank / 1e9, changed ? " new" : "", torn ? " torn" : "");
            fflush(frames);
        }
        if (nl) {
            sim.refreshes++;
            sim.new_frames += changed;
            sim.torn += torn;
        }
        if (stats && now_ns() >= next_report) {
            next_report += 1000000000ull;
            fprintf(stats, "%.3f refreshes %llu new %llu torn %llu\n", now_ns() / 1e9,
                    (unsigned long long)(sim.refreshes - r0),
                    (unsigned long long)(sim.new_frames - n0),
                    (unsigned long long)(sim.torn - t0));
            fflush(stats);
            r0 = sim.refreshes, n0 = sim.new_frames, t0 = sim.torn;
        }
    }
    return NULL;
}

int kms_open(struct kms *k, const char *path, int want_hz) {
    (void)path, (void)want_hz;
    memset(k, 0, sizeof *k);
    k->fd = timerfd_create(CLOCK_MONOTONIC, TFD_CLOEXEC | TFD_NONBLOCK);
    if (k->fd < 0)
        return -errno;
    /* 1080x2340 at 120.03 Hz, the phone's panel. */
    k->mode.hdisplay = 1080;
    k->mode.vdisplay = 2340;
    k->mode.htotal = 1160;
    k->mode.vtotal = 2370;
    k->mode.clock = 330000;
    k->nplanes = KMS_MAX_PLANES;
    for (int i = 0; i < k->nplanes; i++) {
        k->planes[i].id = 54 + (uint32_t)i;
        k->planes[i].scaler = i == 1 || i == 3 || i == 7;
    }
    k->planes[0].primary = 1;
    k->primary = 0;
    k->plane_id = k->planes[0].id;
    k->nscalers = 3;
    sim.period = 1000000000ull * 1160 * 2370 / 330000000ull;
    sim.t0 = now_ns();
    sim.next_fb = 1;
    pthread_t t;
    pthread_create(&t, NULL, scanout, NULL);
    pthread_detach(t);
    fprintf(stderr, "sim: panel 1080x2340, %.3f ms refresh\n", sim.period / 1e6);
    return 0;
}

void kms_close(struct kms *k) { close(k->fd); }

int kms_add_fb_offset(struct kms *k, int dmabuf_fd, uint32_t width, uint32_t height,
                      uint32_t pitch, uint32_t offset, uint32_t fourcc, uint32_t *fb_id) {
    (void)k, (void)fourcc;
    size_t len = (size_t)offset + (size_t)pitch * height;
    void *m = mmap(NULL, len, PROT_READ, MAP_SHARED, dmabuf_fd, 0);
    if (m == MAP_FAILED)
        return -errno;
    pthread_mutex_lock(&sim.lock);
    struct fb *f = fb_find(0);
    if (!f) {
        pthread_mutex_unlock(&sim.lock);
        munmap(m, len);
        return -ENOSPC;
    }
    *f = (struct fb){sim.next_fb++, m, len, width, height, pitch, offset};
    *fb_id = f->id;
    pthread_mutex_unlock(&sim.lock);
    return 0;
}

int kms_add_fb(struct kms *k, int dmabuf_fd, uint32_t width, uint32_t height,
               uint32_t pitch, uint32_t fourcc, uint32_t *fb_id) {
    return kms_add_fb_offset(k, dmabuf_fd, width, height, pitch, 0, fourcc, fb_id);
}

void kms_rm_fb(struct kms *k, uint32_t fb_id) {
    (void)k;
    pthread_mutex_lock(&sim.lock);
    struct fb *f = fb_find(fb_id);
    if (f) {
        munmap(f->map, f->len);
        memset(f, 0, sizeof *f);
    }
    pthread_mutex_unlock(&sim.lock);
}

void kms_probe_scalers(struct kms *k, uint32_t fb) { (void)k, (void)fb; }

int kms_assign_except(struct kms *k, struct kms_layer *layers, int n, unsigned busy) {
    int p = 0;
    for (int i = 0; i < n; i++) {
        while (p < k->nplanes && (busy & (1u << p)))
            p++;
        if (p >= k->nplanes)
            return -1;
        layers[i].plane = p++;
    }
    return 0;
}

int kms_assign(struct kms *k, struct kms_layer *layers, int n) {
    return kms_assign_except(k, layers, n, 0);
}

int kms_commit_layers(struct kms *k, const struct kms_layer *layers, int n, int flags,
                      uint64_t user_data) {
    if (n > KMS_MAX_PLANES)
        return -EINVAL;
    if (flags & KMS_TEST)
        return 0;
    pthread_mutex_lock(&sim.lock);
    if (!(flags & KMS_FLIP)) {
        show(layers, n);
        pthread_mutex_unlock(&sim.lock);
        return 0;
    }
    if (sim.flip_pending) {
        pthread_mutex_unlock(&sim.lock);
        return -EBUSY;
    }
    memcpy(sim.queued, layers, (size_t)n * sizeof *layers);
    sim.nqueued = n;
    sim.flip_pending = 1;
    sim.user_data = user_data;
    /* The next vblank the commit can still make (the driver's own work). */
    uint64_t n_vb = (now_ns() + 300000 - sim.t0) / sim.period + 1;
    sim.flip_at = sim.t0 + n_vb * sim.period;
    sim.sequence = (uint32_t)n_vb;
    pthread_mutex_unlock(&sim.lock);
    struct itimerspec its = {.it_value = {(time_t)(sim.flip_at / 1000000000ull),
                                          (long)(sim.flip_at % 1000000000ull)}};
    return timerfd_settime(k->fd, TFD_TIMER_ABSTIME, &its, NULL) ? -errno : 0;
}

static struct kms_layer full(struct kms *k, uint32_t fb) {
    return (struct kms_layer){.fb = fb, .sw = k->mode.hdisplay, .sh = k->mode.vdisplay,
                              .dw = k->mode.hdisplay, .dh = k->mode.vdisplay,
                              .alpha = 0xffff, .in_fence = -1, .plane = k->primary};
}

int kms_commit(struct kms *k, uint32_t fb, int in_fence, int modeset, uint64_t user_data) {
    (void)in_fence;
    struct kms_layer l = full(k, fb);
    if (modeset) {
        pthread_mutex_lock(&sim.lock);
        sim.active = 1;
        pthread_mutex_unlock(&sim.lock);
        return kms_commit_layers(k, &l, 1, 0, 0);
    }
    return kms_commit_layers(k, &l, 1, KMS_FLIP, user_data);
}

int kms_commit_blocking(struct kms *k, uint32_t fb) {
    struct kms_layer l = full(k, fb);
    return kms_commit_layers(k, &l, 1, 0, 0);
}

int kms_set_active(struct kms *k, int active, uint32_t fb) {
    pthread_mutex_lock(&sim.lock);
    sim.active = active;
    sim.flip_pending = 0;
    if (active) {
        struct kms_layer l = full(k, fb);
        show(&l, 1);
    }
    pthread_mutex_unlock(&sim.lock);
    return 0;
}

int kms_writeback(struct kms *k, const struct kms_layer *layers, int n, uint32_t fb) {
    (void)k, (void)layers, (void)n, (void)fb;
    return -ENOTSUP;
}

int kms_wait_flip(struct kms *k, int timeout_ms, struct kms_flip *out) {
    for (;;) {
        uint64_t expirations;
        if (read(k->fd, &expirations, sizeof expirations) == sizeof expirations) {
            pthread_mutex_lock(&sim.lock);
            int got = sim.flip_pending;
            if (got) {
                if (!sim.latched)
                    show(sim.queued, sim.nqueued);
                sim.latched = 0;
                sim.flip_pending = 0;
                out->time_ns = sim.flip_at;
                out->sequence = sim.sequence;
                out->user_data = sim.user_data;
            }
            pthread_mutex_unlock(&sim.lock);
            if (got)
                return 1;
        }
        if (timeout_ms <= 0)
            return 0;
        struct timespec z = {0, 1000000};
        nanosleep(&z, NULL);
        timeout_ms--;
    }
}
