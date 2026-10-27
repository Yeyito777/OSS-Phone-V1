// wlbench: a full-screen Wayland client that redraws M Mpx per frame and measures
// when the compositor really put each frame on the panel (wp_presentation).
//
//   wlbench [--mpx M] [--seconds S] [--op src|scale] [--dmabuf system|dumb]
//
// Frames are driven by wl_surface.frame callbacks, like an animating toolkit.
// With --dmabuf the buffers are linux-dmabuf (a cached "system" dma-heap buffer,
// or a DRM dumb buffer), which a compositor can put on a plane without copying.

#define _GNU_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <time.h>
#include <unistd.h>

#include <drm_fourcc.h>
#include <linux/dma-buf.h>
#include <linux/dma-heap.h>
#include <pixman.h>
#include <wayland-client.h>
#include <xf86drm.h>

#include "linux-dmabuf-v1-client-protocol.h"
#include "presentation-time-client-protocol.h"
#include "xdg-shell-client-protocol.h"

static struct wl_compositor *compositor;
static struct wl_shm *shm;
static struct xdg_wm_base *wm;
static struct wp_presentation *presentation;
static struct zwp_linux_dmabuf_v1 *dmabuf_global;
static struct wl_surface *surface;
static int width, height, configured, running = 1;
static double opt_mpx = 1.0, opt_seconds = 10;
static const char *opt_op = "scale", *opt_dmabuf;

static double now_ms(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return ts.tv_sec * 1e3 + ts.tv_nsec / 1e6;
}

struct buffer {
	struct wl_buffer *wl;
	uint32_t *px;
	size_t size;
	int stride, busy, dmabuf_fd;
	pixman_image_t *img;
	int age_y[3];
};
static struct buffer bufs[3];
static int nbufs = 3;

static void buf_release(void *d, struct wl_buffer *b) { (void)b; ((struct buffer *)d)->busy = 0; }
static const struct wl_buffer_listener buf_listener = { buf_release };

