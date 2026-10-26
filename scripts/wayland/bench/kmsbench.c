// kmsbench: what can a compositor that owns KMS on the A26 do per 120 Hz frame?
//
//   kmsbench probe                 TEST_ONLY matrix: planes, scaling, alpha, zpos, counts
//   kmsbench flip  [opts]          CPU-composited frames on the primary plane, paced by
//                                  page-flip events (the pixman-compositor model)
//   kmsbench planes [opts]         a switcher-like animation composed only by DPU planes
//                                  (background + scaled, fading cards), no CPU pixels
//   kmsbench bw                    CPU bandwidth into dumb (uncached) and heap (cached) buffers
//   kmsbench dpms [--cycles N]     screen off/on as ACTIVE=0/1 commits (what DPMS does), timed
//   kmsbench gbm                   can mesa's GBM allocate scanout buffers here (wlroots/smithay need it)
//
// Options: --seconds S, --mpx M (flip: changed Mpx per frame), --op src|over|scale,
//          --threads T, --buf dumb|system (a dma-heap name), --nbuf 2|3, --noshadow 1
//          (flip into the uncached dumb buffer directly), --cards N and --bg-mpx M
//          (planes: also redraw M Mpx of the primary every frame), --log FILE.
//
// It never changes the mode: every commit reuses the CRTC's current MODE_ID blob
// and only asks for ACTIVE=1, as DPMS-on does. It never disables the CRTC on exit.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <poll.h>
#include <pthread.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#include <linux/dma-buf.h>
#include <linux/dma-heap.h>
#include <drm_fourcc.h>
#include <gbm.h>
#include <pixman.h>
#include <xf86drm.h>
#include <xf86drmMode.h>

#define W 1080
#define H 2340
#define MAXP 16

static int fd = -1;
static uint32_t crtc_id, conn_id, mode_blob;
static int crtc_index;
static double period_ms = 8.333;
static FILE *logfp;

static double now_ms(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return ts.tv_sec * 1e3 + ts.tv_nsec / 1e6;
}

static void die(const char *m)
{
	fprintf(stderr, "kmsbench: %s: %s\n", m, strerror(errno));
	exit(1);
}

/* ---------- properties ---------- */

struct plane {
	uint32_t id;
	int type; /* 0 overlay 1 primary 2 cursor */
	uint64_t zpos_min, zpos_max, zpos;
	int has_alpha, has_blend;
	uint32_t p_fb, p_crtc, p_sx, p_sy, p_sw, p_sh, p_cx, p_cy, p_cw, p_ch, p_alpha, p_zpos, p_blend;
	uint64_t blend_premul, blend_none;
};
static struct plane planes[MAXP];
static int nplanes;
static uint32_t p_crtc_active, p_crtc_mode, p_conn_crtc;

static uint32_t prop_id(uint32_t obj, uint32_t type, const char *name, uint64_t *val, drmModePropertyPtr *out)
{
	drmModeObjectPropertiesPtr props = drmModeObjectGetProperties(fd, obj, type);
	uint32_t id = 0;
	if (!props) return 0;
	for (uint32_t i = 0; i < props->count_props; i++) {
		drmModePropertyPtr p = drmModeGetProperty(fd, props->props[i]);
		if (!p) continue;
		if (!strcmp(p->name, name)) {
			id = p->prop_id;
			if (val) *val = props->prop_values[i];
			if (out) { *out = p; break; }
		}
		drmModeFreeProperty(p);
		if (id) break;
	}
	drmModeFreeObjectProperties(props);
	return id;
}

