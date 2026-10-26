#include "kms.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>

#ifndef DRM_PLANE_TYPE_PRIMARY
#define DRM_PLANE_TYPE_PRIMARY 1 /* libdrm, not uapi */
#endif

#define U64(p) ((uint64_t)(uintptr_t)(p))

static int xioctl(int fd, unsigned long req, void *arg) {
    int r;
    do {
        r = ioctl(fd, req, arg);
    } while (r == -1 && (errno == EINTR || errno == EAGAIN));
    return r == -1 ? -errno : 0;
}

/* Looks up a property id by name on an object. */
static uint32_t prop_id(int fd, uint32_t obj, uint32_t type, const char *name,
                        uint64_t *value) {
    struct drm_mode_obj_get_properties gp = {.obj_id = obj, .obj_type = type};
    if (xioctl(fd, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &gp) || !gp.count_props)
        return 0;
    uint32_t *ids = calloc(gp.count_props, sizeof *ids);
    uint64_t *vals = calloc(gp.count_props, sizeof *vals);
    gp.props_ptr = U64(ids);
    gp.prop_values_ptr = U64(vals);
    uint32_t found = 0;
    if (!xioctl(fd, DRM_IOCTL_MODE_OBJ_GETPROPERTIES, &gp)) {
        for (uint32_t i = 0; i < gp.count_props && !found; i++) {
            struct drm_mode_get_property p = {.prop_id = ids[i]};
            if (xioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &p))
                continue;
            if (!strcmp(p.name, name)) {
                found = ids[i];
                if (value)
                    *value = vals[i];
            }
        }
    }
    free(ids);
    free(vals);
    return found;
}

static int mode_hz(const struct drm_mode_modeinfo *m) {
    if (!m->htotal || !m->vtotal)
        return 0;
    return (int)((m->clock * 1000LL + m->htotal * m->vtotal / 2) /
                 ((long long)m->htotal * m->vtotal));
}

