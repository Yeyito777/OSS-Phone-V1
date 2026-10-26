/* Minimal atomic KMS over raw DRM ioctls (no libdrm: it is a private
 * platform library on Android). One CRTC, its connector and primary plane. */
#ifndef MOON_GPU_KMS_H
#define MOON_GPU_KMS_H

#include <stdint.h>
#include <drm/drm.h>
#include <drm/drm_mode.h>

struct kms {
    int fd;
    uint32_t crtc_id, conn_id, plane_id;
    int crtc_index;
    struct drm_mode_modeinfo mode;
    uint32_t mode_blob;
    int crtc_active; /* CRTC ACTIVE when we opened it, with our mode */
    /* property ids */
    uint32_t crtc_active_prop, crtc_mode_prop;
    uint32_t conn_crtc_prop;
    uint32_t plane_fb, plane_crtc, plane_in_fence;
    uint32_t plane_src_x, plane_src_y, plane_src_w, plane_src_h;
    uint32_t plane_crtc_x, plane_crtc_y, plane_crtc_w, plane_crtc_h;
    /* Samsung's fd-valued properties (0 if absent): see kms_commit. */
    uint32_t plane_hdr_fd, crtc_dqe_fd;
};

struct kms_flip {
    uint64_t time_ns; /* CLOCK_MONOTONIC vblank time of the flip */
    uint32_t sequence;
    uint64_t user_data;
};

/* Opens the card, becomes master, finds the connected connector (preferring
 * DSI), its CRTC and primary plane, and the mode with `want_hz` refresh
 * (0: preferred). Returns 0 or -errno; prints why on failure. */
int kms_open(struct kms *k, const char *path, int want_hz);
void kms_close(struct kms *k);

/* Imports a dmabuf as a framebuffer. */
int kms_add_fb(struct kms *k, int dmabuf_fd, uint32_t width, uint32_t height,
               uint32_t pitch, uint32_t fourcc, uint32_t *fb_id);
void kms_rm_fb(struct kms *k, uint32_t fb_id);

/* Shows `fb` full screen. `in_fence` -1 for none. `modeset` also programs
 * CRTC ACTIVE/MODE and the connector (blocking, ALLOW_MODESET).
 * Otherwise nonblocking with a flip event carrying `user_data`. */
int kms_commit(struct kms *k, uint32_t fb, int in_fence, int modeset,
               uint64_t user_data);
/* Shows `fb` with a blocking commit (no modeset, no event). */
int kms_commit_blocking(struct kms *k, uint32_t fb);
/* Turns the CRTC off (ACTIVE=0, keeping the mode, like DPMS off). */
int kms_set_active(struct kms *k, int active, uint32_t fb);

/* Waits up to timeout_ms for a flip event. 1 got one, 0 timeout, <0 error. */
int kms_wait_flip(struct kms *k, int timeout_ms, struct kms_flip *out);

#endif