static void init_kms(void)
{
	fd = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
	if (fd < 0) die("open card0");
	if (drmSetClientCap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1) ||
	    drmSetClientCap(fd, DRM_CLIENT_CAP_ATOMIC, 1))
		die("client caps");
	if (drmIsMaster(fd) == 0) fprintf(stderr, "kmsbench: warning: not DRM master (is Xorg still running?)\n");

	drmModeResPtr res = drmModeGetResources(fd);
	if (!res) die("resources");
	for (int i = 0; i < res->count_connectors && !conn_id; i++) {
		drmModeConnectorPtr c = drmModeGetConnector(fd, res->connectors[i]);
		if (c && c->connector_type == DRM_MODE_CONNECTOR_DSI) conn_id = c->connector_id;
		drmModeFreeConnector(c);
	}
	if (!conn_id) die("no DSI connector");
	uint64_t v = 0;
	p_conn_crtc = prop_id(conn_id, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID", &v, NULL);
	crtc_id = (uint32_t)v;
	if (!crtc_id) crtc_id = res->crtcs[0];
	for (int i = 0; i < res->count_crtcs; i++)
		if (res->crtcs[i] == crtc_id) crtc_index = i;
	v = 0;
	p_crtc_mode = prop_id(crtc_id, DRM_MODE_OBJECT_CRTC, "MODE_ID", &v, NULL);
	mode_blob = (uint32_t)v;
	p_crtc_active = prop_id(crtc_id, DRM_MODE_OBJECT_CRTC, "ACTIVE", NULL, NULL);
	if (!mode_blob) {
		errno = ENOENT;
		die("CRTC has no MODE_ID (refusing to choose a mode on this firmware)");
	}
	drmModePropertyBlobPtr b = drmModeGetPropertyBlob(fd, mode_blob);
	if (b && b->length >= sizeof(drmModeModeInfo)) {
		drmModeModeInfo *m = b->data;
		period_ms = 1000.0 / m->vrefresh;
		fprintf(stderr, "kmsbench: crtc %u mode %s %u Hz, connector %u\n", crtc_id, m->name, m->vrefresh, conn_id);
	}
	drmModeFreePropertyBlob(b);

	drmModePlaneResPtr pr = drmModeGetPlaneResources(fd);
	for (uint32_t i = 0; i < pr->count_planes && nplanes < MAXP; i++) {
		drmModePlanePtr p = drmModeGetPlane(fd, pr->planes[i]);
		if (!(p->possible_crtcs & (1u << crtc_index))) { drmModeFreePlane(p); continue; }
		struct plane *P = &planes[nplanes++];
		P->id = p->plane_id;
		uint64_t t = 0;
		prop_id(P->id, DRM_MODE_OBJECT_PLANE, "type", &t, NULL);
		P->type = t == DRM_PLANE_TYPE_PRIMARY ? 1 : t == DRM_PLANE_TYPE_CURSOR ? 2 : 0;
		P->p_fb = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "FB_ID", NULL, NULL);
		P->p_crtc = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "CRTC_ID", NULL, NULL);
		P->p_sx = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "SRC_X", NULL, NULL);
		P->p_sy = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "SRC_Y", NULL, NULL);
		P->p_sw = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "SRC_W", NULL, NULL);
		P->p_sh = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "SRC_H", NULL, NULL);
		P->p_cx = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "CRTC_X", NULL, NULL);
		P->p_cy = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "CRTC_Y", NULL, NULL);
		P->p_cw = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "CRTC_W", NULL, NULL);
		P->p_ch = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "CRTC_H", NULL, NULL);
		P->p_alpha = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "alpha", NULL, NULL);
		P->has_alpha = P->p_alpha != 0;
		drmModePropertyPtr zp = NULL;
		P->p_zpos = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "zpos", &P->zpos, &zp);
		if (zp) {
			if (zp->count_values >= 2) { P->zpos_min = zp->values[0]; P->zpos_max = zp->values[1]; }
			drmModeFreeProperty(zp);
		}
		drmModePropertyPtr bp = NULL;
		P->p_blend = prop_id(P->id, DRM_MODE_OBJECT_PLANE, "pixel blend mode", NULL, &bp);
		if (bp) {
			for (int e = 0; e < bp->count_enums; e++) {
				if (!strcmp(bp->enums[e].name, "Pre-multiplied")) P->blend_premul = bp->enums[e].value;
				if (!strcmp(bp->enums[e].name, "None")) P->blend_none = bp->enums[e].value;
			}
			P->has_blend = 1;
			drmModeFreeProperty(bp);
		}
		drmModeFreePlane(p);
	}
	drmModeFreePlaneResources(pr);
	drmModeFreeResources(res);
	/* Planes in zpos order. */
	for (int i = 0; i < nplanes; i++)
		for (int j = i + 1; j < nplanes; j++)
			if (planes[j].zpos < planes[i].zpos) {
				struct plane t = planes[i]; planes[i] = planes[j]; planes[j] = t;
			}
}

/* ---------- buffers ---------- */

struct buf {
	uint32_t fb, handle, pitch;
	int dmabuf; /* heap buffers: fd for DMA_BUF_IOCTL_SYNC */
	uint32_t *map;
	size_t size;
	pixman_image_t *img;
};

static void buf_dumb(struct buf *b, int w, int h, uint32_t fmt)
{
	struct drm_mode_create_dumb c = { .width = w, .height = h, .bpp = 32 };
	if (drmIoctl(fd, DRM_IOCTL_MODE_CREATE_DUMB, &c)) die("create dumb");
	struct drm_mode_map_dumb m = { .handle = c.handle };
	if (drmIoctl(fd, DRM_IOCTL_MODE_MAP_DUMB, &m)) die("map dumb");
	b->map = mmap(NULL, c.size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, m.offset);
	if (b->map == MAP_FAILED) die("mmap dumb");
	b->handle = c.handle; b->pitch = c.pitch; b->size = c.size; b->dmabuf = -1;
	uint32_t hs[4] = { c.handle }, ps[4] = { c.pitch }, os[4] = { 0 };
	if (drmModeAddFB2(fd, w, h, fmt, hs, ps, os, &b->fb, 0)) die("addfb2 dumb");
	b->img = pixman_image_create_bits(fmt == DRM_FORMAT_ARGB8888 ? PIXMAN_a8r8g8b8 : PIXMAN_x8r8g8b8,
					  w, h, b->map, b->pitch);
}

static void buf_heap(struct buf *b, int w, int h, uint32_t fmt, const char *heap)
{
	char path[64];
	snprintf(path, sizeof path, "/dev/dma_heap/%s", heap);
	int hfd = open(path, O_RDWR | O_CLOEXEC);
	if (hfd < 0) die(path);
	b->pitch = w * 4;
	b->size = (size_t)b->pitch * h;
	b->size = (b->size + 4095) & ~(size_t)4095;
	struct dma_heap_allocation_data a = { .len = b->size, .fd_flags = O_RDWR | O_CLOEXEC };
	if (ioctl(hfd, DMA_HEAP_IOCTL_ALLOC, &a)) die("heap alloc");
	close(hfd);
	b->dmabuf = a.fd;
	b->map = mmap(NULL, b->size, PROT_READ | PROT_WRITE, MAP_SHARED, b->dmabuf, 0);
	if (b->map == MAP_FAILED) die("mmap heap");
	if (drmPrimeFDToHandle(fd, b->dmabuf, &b->handle)) die("prime import");
	uint32_t hs[4] = { b->handle }, ps[4] = { b->pitch }, os[4] = { 0 };
	if (drmModeAddFB2(fd, w, h, fmt, hs, ps, os, &b->fb, 0)) die("addfb2 heap");
	b->img = pixman_image_create_bits(fmt == DRM_FORMAT_ARGB8888 ? PIXMAN_a8r8g8b8 : PIXMAN_x8r8g8b8,
					  w, h, b->map, b->pitch);
}