int kms_open(struct kms *k, const char *path, int want_hz) {
    memset(k, 0, sizeof *k);
    k->fd = open(path, O_RDWR | O_CLOEXEC);
    if (k->fd < 0) {
        int e = -errno;
        fprintf(stderr, "kms: open %s: %s\n", path, strerror(errno));
        return e;
    }
    int fd = k->fd, r;
    if ((r = xioctl(fd, DRM_IOCTL_SET_MASTER, NULL))) {
        fprintf(stderr, "kms: SET_MASTER: %s (is Xorg still running?)\n",
                strerror(-r));
        goto fail;
    }
    struct drm_set_client_cap cap = {DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1};
    if ((r = xioctl(fd, DRM_IOCTL_SET_CLIENT_CAP, &cap)))
        goto fail;
    cap.capability = DRM_CLIENT_CAP_ATOMIC;
    if ((r = xioctl(fd, DRM_IOCTL_SET_CLIENT_CAP, &cap))) {
        fprintf(stderr, "kms: no atomic: %s\n", strerror(-r));
        goto fail;
    }

    struct drm_mode_card_res res = {0};
    if ((r = xioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res)))
        goto fail;
    uint32_t crtcs[16] = {0}, conns[16] = {0}, encs[16] = {0};
    if (res.count_crtcs > 16 || res.count_connectors > 16 ||
        res.count_encoders > 16) {
        r = -E2BIG;
        goto fail;
    }
    res.crtc_id_ptr = U64(crtcs);
    res.connector_id_ptr = U64(conns);
    res.encoder_id_ptr = U64(encs);
    res.count_fbs = 0;
    res.fb_id_ptr = 0;
    if ((r = xioctl(fd, DRM_IOCTL_MODE_GETRESOURCES, &res)))
        goto fail;

    /* The connected connector: DSI first, anything but writeback otherwise. */
    struct drm_mode_get_connector best = {0};
    struct drm_mode_modeinfo *best_modes = NULL;
    for (uint32_t i = 0; i < res.count_connectors; i++) {
        struct drm_mode_get_connector c = {.connector_id = conns[i]};
        if (xioctl(fd, DRM_IOCTL_MODE_GETCONNECTOR, &c))
            continue;
        if (c.connection != 1 || !c.count_modes ||
            c.connector_type == DRM_MODE_CONNECTOR_WRITEBACK)
            continue;
        struct drm_mode_modeinfo *modes = calloc(c.count_modes, sizeof *modes);
        uint32_t *enc_ids = calloc(c.count_encoders + 1, sizeof *enc_ids);
        c.modes_ptr = U64(modes);
        c.encoders_ptr = U64(enc_ids);
        c.count_props = 0;
        c.props_ptr = c.prop_values_ptr = 0;
        if (xioctl(fd, DRM_IOCTL_MODE_GETCONNECTOR, &c)) {
            free(modes);
            free(enc_ids);
            continue;
        }
        if (!best.connector_id || c.connector_type == DRM_MODE_CONNECTOR_DSI) {
            free(best_modes);
            best = c;
            best_modes = modes;
            if (!best.encoder_id && c.count_encoders)
                best.encoder_id = enc_ids[0];
        } else {
            free(modes);
        }
        free(enc_ids);
    }
    if (!best.connector_id) {
        fprintf(stderr, "kms: no connected connector\n");
        r = -ENODEV;
        goto fail;
    }
    k->conn_id = best.connector_id;
    int pick = -1;
    for (uint32_t i = 0; i < best.count_modes; i++) {
        int hz = mode_hz(&best_modes[i]);
        if (want_hz ? hz == want_hz
                    : (best_modes[i].type & DRM_MODE_TYPE_PREFERRED)) {
            pick = (int)i;
            break;
        }
    }
    if (pick < 0)
        pick = 0;
    k->mode = best_modes[pick];
    free(best_modes);

    struct drm_mode_get_encoder enc = {.encoder_id = best.encoder_id};
    if ((r = xioctl(fd, DRM_IOCTL_MODE_GETENCODER, &enc)))
        goto fail;
    k->crtc_id = enc.crtc_id;
    for (uint32_t i = 0; i < res.count_crtcs; i++) {
        if (!k->crtc_id && (enc.possible_crtcs & (1u << i)))
            k->crtc_id = crtcs[i];
        if (crtcs[i] == k->crtc_id)
            k->crtc_index = (int)i;
    }

    struct drm_mode_get_plane_res pr = {0};
    uint32_t planes[32];
    pr.plane_id_ptr = U64(planes);
    pr.count_planes = 32;
    if ((r = xioctl(fd, DRM_IOCTL_MODE_GETPLANERESOURCES, &pr)))
        goto fail;
    for (uint32_t i = 0; i < pr.count_planes && i < 32 && !k->plane_id; i++) {
        struct drm_mode_get_plane p = {.plane_id = planes[i]};
        if (xioctl(fd, DRM_IOCTL_MODE_GETPLANE, &p))
            continue;
        if (!(p.possible_crtcs & (1u << k->crtc_index)))
            continue;
        uint64_t type = 0;
        if (prop_id(fd, planes[i], DRM_MODE_OBJECT_PLANE, "type", &type) &&
            type == DRM_PLANE_TYPE_PRIMARY)
            k->plane_id = planes[i];
    }
    if (!k->plane_id) {
        fprintf(stderr, "kms: no primary plane for crtc %u\n", k->crtc_id);
        r = -ENODEV;
        goto fail;
    }

    uint64_t active = 0, cur_mode = 0;
    k->crtc_active_prop =
        prop_id(fd, k->crtc_id, DRM_MODE_OBJECT_CRTC, "ACTIVE", &active);
    k->crtc_mode_prop =
        prop_id(fd, k->crtc_id, DRM_MODE_OBJECT_CRTC, "MODE_ID", &cur_mode);
    k->conn_crtc_prop =
        prop_id(fd, k->conn_id, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID", NULL);
#define PP(field, name)                                                        \
    k->field = prop_id(fd, k->plane_id, DRM_MODE_OBJECT_PLANE, name, NULL)
    PP(plane_fb, "FB_ID");
    PP(plane_crtc, "CRTC_ID");
    PP(plane_in_fence, "IN_FENCE_FD");
    PP(plane_src_x, "SRC_X");
    PP(plane_src_y, "SRC_Y");
    PP(plane_src_w, "SRC_W");
    PP(plane_src_h, "SRC_H");
    PP(plane_crtc_x, "CRTC_X");
    PP(plane_crtc_y, "CRTC_Y");
    PP(plane_crtc_w, "CRTC_W");
    PP(plane_crtc_h, "CRTC_H");
    PP(plane_hdr_fd, "HDR_FD");
#undef PP
    k->crtc_dqe_fd = prop_id(fd, k->crtc_id, DRM_MODE_OBJECT_CRTC, "DQE_FD", NULL);
    if (!k->crtc_active_prop || !k->crtc_mode_prop || !k->conn_crtc_prop ||
        !k->plane_fb || !k->plane_crtc) {
        fprintf(stderr, "kms: missing atomic properties\n");
        r = -ENOTSUP;
        goto fail;
    }

    struct drm_mode_create_blob blob = {.data = U64(&k->mode),
                                        .length = sizeof k->mode};
    if ((r = xioctl(fd, DRM_IOCTL_MODE_CREATEPROPBLOB, &blob)))
        goto fail;
    k->mode_blob = blob.blob_id;

    /* Reuse an already active CRTC with the same timing: no modeset. */
    if (active && cur_mode) {
        struct drm_mode_modeinfo m;
        struct drm_mode_get_blob gb = {.blob_id = (uint32_t)cur_mode,
                                       .length = sizeof m,
                                       .data = U64(&m)};
        if (!xioctl(fd, DRM_IOCTL_MODE_GETPROPBLOB, &gb) &&
            gb.length == sizeof m && m.hdisplay == k->mode.hdisplay &&
            m.vdisplay == k->mode.vdisplay && mode_hz(&m) == mode_hz(&k->mode))
            k->crtc_active = 1;
    }
    fprintf(stderr,
            "kms: crtc %u (index %d, %s) connector %u plane %u mode %ux%u@%d\n",
            k->crtc_id, k->crtc_index, k->crtc_active ? "active" : "inactive",
            k->conn_id, k->plane_id, k->mode.hdisplay, k->mode.vdisplay,
            mode_hz(&k->mode));
    return 0;
fail:
    if (!r)
        r = -EIO;
    fprintf(stderr, "kms: setup failed: %s\n", strerror(-r));
    close(k->fd);
    k->fd = -1;
    return r;
}

