/* Minimal atomic KMS over raw DRM ioctls (no libdrm: it is a private
 * platform library on Android). One CRTC and its connector, with every plane
 * that can show on it: exynos-drmdpu offers eight (DPP0-7), each with alpha
 * and zpos, three of them scaling. */
#ifndef MOON_GPU_KMS_H
#define MOON_GPU_KMS_H

#include <stdint.h>
#include <drm/drm.h>
#include <drm/drm_mode.h>

#define KMS_MAX_PLANES 8

struct kms_plane {
    uint32_t id;
    int primary;
    int scaler; /* accepts scaled layers (probed with TEST_ONLY) */
    uint32_t p_fb, p_crtc, p_in_fence;
    uint32_t p_sx, p_sy, p_sw, p_sh, p_cx, p_cy, p_cw, p_ch;
    uint32_t p_alpha, p_zpos, p_blend;
    uint64_t blend_none, blend_premult;
    /* Samsung's fd-valued property (0 if absent): see kms_commit_layers. */
    uint32_t p_hdr_fd;
};

struct kms {
    int fd;
    uint32_t crtc_id, conn_id, plane_id; /* plane_id: the primary plane */
    int crtc_index;
    struct drm_mode_modeinfo mode;
    uint32_t mode_blob;
    int crtc_active; /* CRTC ACTIVE when we opened it, with our mode */
    uint32_t crtc_active_prop, crtc_mode_prop, crtc_dqe_fd;
    uint32_t conn_crtc_prop;
    struct kms_plane planes[KMS_MAX_PLANES];
    int nplanes, primary, nscalers;
};

/* One plane's worth of a frame, bottom first. Coordinates in pixels; the
 * source may be fractional, the destination lies on screen. */
struct kms_layer {
    uint32_t fb;
    float sx, sy, sw, sh;
    int32_t dx, dy, dw, dh;
    uint16_t alpha;    /* 0xffff opaque */
    int premultiplied; /* blend with the buffer's alpha */
    int in_fence;      /* -1 for none */
    int plane;         /* index into kms.planes, set by kms_assign */
};

struct kms_flip {
    uint64_t time_ns; /* CLOCK_MONOTONIC vblank time of the flip */
    uint32_t sequence;
    uint64_t user_data;
};

/* Opens the card, becomes master, finds the connected connector (preferring
 * DSI), its CRTC, its planes and the mode with `want_hz` refresh (0:
 * preferred). Returns 0 or -errno; prints why on failure. */
int kms_open(struct kms *k, const char *path, int want_hz);
void kms_close(struct kms *k);

/* Imports a dmabuf as a framebuffer. */
int kms_add_fb(struct kms *k, int dmabuf_fd, uint32_t width, uint32_t height,
               uint32_t pitch, uint32_t fourcc, uint32_t *fb_id);
/* The same, with the pixels `offset` bytes into the buffer. */
int kms_add_fb_offset(struct kms *k, int dmabuf_fd, uint32_t width,
                      uint32_t height, uint32_t pitch, uint32_t offset,
                      uint32_t fourcc, uint32_t *fb_id);
void kms_rm_fb(struct kms *k, uint32_t fb_id);

/* Finds which planes scale (TEST_ONLY with `fb`, a full-screen buffer). */
void kms_probe_scalers(struct kms *k, uint32_t fb);

/* Picks a plane for each layer (zpos follows the order): scaled layers on
 * scaling planes. Returns 0, or -1 when the layers do not fit. */
int kms_assign(struct kms *k, struct kms_layer *layers, int n);

enum {
    KMS_TEST = 1,    /* TEST_ONLY */
    KMS_FLIP = 2,    /* nonblocking, with a flip event */
    KMS_MODESET = 4, /* also CRTC ACTIVE/MODE and the connector, blocking */
};
/* Shows exactly `layers` (planes not named are turned off). Without
 * KMS_FLIP the commit blocks and sends no event. */
int kms_commit_layers(struct kms *k, const struct kms_layer *layers, int n,
                      int flags, uint64_t user_data);

/* Shows `fb` full screen on the primary plane. `in_fence` -1 for none.
 * `modeset` also programs CRTC ACTIVE/MODE and the connector (blocking).
 * Otherwise nonblocking with a flip event carrying `user_data`. */
int kms_commit(struct kms *k, uint32_t fb, int in_fence, int modeset,
               uint64_t user_data);
/* Shows `fb` with a blocking commit (no modeset, no event). */
int kms_commit_blocking(struct kms *k, uint32_t fb);
/* Turns the CRTC off (ACTIVE=0, keeping the mode, like DPMS off), or on
 * with `fb` full screen. */
int kms_set_active(struct kms *k, int active, uint32_t fb);

/* Waits up to timeout_ms for a flip event. 1 got one, 0 timeout, <0 error. */
int kms_wait_flip(struct kms *k, int timeout_ms, struct kms_flip *out);

#endif