static void heap_sync(struct buf *b, int start)
{
	if (b->dmabuf < 0) return;
	struct dma_buf_sync s = { .flags = (start ? DMA_BUF_SYNC_START : DMA_BUF_SYNC_END) | DMA_BUF_SYNC_RW };
	ioctl(b->dmabuf, DMA_BUF_IOCTL_SYNC, &s);
}

/* Something that looks like content: a diagonal gradient with soft bands. */
static void paint_pattern(uint32_t *p, int stride_px, int w, int h, int seed, int alpha)
{
	for (int y = 0; y < h; y++)
		for (int x = 0; x < w; x++) {
			int r = (x * 255 / w + seed * 37) & 255;
			int g = (y * 255 / h + seed * 91) & 255;
			int bl = ((x + y) / 8 + seed * 53) & 255;
			if (((x / 90) + (y / 90) + seed) % 7 == 0) { r = 250; g = 250; bl = 250; }
			uint32_t a = alpha;
			p[y * stride_px + x] = a << 24 | (r * a / 255) << 16 | (g * a / 255) << 8 | (bl * a / 255);
		}
}

/* ---------- atomic helpers ---------- */

static void plane_set(drmModeAtomicReqPtr r, struct plane *P, uint32_t fb, int sx, int sy, int sw, int sh,
		      int cx, int cy, int cw, int ch, int alpha16)
{
	drmModeAtomicAddProperty(r, P->id, P->p_fb, fb);
	drmModeAtomicAddProperty(r, P->id, P->p_crtc, fb ? crtc_id : 0);
	drmModeAtomicAddProperty(r, P->id, P->p_sx, (uint64_t)sx << 16);
	drmModeAtomicAddProperty(r, P->id, P->p_sy, (uint64_t)sy << 16);
	drmModeAtomicAddProperty(r, P->id, P->p_sw, (uint64_t)sw << 16);
	drmModeAtomicAddProperty(r, P->id, P->p_sh, (uint64_t)sh << 16);
	drmModeAtomicAddProperty(r, P->id, P->p_cx, (uint64_t)(int64_t)cx);
	drmModeAtomicAddProperty(r, P->id, P->p_cy, (uint64_t)(int64_t)cy);
	drmModeAtomicAddProperty(r, P->id, P->p_cw, cw);
	drmModeAtomicAddProperty(r, P->id, P->p_ch, ch);
	if (P->has_alpha && alpha16 >= 0) drmModeAtomicAddProperty(r, P->id, P->p_alpha, alpha16);
}

static void plane_off(drmModeAtomicReqPtr r, struct plane *P)
{
	drmModeAtomicAddProperty(r, P->id, P->p_fb, 0);
	drmModeAtomicAddProperty(r, P->id, P->p_crtc, 0);
}

static void crtc_on(drmModeAtomicReqPtr r)
{
	drmModeAtomicAddProperty(r, crtc_id, p_crtc_active, 1);
	drmModeAtomicAddProperty(r, crtc_id, p_crtc_mode, mode_blob);
	drmModeAtomicAddProperty(r, conn_id, p_conn_crtc, crtc_id);
}

static struct plane *primary(void)
{
	for (int i = 0; i < nplanes; i++)
		if (planes[i].type == 1 && planes[i].zpos == 0) return &planes[i];
	return &planes[0];
}

/* First commit: everything off but the primary, CRTC active (DPMS-on style). */
static void initial_commit(uint32_t fb)
{
	drmModeAtomicReqPtr r = drmModeAtomicAlloc();
	crtc_on(r);
	for (int i = 0; i < nplanes; i++) {
		if (&planes[i] == primary()) plane_set(r, &planes[i], fb, 0, 0, W, H, 0, 0, W, H, 0xffff);
		else plane_off(r, &planes[i]);
	}
	int ret = drmModeAtomicCommit(fd, r, DRM_MODE_ATOMIC_ALLOW_MODESET, NULL);
	drmModeAtomicFree(r);
	if (ret) die("initial commit (ACTIVE=1, current mode)");
}

static double last_event_ms;
static int flip_pending;
static void on_flip(int f, unsigned seq, unsigned sec, unsigned usec, unsigned crtc, void *data)
{
	(void)f; (void)seq; (void)crtc; (void)data;
	last_event_ms = sec * 1e3 + usec / 1e3;
	flip_pending = 0;
}

static void wait_flip(void)
{
	drmEventContext ev = { .version = 3, .page_flip_handler2 = on_flip };
	while (flip_pending) {
		struct pollfd p = { .fd = fd, .events = POLLIN };
		int n = poll(&p, 1, 1000);
		if (n <= 0) { fprintf(stderr, "kmsbench: flip event timeout\n"); exit(2); }
		drmHandleEvent(fd, &ev);
	}
}

/* ---------- statistics ---------- */

struct stats {
	double *iv; int n, cap;
	double render_sum, render_max, commit_sum, commit_max;
};

static void stats_add(struct stats *s, double iv)
{
	if (s->n == s->cap) { s->cap = s->cap ? s->cap * 2 : 1024; s->iv = realloc(s->iv, s->cap * sizeof(double)); }
	s->iv[s->n++] = iv;
}