void kms_close(struct kms *k) {
    if (k->fd >= 0)
        close(k->fd);
    k->fd = -1;
}

int kms_add_fb(struct kms *k, int dmabuf_fd, uint32_t width, uint32_t height,
               uint32_t pitch, uint32_t fourcc, uint32_t *fb_id) {
    struct drm_prime_handle ph = {.fd = dmabuf_fd};
    int r = xioctl(k->fd, DRM_IOCTL_PRIME_FD_TO_HANDLE, &ph);
    if (r) {
        fprintf(stderr, "kms: PRIME import: %s\n", strerror(-r));
        return r;
    }
    struct drm_mode_fb_cmd2 f = {
        .width = width,
        .height = height,
        .pixel_format = fourcc,
        .handles = {ph.handle},
        .pitches = {pitch},
    };
    r = xioctl(k->fd, DRM_IOCTL_MODE_ADDFB2, &f);
    /* The framebuffer holds its own reference to the GEM object. */
    struct drm_gem_close gc = {.handle = ph.handle};
    xioctl(k->fd, DRM_IOCTL_GEM_CLOSE, &gc);
    if (r) {
        fprintf(stderr, "kms: ADDFB2: %s\n", strerror(-r));
        return r;
    }
    *fb_id = f.fb_id;
    return 0;
}

void kms_rm_fb(struct kms *k, uint32_t fb_id) {
    xioctl(k->fd, DRM_IOCTL_MODE_RMFB, &fb_id);
}

struct req {
    uint32_t objs[4], counts[4];
    uint32_t props[24];
    uint64_t vals[24];
    int nobj, nprop;
};

static void add(struct req *q, uint32_t obj, uint32_t prop, uint64_t val) {
    if (!prop)
        return;
    if (!q->nobj || q->objs[q->nobj - 1] != obj) {
        q->objs[q->nobj] = obj;
        q->counts[q->nobj++] = 0;
    }
    q->counts[q->nobj - 1]++;
    q->props[q->nprop] = prop;
    q->vals[q->nprop++] = val;
}

static int submit(struct kms *k, struct req *q, uint32_t flags,
                  uint64_t user_data) {
    struct drm_mode_atomic a = {
        .flags = flags,
        .count_objs = (uint32_t)q->nobj,
        .objs_ptr = U64(q->objs),
        .count_props_ptr = U64(q->counts),
        .props_ptr = U64(q->props),
        .prop_values_ptr = U64(q->vals),
        .user_data = user_data,
    };
    int r;
    do {
        r = ioctl(k->fd, DRM_IOCTL_MODE_ATOMIC, &a);
    } while (r == -1 && errno == EINTR);
    return r == -1 ? -errno : 0;
}