static void make_shm(struct buffer *b)
{
	b->stride = width * 4;
	b->size = (size_t)b->stride * height;
	int fd = memfd_create("wlbench", MFD_CLOEXEC);
	if (fd < 0 || ftruncate(fd, b->size)) exit(1);
	b->px = mmap(NULL, b->size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	struct wl_shm_pool *pool = wl_shm_create_pool(shm, fd, b->size);
	b->wl = wl_shm_pool_create_buffer(pool, 0, width, height, b->stride, WL_SHM_FORMAT_XRGB8888);
	wl_shm_pool_destroy(pool);
	close(fd);
	b->dmabuf_fd = -1;
}

static void make_dmabuf(struct buffer *b)
{
	b->stride = width * 4;
	b->size = ((size_t)b->stride * height + 4095) & ~(size_t)4095;
	int fd = -1;
	if (!strcmp(opt_dmabuf, "dumb")) {
		int card = open("/dev/dri/card0", O_RDWR | O_CLOEXEC);
		struct drm_mode_create_dumb c = { .width = width, .height = height, .bpp = 32 };
		if (card < 0 || drmIoctl(card, DRM_IOCTL_MODE_CREATE_DUMB, &c) ||
		    drmPrimeHandleToFD(card, c.handle, DRM_CLOEXEC | DRM_RDWR, &fd)) {
			perror("dumb dmabuf");
			exit(1);
		}
		b->stride = c.pitch;
	} else {
		char path[64];
		snprintf(path, sizeof path, "/dev/dma_heap/%s", opt_dmabuf);
		int h = open(path, O_RDWR | O_CLOEXEC);
		struct dma_heap_allocation_data a = { .len = b->size, .fd_flags = O_RDWR | O_CLOEXEC };
		if (h < 0 || ioctl(h, DMA_HEAP_IOCTL_ALLOC, &a)) { perror(path); exit(1); }
		close(h);
		fd = a.fd;
	}
	b->dmabuf_fd = fd;
	b->px = mmap(NULL, b->size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
	if (b->px == MAP_FAILED) { perror("mmap dmabuf"); exit(1); }
	struct zwp_linux_buffer_params_v1 *p = zwp_linux_dmabuf_v1_create_params(dmabuf_global);
	zwp_linux_buffer_params_v1_add(p, fd, 0, 0, b->stride, DRM_FORMAT_MOD_LINEAR >> 32,
				       DRM_FORMAT_MOD_LINEAR & 0xffffffff);
	b->wl = zwp_linux_buffer_params_v1_create_immed(p, width, height, DRM_FORMAT_XRGB8888, 0);
	zwp_linux_buffer_params_v1_destroy(p);
}

static void sync_dmabuf(struct buffer *b, int start)
{
	if (b->dmabuf_fd < 0) return;
	struct dma_buf_sync s = { .flags = (start ? DMA_BUF_SYNC_START : DMA_BUF_SYNC_END) | DMA_BUF_SYNC_RW };
	ioctl(b->dmabuf_fd, DMA_BUF_IOCTL_SYNC, &s);
}

/* ---- presentation statistics ---- */
static double *pres; static int npres, cap_pres, discarded, zero_copy, frames;
static double refresh_ns, render_sum, render_max, last_present;

static void fb_sync_output(void *d, struct wp_presentation_feedback *f, struct wl_output *o) { (void)d; (void)f; (void)o; }
static void fb_presented(void *d, struct wp_presentation_feedback *f, uint32_t hi, uint32_t lo, uint32_t nsec,
			 uint32_t refresh, uint32_t seq_hi, uint32_t seq_lo, uint32_t flags)
{
	(void)d; (void)seq_hi; (void)seq_lo;
	double t = ((double)(((uint64_t)hi << 32) | lo)) * 1e3 + nsec / 1e6;
	refresh_ns = refresh;
	if (flags & WP_PRESENTATION_FEEDBACK_KIND_ZERO_COPY) zero_copy++;
	if (last_present > 0) {
		if (npres == cap_pres) { cap_pres = cap_pres ? cap_pres * 2 : 1024; pres = realloc(pres, cap_pres * sizeof(double)); }
		pres[npres++] = t - last_present;
	}
	last_present = t;
	wp_presentation_feedback_destroy(f);
}
static void fb_discarded(void *d, struct wp_presentation_feedback *f) { (void)d; discarded++; wp_presentation_feedback_destroy(f); }
static const struct wp_presentation_feedback_listener fb_listener = { fb_sync_output, fb_presented, fb_discarded };

/* ---- drawing ---- */
static pixman_image_t *src;
static void draw_frame(void);
static void frame_done(void *d, struct wl_callback *cb, uint32_t t)
{
	(void)d; (void)t;
	wl_callback_destroy(cb);
	draw_frame();
}
static const struct wl_callback_listener frame_listener = { frame_done };

static int band_rows(void)
{
	int band = (int)(opt_mpx * 1e6 / width);
	return band < 1 ? 1 : band > height ? height : band;
}

static void draw_frame(void)
{
	struct buffer *b = NULL;
	for (int i = 0; i < nbufs; i++)
		if (!bufs[i].busy) { b = &bufs[i]; break; }
	if (!b) { /* all held: wait for a release, then try again on the next callback */
		struct wl_callback *cb = wl_surface_frame(surface);
		wl_callback_add_listener(cb, &frame_listener, NULL);
		wl_surface_commit(surface);
		return;
	}
	int band = band_rows();
	int y = (frames * 37) % (height - band + 1);
	double t0 = now_ms();
	sync_dmabuf(b, 1);
	pixman_image_composite32(PIXMAN_OP_SRC, src, NULL, b->img, 0, y, 0, 0, 0, y,
				 width, band);
	sync_dmabuf(b, 0);
	double t1 = now_ms();
	render_sum += t1 - t0;
	if (t1 - t0 > render_max) render_max = t1 - t0;
	wl_surface_attach(surface, b->wl, 0, 0);
	wl_surface_damage_buffer(surface, 0, y, width, band);
	struct wl_callback *cb = wl_surface_frame(surface);
	wl_callback_add_listener(cb, &frame_listener, NULL);
	if (presentation) {
		struct wp_presentation_feedback *f = wp_presentation_feedback(presentation, surface);
		wp_presentation_feedback_add_listener(f, &fb_listener, NULL);
	}
	wl_surface_commit(surface);
	b->busy = 1;
	frames++;
}

/* ---- shell ---- */
static void wm_ping(void *d, struct xdg_wm_base *w, uint32_t s) { (void)d; xdg_wm_base_pong(w, s); }
static const struct xdg_wm_base_listener wm_listener = { wm_ping };
static void xs_configure(void *d, struct xdg_surface *s, uint32_t serial) { (void)d; xdg_surface_ack_configure(s, serial); configured = 1; }
static const struct xdg_surface_listener xs_listener = { xs_configure };
static void tl_configure(void *d, struct xdg_toplevel *t, int32_t w, int32_t h, struct wl_array *st)
{
	(void)d; (void)t; (void)st;
	if (w > 0 && h > 0) { width = w; height = h; }
}
static void tl_close(void *d, struct xdg_toplevel *t) { (void)d; (void)t; running = 0; }
static void tl_bounds(void *d, struct xdg_toplevel *t, int32_t w, int32_t h) { (void)d; (void)t; (void)w; (void)h; }
static void tl_caps(void *d, struct xdg_toplevel *t, struct wl_array *c) { (void)d; (void)t; (void)c; }
static const struct xdg_toplevel_listener tl_listener = { tl_configure, tl_close, tl_bounds, tl_caps };

static void reg_global(void *d, struct wl_registry *r, uint32_t name, const char *iface, uint32_t ver)
{
	(void)d;
	if (!strcmp(iface, wl_compositor_interface.name)) compositor = wl_registry_bind(r, name, &wl_compositor_interface, 4);
	else if (!strcmp(iface, wl_shm_interface.name)) shm = wl_registry_bind(r, name, &wl_shm_interface, 1);
	else if (!strcmp(iface, xdg_wm_base_interface.name)) {
		wm = wl_registry_bind(r, name, &xdg_wm_base_interface, 1);
		xdg_wm_base_add_listener(wm, &wm_listener, NULL);
	} else if (!strcmp(iface, wp_presentation_interface.name)) presentation = wl_registry_bind(r, name, &wp_presentation_interface, 1);
	else if (!strcmp(iface, zwp_linux_dmabuf_v1_interface.name) && ver >= 3)
		dmabuf_global = wl_registry_bind(r, name, &zwp_linux_dmabuf_v1_interface, 3);
}
static void reg_remove(void *d, struct wl_registry *r, uint32_t n) { (void)d; (void)r; (void)n; }
static const struct wl_registry_listener reg_listener = { reg_global, reg_remove };

static int cmpd(const void *a, const void *b)
{
	double x = *(const double *)a, y = *(const double *)b;
	return x < y ? -1 : x > y;
}

int main(int argc, char **argv)
{
	for (int i = 1; i + 1 < argc; i += 2) {
		if (!strcmp(argv[i], "--mpx")) opt_mpx = atof(argv[i + 1]);
		else if (!strcmp(argv[i], "--seconds")) opt_seconds = atof(argv[i + 1]);
		else if (!strcmp(argv[i], "--op")) opt_op = argv[i + 1];
		else if (!strcmp(argv[i], "--dmabuf")) opt_dmabuf = argv[i + 1];
	}
	struct wl_display *dpy = wl_display_connect(NULL);
	if (!dpy) { fprintf(stderr, "wlbench: no Wayland display\n"); return 1; }
	struct wl_registry *reg = wl_display_get_registry(dpy);
	wl_registry_add_listener(reg, &reg_listener, NULL);
	wl_display_roundtrip(dpy);
	if (!compositor || !shm || !wm) { fprintf(stderr, "wlbench: missing globals\n"); return 1; }
	if (opt_dmabuf && !dmabuf_global) { fprintf(stderr, "wlbench: no linux-dmabuf v3\n"); return 1; }
	surface = wl_compositor_create_surface(compositor);
	struct xdg_surface *xs = xdg_wm_base_get_xdg_surface(wm, surface);
	xdg_surface_add_listener(xs, &xs_listener, NULL);
	struct xdg_toplevel *tl = xdg_surface_get_toplevel(xs);
	xdg_toplevel_add_listener(tl, &tl_listener, NULL);
	xdg_toplevel_set_title(tl, "wlbench");
	xdg_toplevel_set_fullscreen(tl, NULL);
	wl_surface_commit(surface);
	while (!configured) wl_display_dispatch(dpy);
	if (!width) { width = 1080; height = 2340; }
	fprintf(stderr, "wlbench: %dx%d, %.2f Mpx per frame (%d rows), %s buffers\n", width, height, opt_mpx, band_rows(),
		opt_dmabuf ? opt_dmabuf : "shm");

	uint32_t *spx = malloc((size_t)width * height * 4);
	for (int y = 0; y < height; y++)
		for (int x = 0; x < width; x++)
			spx[y * width + x] = 0xff000000 | ((x * 255 / width) << 16) | ((y * 255 / height) << 8) | (((x + y) / 8) & 255);
	src = pixman_image_create_bits(PIXMAN_x8r8g8b8, width, height, spx, width * 4);
	if (!strcmp(opt_op, "scale")) {
		pixman_transform_t t;
		pixman_transform_init_scale(&t, pixman_double_to_fixed(1.0 / 0.93), pixman_double_to_fixed(1.0 / 0.93));
		pixman_image_set_transform(src, &t);
		pixman_image_set_filter(src, PIXMAN_FILTER_BILINEAR, NULL, 0);
		pixman_image_set_repeat(src, PIXMAN_REPEAT_PAD);
	}
	for (int i = 0; i < nbufs; i++) {
		if (opt_dmabuf) make_dmabuf(&bufs[i]); else make_shm(&bufs[i]);
		wl_buffer_add_listener(bufs[i].wl, &buf_listener, &bufs[i]);
		bufs[i].img = pixman_image_create_bits(PIXMAN_x8r8g8b8, width, height, bufs[i].px, bufs[i].stride);
		sync_dmabuf(&bufs[i], 1);
		memset(bufs[i].px, 0x30, bufs[i].size);
		sync_dmabuf(&bufs[i], 0);
	}
	/* Full first frame, then the moving band. */
	wl_surface_attach(surface, bufs[0].wl, 0, 0);
	wl_surface_damage_buffer(surface, 0, 0, width, height);
	wl_surface_commit(surface);
	bufs[0].busy = 1;
	wl_display_roundtrip(dpy);
	draw_frame();
	double t_end = now_ms() + opt_seconds * 1e3;
	while (running && now_ms() < t_end && wl_display_dispatch(dpy) != -1) {}
	wl_display_roundtrip(dpy);

	if (npres) {
		double period = refresh_ns ? refresh_ns / 1e6 : 8.333, sum = 0;
		int missed = 0;
		for (int i = 0; i < npres; i++) { sum += pres[i]; missed += (int)floor(pres[i] / period + 0.5) - 1; }
		qsort(pres, npres, sizeof(double), cmpd);
		printf("wlbench mpx=%.2f op=%s buf=%s: frames=%d presented=%d discarded=%d zero-copy=%d refresh=%.2f ms; "
		       "interval mean=%.2f p50=%.2f p95=%.2f max=%.2f ms, missed refreshes=%d (%.1f%%), client draw mean=%.2f max=%.2f ms, %.1f fps\n",
		       opt_mpx, opt_op, opt_dmabuf ? opt_dmabuf : "shm", frames, npres + 1, discarded, zero_copy, period,
		       sum / npres, pres[npres / 2], pres[(int)(npres * 0.95)], pres[npres - 1], missed,
		       100.0 * missed / (npres + missed), render_sum / frames, render_max, 1000.0 * npres / sum);
	} else {
		printf("wlbench: no presentation feedback (frames=%d)\n", frames);
	}
	return 0;
}