static int cmpd(const void *a, const void *b)
{
	double x = *(const double *)a, y = *(const double *)b;
	return x < y ? -1 : x > y;
}

static void stats_print(const char *label, struct stats *s, int frames)
{
	if (!s->n) return;
	double sum = 0; int missed = 0;
	for (int i = 0; i < s->n; i++) {
		sum += s->iv[i];
		missed += (int)floor(s->iv[i] / period_ms + 0.5) - 1;
	}
	qsort(s->iv, s->n, sizeof(double), cmpd);
	printf("%s: frames=%d interval mean=%.2f p50=%.2f p95=%.2f max=%.2f ms, missed refreshes=%d (%.1f%%), "
	       "render mean=%.2f max=%.2f ms, commit ioctl mean=%.3f max=%.3f ms, effective=%.1f fps\n",
	       label, frames, sum / s->n, s->iv[s->n / 2], s->iv[(int)(s->n * 0.95)], s->iv[s->n - 1],
	       missed, 100.0 * missed / (s->n + missed), s->render_sum / frames, s->render_max,
	       s->commit_sum / frames, s->commit_max, 1000.0 * s->n / sum);
	fflush(stdout);
}

/* ---------- threaded pixman ---------- */

struct job {
	pixman_op_t op;
	pixman_image_t *src, *dst;
	int x, y, w, h; /* dst rect; src sampled at the same coords (transform applies) */
};
static int nthreads = 1;
static pthread_t thr[8];
static pthread_barrier_t bar_go, bar_done;
static struct job cur_job;
static int quit_threads;

static void run_slice(int i)
{
	struct job *j = &cur_job;
	int y0 = j->y + j->h * i / nthreads, y1 = j->y + j->h * (i + 1) / nthreads;
	if (y1 > y0)
		pixman_image_composite32(j->op, j->src, NULL, j->dst, j->x, y0, 0, 0, j->x, y0, j->w, y1 - y0);
}

static void *worker(void *arg)
{
	int i = (int)(intptr_t)arg;
	for (;;) {
		pthread_barrier_wait(&bar_go);
		if (quit_threads) return NULL;
		run_slice(i);
		pthread_barrier_wait(&bar_done);
	}
}

static void composite(pixman_op_t op, pixman_image_t *src, pixman_image_t *dst, int x, int y, int w, int h)
{
	cur_job = (struct job){ op, src, dst, x, y, w, h };
	if (nthreads == 1) { run_slice(0); return; }
	pthread_barrier_wait(&bar_go);
	run_slice(0);
	pthread_barrier_wait(&bar_done);
}

static void threads_start(void)
{
	if (nthreads <= 1) return;
	pthread_barrier_init(&bar_go, NULL, nthreads);
	pthread_barrier_init(&bar_done, NULL, nthreads);
	for (int i = 1; i < nthreads; i++) pthread_create(&thr[i], NULL, worker, (void *)(intptr_t)i);
}

/* ---------- modes ---------- */

static double opt_seconds = 10, opt_mpx = 1.0;
static const char *opt_op = "scale", *opt_buf = "dumb";
static int opt_nbuf = 2, opt_cards = 2, opt_noshadow;
static double opt_bg_mpx;