static void plane_props(struct kms *k, struct req *q, uint32_t fb,
                        int in_fence) {
    uint32_t w = k->mode.hdisplay, h = k->mode.vdisplay;
    add(q, k->plane_id, k->plane_fb, fb);
    add(q, k->plane_id, k->plane_crtc, k->crtc_id);
    add(q, k->plane_id, k->plane_src_x, 0);
    add(q, k->plane_id, k->plane_src_y, 0);
    add(q, k->plane_id, k->plane_src_w, (uint64_t)w << 16);
    add(q, k->plane_id, k->plane_src_h, (uint64_t)h << 16);
    add(q, k->plane_id, k->plane_crtc_x, 0);
    add(q, k->plane_id, k->plane_crtc_y, 0);
    add(q, k->plane_id, k->plane_crtc_w, w);
    add(q, k->plane_id, k->plane_crtc_h, h);
    if (in_fence >= 0)
        add(q, k->plane_id, k->plane_in_fence, (uint64_t)(int64_t)in_fence);
    /* exynos-drm resolves HDR_FD as a file descriptor of the committing
     * process on every commit and vmaps it as HDR coefficients. The HWC
     * leaves its own fd number in the plane state (16 on this firmware); in
     * another process that number is an unrelated file or one of our
     * dmabufs, which hdr_prepare_context() then memcpy()s past its buffer:
     * a kernel panic. Always clear it. */
    add(q, k->plane_id, k->plane_hdr_fd, (uint64_t)(int64_t)-1);
}

int kms_commit(struct kms *k, uint32_t fb, int in_fence, int modeset,
               uint64_t user_data) {
    struct req q = {0};
    if (modeset) {
        add(&q, k->crtc_id, k->crtc_active_prop, 1);
        add(&q, k->crtc_id, k->crtc_mode_prop, k->mode_blob);
        add(&q, k->crtc_id, k->crtc_dqe_fd, (uint64_t)(int64_t)-1);
        add(&q, k->conn_id, k->conn_crtc_prop, k->crtc_id);
        plane_props(k, &q, fb, in_fence);
        int r = submit(k, &q, DRM_MODE_ATOMIC_ALLOW_MODESET, 0);
        if (!r)
            k->crtc_active = 1;
        return r;
    }
    plane_props(k, &q, fb, in_fence);
    return submit(k, &q, DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_PAGE_FLIP_EVENT,
                  user_data);
}

int kms_commit_blocking(struct kms *k, uint32_t fb) {
    struct req q = {0};
    plane_props(k, &q, fb, -1);
    return submit(k, &q, 0, 0);
}

int kms_set_active(struct kms *k, int active, uint32_t fb) {
    struct req q = {0};
    add(&q, k->crtc_id, k->crtc_active_prop, (uint64_t)active);
    if (active)
        plane_props(k, &q, fb, -1);
    int r = submit(k, &q, DRM_MODE_ATOMIC_ALLOW_MODESET, 0);
    if (!r)
        k->crtc_active = active;
    return r;
}

int kms_wait_flip(struct kms *k, int timeout_ms, struct kms_flip *out) {
    struct pollfd p = {.fd = k->fd, .events = POLLIN};
    for (;;) {
        int n = poll(&p, 1, timeout_ms);
        if (n < 0 && errno == EINTR)
            continue;
        if (n <= 0)
            return n < 0 ? -errno : 0;
        char buf[1024];
        ssize_t len = read(k->fd, buf, sizeof buf);
        if (len < 0)
            return errno == EAGAIN ? 0 : -errno;
        int got = 0;
        for (ssize_t off = 0; off + (ssize_t)sizeof(struct drm_event) <= len;) {
            struct drm_event *e = (struct drm_event *)(buf + off);
            if (e->type == DRM_EVENT_FLIP_COMPLETE &&
                e->length >= sizeof(struct drm_event_vblank)) {
                struct drm_event_vblank *v = (struct drm_event_vblank *)e;
                out->time_ns = v->tv_sec * 1000000000ull + v->tv_usec * 1000ull;
                out->sequence = v->sequence;
                out->user_data = v->user_data;
                got = 1;
            }
            if (!e->length)
                break;
            off += e->length;
        }
        if (got)
            return 1;
    }
}