static int cmd_flip(void)
{
	struct buf bufs[3];
	for (int i = 0; i < opt_nbuf; i++) {
		if (!strcmp(opt_buf, "dumb")) buf_dumb(&bufs[i], W, H, DRM_FORMAT_XRGB8888);
		else buf_heap(&bufs[i], W, H, DRM_FORMAT_XRGB8888, opt_buf);
		memset(bufs[i].map, 0x20, bufs[i].size);
	}
	/* Dumb buffers are write-combined: compose into a cached shadow and copy the
	 * damage out (what weston's pixman renderer and Xorg's ShadowFB do). Heap
	 * buffers are cached: compose in place, then flush with DMA_BUF_SYNC_END. */
	int shadow = !strcmp(opt_buf, "dumb") && !opt_noshadow;
	uint32_t *sh_px = NULL;
	pixman_image_t *sh = NULL;
	if (shadow) {
		sh_px = aligned_alloc(64, (size_t)W * H * 4);
		memset(sh_px, 0x20, (size_t)W * H * 4);
		sh = pixman_image_create_bits(PIXMAN_x8r8g8b8, W, H, sh_px, W * 4);
	}
	/* Source: an "app snapshot", pre-rendered once, sampled scaled+bilinear like a card. */
	uint32_t *src_px = aligned_alloc(64, (size_t)W * H * 4);
	paint_pattern(src_px, W, W, H, 1, 255);
	pixman_image_t *src = pixman_image_create_bits(PIXMAN_a8r8g8b8, W, H, src_px, W * 4);
	pixman_op_t op = PIXMAN_OP_SRC;
	if (!strcmp(opt_op, "over")) {
		paint_pattern(src_px, W, W, H, 2, 200);
		op = PIXMAN_OP_OVER;
	} else if (!strcmp(opt_op, "scale")) {
		pixman_transform_t t;
		pixman_transform_init_scale(&t, pixman_double_to_fixed(1.0 / 0.93), pixman_double_to_fixed(1.0 / 0.93));
		pixman_image_set_transform(src, &t);
		pixman_image_set_filter(src, PIXMAN_FILTER_BILINEAR, NULL, 0);
		pixman_image_set_repeat(src, PIXMAN_REPEAT_PAD);
	}
	threads_start();

	int band = (int)(opt_mpx * 1e6 / W);
	if (band > H) band = H;
	if (band < 1) band = 1;
	initial_commit(bufs[0].fb);
	struct plane *P = primary();
	int ydmg[3] = { 0, 0, 0 }; /* damage band position rendered into each buffer last time */
	int hist[3] = { -1, -1, -1 };
	struct stats st = { 0 };
	double t_end = now_ms() + opt_seconds * 1e3, prev = 0;
	int frames = 0;
	while (now_ms() < t_end) {
		int bi = frames % opt_nbuf;
		struct buf *b = &bufs[bi];
		int y = (frames * 37) % (H - band + 1);
		double t0 = now_ms();
		pixman_image_t *dst = shadow ? sh : b->img;
		heap_sync(b, 1);
		composite(op, src, dst, 0, y, W, band);
		if (shadow) {
			/* Copy this frame's band and the bands the buffer has not seen yet (buffer age). */
			composite(PIXMAN_OP_SRC, sh, b->img, 0, y, W, band);
			for (int k = 1; k < opt_nbuf; k++) {
				int prevb = (bi + opt_nbuf - k) % opt_nbuf;
				if (hist[prevb] >= 0) composite(PIXMAN_OP_SRC, sh, b->img, 0, ydmg[prevb], W, band);
			}
		} else {
			for (int k = 1; k < opt_nbuf; k++) {
				int prevb = (bi + opt_nbuf - k) % opt_nbuf;
				if (hist[prevb] >= 0)
					composite(PIXMAN_OP_SRC, bufs[(bi + opt_nbuf - 1) % opt_nbuf].img, b->img, 0, ydmg[prevb], W, band);
			}
		}
		heap_sync(b, 0);
		ydmg[bi] = y; hist[bi] = frames;
		double t1 = now_ms();
		st.render_sum += t1 - t0;
		if (t1 - t0 > st.render_max) st.render_max = t1 - t0;
		drmModeAtomicReqPtr r = drmModeAtomicAlloc();
		drmModeAtomicAddProperty(r, P->id, P->p_fb, b->fb);
		flip_pending = 1;
		double c0 = now_ms();
		if (drmModeAtomicCommit(fd, r, DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_PAGE_FLIP_EVENT, NULL)) die("commit");
		double c1 = now_ms() - c0;
		st.commit_sum += c1;
		if (c1 > st.commit_max) st.commit_max = c1;
		drmModeAtomicFree(r);
		wait_flip();
		if (prev > 0) stats_add(&st, last_event_ms - prev);
		if (logfp) fprintf(logfp, "%d %.3f %.3f %.3f\n", frames, last_event_ms, t1 - t0, last_event_ms - t0);
		prev = last_event_ms;
		frames++;
	}
	char label[160];
	snprintf(label, sizeof label, "flip op=%s mpx=%.2f (band %d rows) buf=%s nbuf=%d threads=%d", opt_op,
		 opt_mpx, band, opt_buf, opt_nbuf, nthreads);
	stats_print(label, &st, frames);
	return 0;
}

/* Hardware-composed switcher: wallpaper on the primary plane, cards (full app
 * snapshots) on scaling planes that shrink, slide and fade; the CPU only fills
 * in plane properties. */
static int cmd_planes(void)
{
	struct buf bgs[2], cards[6];
	int nbg = opt_bg_mpx > 0 ? 2 : 1;
	for (int i = 0; i < nbg; i++) {
		buf_dumb(&bgs[i], W, H, DRM_FORMAT_XRGB8888);
		paint_pattern(bgs[i].map, bgs[i].pitch / 4, W, H, 5, 255);
	}
	/* --mpx with planes: the primary is also a renderer layer whose band is redrawn
	 * every frame (SRC, bilinear, write-only into the uncached buffer). */
	uint32_t *src_px = aligned_alloc(64, (size_t)W * H * 4);
	paint_pattern(src_px, W, W, H, 3, 255);
	pixman_image_t *src = pixman_image_create_bits(PIXMAN_x8r8g8b8, W, H, src_px, W * 4);
	pixman_transform_t tr;
	pixman_transform_init_scale(&tr, pixman_double_to_fixed(1.0 / 0.93), pixman_double_to_fixed(1.0 / 0.93));
	pixman_image_set_transform(src, &tr);
	pixman_image_set_filter(src, PIXMAN_FILTER_BILINEAR, NULL, 0);
	pixman_image_set_repeat(src, PIXMAN_REPEAT_PAD);
	threads_start();
	int band = (int)(opt_bg_mpx * 1e6 / W);
	if (band > H) band = H;
	struct plane *pri = primary();
	int ylast[2] = { -1, -1 };
	initial_commit(bgs[0].fb);
	/* Ask the kernel which planes scale, and use those first. */
	struct plane *cand[MAXP];
	int ncand = 0, cscal[MAXP];
	struct buf tb;
	buf_dumb(&tb, W, H, DRM_FORMAT_ARGB8888);
	for (int pass = 0; pass < 2; pass++)
		for (int i = 0; i < nplanes; i++) {
			if (&planes[i] == primary()) continue;
			if (pass == 0) {
				drmModeAtomicReqPtr r = drmModeAtomicAlloc();
				plane_set(r, &planes[i], tb.fb, 0, 0, W, H, 100, 200, W * 6 / 10, H * 6 / 10, 0xffff);
				cscal[i] = drmModeAtomicCommit(fd, r, DRM_MODE_ATOMIC_TEST_ONLY, NULL) == 0;
				drmModeAtomicFree(r);
				if (cscal[i]) cand[ncand++] = &planes[i];
			} else if (!cscal[i]) {
				cand[ncand++] = &planes[i];
			}
		}
	int nc = 0, scal[6];
	struct plane *cp[6];
	for (int k = 0; k < ncand && nc < opt_cards; k++) {
		cp[nc] = cand[k];
		scal[nc] = cscal[cand[k] - planes];
		if (!strcmp(opt_buf, "dumb")) buf_dumb(&cards[nc], W, H, DRM_FORMAT_ARGB8888);
		else buf_heap(&cards[nc], W, H, DRM_FORMAT_ARGB8888, opt_buf);
		heap_sync(&cards[nc], 1);
		paint_pattern(cards[nc].map, cards[nc].pitch / 4, W, H, 10 + nc, 255);
		heap_sync(&cards[nc], 0);
		nc++;
	}
	struct stats st = { 0 };
	double t_start = now_ms(), t_end = t_start + opt_seconds * 1e3, prev = 0;
	int frames = 0, rejected = 0;
	while (now_ms() < t_end) {
		double t = (now_ms() - t_start) / 1000.0;
		double t0 = now_ms();
		drmModeAtomicReqPtr r = drmModeAtomicAlloc();
		if (band > 0) {
			int bi = frames & 1, y = (frames * 37) % (H - band + 1);
			composite(PIXMAN_OP_SRC, src, bgs[bi].img, 0, y, W, band);
			if (ylast[bi ^ 1] >= 0) composite(PIXMAN_OP_SRC, src, bgs[bi].img, 0, ylast[bi ^ 1], W, band);
			ylast[bi] = y;
			drmModeAtomicAddProperty(r, pri->id, pri->p_fb, bgs[bi].fb);
		}
		for (int c = 0; c < nc; c++) {
			double ph = t * 1.3 + c * 1.7;
			double s = scal[c] ? 0.62 + 0.36 * (0.5 + 0.5 * sin(ph)) : 1.0;
			int cw = (int)(W * s), ch = (int)(H * s);
			int cx = (W - cw) / 2 + (int)(260 * sin(ph * 0.7)) * (scal[c] ? 1 : 0);
			int cy = (H - ch) / 2 + (int)(200 * cos(ph * 0.9));
			/* Keep the destination on screen: clip and move the source window with it. */
			int sx = 0, sy = 0, sw = W, sh = H;
			if (cx < 0) { sx = (int)(-cx / s); sw -= sx; cw += cx; cx = 0; }
			if (cy < 0) { sy = (int)(-cy / s); sh -= sy; ch += cy; cy = 0; }
			if (cx + cw > W) { int o = cx + cw - W; cw -= o; sw -= (int)(o / s); }
			if (cy + ch > H) { int o = cy + ch - H; ch -= o; sh -= (int)(o / s); }
			int alpha = (int)(65535 * (0.55 + 0.45 * (0.5 + 0.5 * cos(ph * 1.1))));
			plane_set(r, cp[c], cards[c].fb, sx, sy, sw, sh, cx, cy, cw, ch, alpha);
		}
		double t1 = now_ms();
		st.render_sum += t1 - t0;
		if (t1 - t0 > st.render_max) st.render_max = t1 - t0;
		flip_pending = 1;
		double c0 = now_ms();
		int cret = drmModeAtomicCommit(fd, r, DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_PAGE_FLIP_EVENT, NULL);
		double c1 = now_ms() - c0;
		st.commit_sum += c1;
		if (c1 > st.commit_max) st.commit_max = c1;
		if (cret) {
			rejected++;
			flip_pending = 0;
			drmModeAtomicFree(r);
			if (rejected > 20) die("planes commit rejected repeatedly");
			continue;
		}
		drmModeAtomicFree(r);
		wait_flip();
		if (prev > 0) stats_add(&st, last_event_ms - prev);
		prev = last_event_ms;
		frames++;
	}
	char label[160];
	int ns = 0;
	for (int c = 0; c < nc; c++) ns += scal[c];
	snprintf(label, sizeof label, "planes cards=%d (scaling %d) buf=%s bg-mpx=%.2f threads=%d rejected=%d", nc, ns,
		 opt_buf, opt_bg_mpx, nthreads, rejected);
	stats_print(label, &st, frames);
	return 0;
}

static int test_commit(void (*fill)(drmModeAtomicReqPtr, void *), void *arg)
{
	drmModeAtomicReqPtr r = drmModeAtomicAlloc();
	crtc_on(r);
	fill(r, arg);
	int ret = drmModeAtomicCommit(fd, r, DRM_MODE_ATOMIC_TEST_ONLY, NULL);
	drmModeAtomicFree(r);
	return ret ? -errno : 0;
}

static struct buf pbg, pfull;
struct probe { int n; double scale; int alpha; int cx, cy; int sw, sh; int dw, dh; int which; };

static void fill_probe(drmModeAtomicReqPtr r, void *arg)
{
	struct probe *p = arg;
	struct plane *pri = primary();
	plane_set(r, pri, pbg.fb, 0, 0, W, H, 0, 0, W, H, 0xffff);
	int used = 0;
	for (int i = 0; i < nplanes; i++) {
		struct plane *P = &planes[i];
		if (P == pri) continue;
		int use = p->which >= 0 ? (i == p->which) : (used < p->n);
		if (!use) { plane_off(r, P); continue; }
		used++;
		int sw = p->sw ? p->sw : W, sh = p->sh ? p->sh : H;
		int dw = p->dw ? p->dw : (int)(sw * p->scale), dh = p->dh ? p->dh : (int)(sh * p->scale);
		plane_set(r, P, pfull.fb, 0, 0, sw, sh, p->cx, p->cy, dw, dh, p->alpha);
	}
}

static int cmd_probe(void)
{
	buf_dumb(&pbg, W, H, DRM_FORMAT_XRGB8888);
	buf_dumb(&pfull, W, H, DRM_FORMAT_ARGB8888);
	printf("planes on crtc %u (zpos order):\n", crtc_id);
	for (int i = 0; i < nplanes; i++)
		printf("  [%d] plane %u type=%s zpos=%llu (%llu..%llu) alpha=%d blend=%d\n", i, planes[i].id,
		       planes[i].type == 1 ? "primary" : planes[i].type == 2 ? "cursor" : "overlay",
		       (unsigned long long)planes[i].zpos, (unsigned long long)planes[i].zpos_min,
		       (unsigned long long)planes[i].zpos_max, planes[i].has_alpha, planes[i].has_blend);
	printf("per plane, over the full-screen primary (TEST_ONLY; 0 = accepted, else -errno):\n");
	printf("  %-6s %8s %8s %8s %8s %8s %8s %8s %8s\n", "plane", "1:1", "x0.6", "x0.5", "x0.45", "x1.5", "a=50%", "x<0", "40x10");
	for (int i = 0; i < nplanes; i++) {
		if (&planes[i] == primary()) continue;
		struct probe ps[] = {
			{ .which = i, .scale = 1, .alpha = 0xffff },
			{ .which = i, .scale = 0.6, .alpha = 0xffff, .cx = 100, .cy = 100 },
			{ .which = i, .scale = 0.5, .alpha = 0xffff, .cx = 10, .cy = 10 },
			{ .which = i, .scale = 0.45, .alpha = 0xffff, .cx = 10, .cy = 10 },
			{ .which = i, .scale = 1, .alpha = 0xffff, .sw = 540, .sh = 1170, .dw = 810, .dh = 1755 },
			{ .which = i, .scale = 1, .alpha = 0x8000 },
			{ .which = i, .scale = 0.6, .alpha = 0xffff, .cx = -100, .cy = 100 },
			{ .which = i, .scale = 1, .alpha = 0xffff, .sw = 40, .sh = 10, .cx = 500, .cy = 500 },
		};
		printf("  %-6u", planes[i].id);
		for (unsigned k = 0; k < sizeof ps / sizeof ps[0]; k++) printf(" %8d", test_commit(fill_probe, &ps[k]));
		printf("\n");
	}
	printf("N full-screen 1:1 overlays at once (alpha 50%%) over the primary:\n ");
	for (int n = 1; n < nplanes; n++) {
		struct probe p = { .n = n, .which = -1, .scale = 1, .alpha = 0x8000 };
		printf(" %d:%d", n, test_commit(fill_probe, &p));
	}
	printf("\n");
	fflush(stdout);
	return 0;
}

static double bw_run(void (*fn)(uint32_t *, uint32_t *, size_t), uint32_t *dst, uint32_t *src, size_t n)
{
	double best = 1e9;
	for (int k = 0; k < 5; k++) {
		double t0 = now_ms();
		fn(dst, src, n);
		double t = now_ms() - t0;
		if (t < best) best = t;
	}
	return best;
}
static void f_copy(uint32_t *d, uint32_t *s, size_t n) { memcpy(d, s, n * 4); }
static void f_read(uint32_t *d, uint32_t *s, size_t n) { (void)d; volatile uint32_t acc = 0; for (size_t i = 0; i < n; i += 4) acc += s[i]; }

static int cmd_bw(void)
{
	size_t n = (size_t)W * H;
	uint32_t *cached = aligned_alloc(64, n * 4);
	memset(cached, 1, n * 4);
	struct buf d, h;
	buf_dumb(&d, W, H, DRM_FORMAT_XRGB8888);
	printf("full frame (%.1f MB) copy cached->cached %.2f ms\n", n * 4 / 1e6,
	       bw_run(f_copy, aligned_alloc(64, n * 4), cached, n));
	printf("copy cached->dumb(WC) %.2f ms, read dumb %.2f ms\n", bw_run(f_copy, d.map, cached, n),
	       bw_run(f_read, NULL, d.map, n));
	const char *heaps[] = { "system", "system-uncached" };
	for (int i = 0; i < 2; i++) {
		char p[64];
		snprintf(p, sizeof p, "/dev/dma_heap/%s", heaps[i]);
		if (access(p, R_OK)) { printf("%s: absent\n", p); continue; }
		buf_heap(&h, W, H, DRM_FORMAT_XRGB8888, heaps[i]);
		double t0 = now_ms();
		heap_sync(&h, 1);
		double ts = now_ms() - t0;
		double c = bw_run(f_copy, h.map, cached, n);
		t0 = now_ms();
		heap_sync(&h, 0);
		double te = now_ms() - t0;
		printf("heap %s: copy %.2f ms, read %.2f ms, sync start %.2f ms end %.2f ms (whole buffer)\n", heaps[i], c,
		       bw_run(f_read, NULL, h.map, n), ts, te);
	}
	return 0;
}

static int opt_cycles = 5;

/* Screen off and on the way Moon's DPMS does it, from a compositor that owns
 * KMS: ACTIVE=0 keeps MODE_ID, so ACTIVE=1 brings the same mode back. */
static int cmd_dpms(void)
{
	struct buf b;
	buf_dumb(&b, W, H, DRM_FORMAT_XRGB8888);
	paint_pattern(b.map, b.pitch / 4, W, H, 7, 255);
	initial_commit(b.fb);
	struct plane *P = primary();
	for (int c = 0; c < opt_cycles; c++) {
		drmModeAtomicReqPtr r = drmModeAtomicAlloc();
		drmModeAtomicAddProperty(r, crtc_id, p_crtc_active, 0);
		plane_off(r, P);
		double t0 = now_ms();
		int ret = drmModeAtomicCommit(fd, r, DRM_MODE_ATOMIC_ALLOW_MODESET, NULL);
		double toff = now_ms() - t0;
		drmModeAtomicFree(r);
		if (ret) die("ACTIVE=0");
		usleep(1500 * 1000);
		r = drmModeAtomicAlloc();
		drmModeAtomicAddProperty(r, crtc_id, p_crtc_active, 1);
		plane_set(r, P, b.fb, 0, 0, W, H, 0, 0, W, H, 0xffff);
		t0 = now_ms();
		ret = drmModeAtomicCommit(fd, r, DRM_MODE_ATOMIC_ALLOW_MODESET, NULL);
		double ton = now_ms() - t0;
		drmModeAtomicFree(r);
		if (ret) die("ACTIVE=1");
		/* First flip after power-up: the panel is really showing frames again. */
		r = drmModeAtomicAlloc();
		drmModeAtomicAddProperty(r, P->id, P->p_fb, b.fb);
		flip_pending = 1;
		if (drmModeAtomicCommit(fd, r, DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_PAGE_FLIP_EVENT, NULL)) die("flip after on");
		drmModeAtomicFree(r);
		wait_flip();
		printf("dpms cycle %d: off commit %.1f ms, on commit %.1f ms, first flip %.1f ms after on\n", c + 1, toff,
		       ton, now_ms() - t0);
		fflush(stdout);
		usleep(500 * 1000);
	}
	return 0;
}

static int cmd_gbm(void)
{
	struct gbm_device *g = gbm_create_device(fd);
	if (!g) { printf("gbm_create_device failed\n"); return 1; }
	printf("gbm backend: %s\n", gbm_device_get_backend_name(g));
	struct gbm_bo *bo = gbm_bo_create(g, W, H, GBM_FORMAT_XRGB8888, GBM_BO_USE_SCANOUT | GBM_BO_USE_LINEAR);
	if (!bo) bo = gbm_bo_create(g, W, H, GBM_FORMAT_XRGB8888, GBM_BO_USE_SCANOUT);
	if (!bo) { printf("gbm_bo_create(scanout) failed: %s\n", strerror(errno)); return 1; }
	uint32_t hs[4] = { gbm_bo_get_handle(bo).u32 }, ps[4] = { gbm_bo_get_stride(bo) }, os[4] = { 0 }, fb = 0;
	int r = drmModeAddFB2(fd, W, H, DRM_FORMAT_XRGB8888, hs, ps, os, &fb, 0);
	printf("gbm bo %ux%u stride %u modifier 0x%llx, addfb2 %s\n", gbm_bo_get_width(bo), gbm_bo_get_height(bo),
	       gbm_bo_get_stride(bo), (unsigned long long)gbm_bo_get_modifier(bo), r ? strerror(errno) : "ok");
	return 0;
}

int main(int argc, char **argv)
{
	if (argc < 2) {
		fprintf(stderr, "usage: kmsbench probe|flip|planes|bw|gbm|dpms [--seconds S] [--mpx M] [--op src|over|scale] "
				"[--threads T] [--buf dumb|system] [--nbuf N] [--cards N] [--noshadow 1] [--bg-mpx M] [--log FILE]\n");
		return 2;
	}
	for (int i = 2; i + 1 < argc; i += 2) {
		if (!strcmp(argv[i], "--seconds")) opt_seconds = atof(argv[i + 1]);
		else if (!strcmp(argv[i], "--mpx")) opt_mpx = atof(argv[i + 1]);
		else if (!strcmp(argv[i], "--op")) opt_op = argv[i + 1];
		else if (!strcmp(argv[i], "--threads")) nthreads = atoi(argv[i + 1]);
		else if (!strcmp(argv[i], "--buf")) opt_buf = argv[i + 1];
		else if (!strcmp(argv[i], "--nbuf")) opt_nbuf = atoi(argv[i + 1]);
		else if (!strcmp(argv[i], "--cards")) opt_cards = atoi(argv[i + 1]);
		else if (!strcmp(argv[i], "--log")) logfp = fopen(argv[i + 1], "w");
		else if (!strcmp(argv[i], "--noshadow")) opt_noshadow = atoi(argv[i + 1]);
		else if (!strcmp(argv[i], "--bg-mpx")) opt_bg_mpx = atof(argv[i + 1]);
		else if (!strcmp(argv[i], "--cycles")) opt_cycles = atoi(argv[i + 1]);
	}
	if (nthreads < 1) nthreads = 1;
	if (nthreads > 8) nthreads = 8;
	if (opt_nbuf < 2) opt_nbuf = 2;
	if (opt_nbuf > 3) opt_nbuf = 3;
	if (opt_cards > 6) opt_cards = 6;
	init_kms();
	int rc = 2;
	if (!strcmp(argv[1], "probe")) rc = cmd_probe();
	else if (!strcmp(argv[1], "flip")) rc = cmd_flip();
	else if (!strcmp(argv[1], "planes")) rc = cmd_planes();
	else if (!strcmp(argv[1], "bw")) rc = cmd_bw();
	else if (!strcmp(argv[1], "gbm")) rc = cmd_gbm();
	else if (!strcmp(argv[1], "dpms")) rc = cmd_dpms();
	if (logfp) fclose(logfp);
	/* Leave the CRTC as it is: never disable it on this firmware. */
	return rc;
}
