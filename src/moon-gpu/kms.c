#include "kms.h"

#include <errno.h>
#include <fcntl.h>
#include <math.h>
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

/* Looks up a property id by name on an object (and its current value). */
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

/* The value of an enum property's entry named `name`, or `fallback`. */
static uint64_t enum_value(int fd, uint32_t prop, const char *name,
                           uint64_t fallback) {
    struct drm_mode_get_property p = {.prop_id = prop};
    if (!prop || xioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &p) || !p.count_enum_blobs)
        return fallback;
    struct drm_mode_property_enum *e = calloc(p.count_enum_blobs, sizeof *e);
    p.enum_blob_ptr = U64(e);
    p.count_values = 0;
    p.values_ptr = 0;
    uint64_t v = fallback;
    if (!xioctl(fd, DRM_IOCTL_MODE_GETPROPERTY, &p))
        for (uint32_t i = 0; i < p.count_enum_blobs; i++)
            if (!strcmp(e[i].name, name))
                v = e[i].value;
    free(e);
    return v;
}

static int mode_hz(const struct drm_mode_modeinfo *m) {
    if (!m->htotal || !m->vtotal)
        return 0;
    return (int)((m->clock * 1000LL + m->htotal * m->vtotal / 2) /
                 ((long long)m->htotal * m->vtotal));
}

static void load_plane(int fd, struct kms_plane *p) {
#define PP(field, name) p->field = prop_id(fd, p->id, DRM_MODE_OBJECT_PLANE, name, NULL)
    PP(p_fb, "FB_ID");
    PP(p_crtc, "CRTC_ID");
    PP(p_in_fence, "IN_FENCE_FD");
    PP(p_sx, "SRC_X");
    PP(p_sy, "SRC_Y");
    PP(p_sw, "SRC_W");
    PP(p_sh, "SRC_H");
    PP(p_cx, "CRTC_X");
    PP(p_cy, "CRTC_Y");
    PP(p_cw, "CRTC_W");
    PP(p_ch, "CRTC_H");
    PP(p_alpha, "alpha");
    PP(p_zpos, "zpos");
    PP(p_blend, "pixel blend mode");
    PP(p_hdr_fd, "HDR_FD");
#undef PP
    p->blend_none = enum_value(fd, p->p_blend, "None", 0);
    p->blend_premult = enum_value(fd, p->p_blend, "Pre-multiplied", 1);
}

int kms_open(struct kms *k, const char *path, int want_hz) {
    memset(k, 0, sizeof *k);
    k->primary = -1;
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
    /* Optional: the writeback connector, for readback (kms_writeback). */
    cap.capability = DRM_CLIENT_CAP_WRITEBACK_CONNECTORS;
    xioctl(fd, DRM_IOCTL_SET_CLIENT_CAP, &cap);

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
        if (c.connector_type == DRM_MODE_CONNECTOR_WRITEBACK && !k->wb_conn) {
            struct drm_mode_get_encoder we = {.encoder_id = c.encoder_id};
            k->wb_conn = conns[i];
            if (!we.encoder_id) {
                uint32_t eid = 0;
                struct drm_mode_get_connector c2 = {.connector_id = conns[i],
                                                    .count_encoders = 1,
                                                    .encoders_ptr = U64(&eid)};
                if (!xioctl(fd, DRM_IOCTL_MODE_GETCONNECTOR, &c2))
                    we.encoder_id = eid;
            }
            if (we.encoder_id && !xioctl(fd, DRM_IOCTL_MODE_GETENCODER, &we))
                for (uint32_t j = 0; j < res.count_crtcs; j++)
                    if (we.possible_crtcs & (1u << j))
                        k->wb_crtc = crtcs[j];
            continue;
        }
        if (c.connection != 1 || !c.count_modes)
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
    uint32_t plane_ids[32];
    pr.plane_id_ptr = U64(plane_ids);
    pr.count_planes = 32;
    if ((r = xioctl(fd, DRM_IOCTL_MODE_GETPLANERESOURCES, &pr)))
        goto fail;
    for (uint32_t i = 0; i < pr.count_planes && i < 32; i++) {
        struct drm_mode_get_plane p = {.plane_id = plane_ids[i]};
        if (xioctl(fd, DRM_IOCTL_MODE_GETPLANE, &p) ||
            !(p.possible_crtcs & (1u << k->crtc_index)) ||
            k->nplanes == KMS_MAX_PLANES)
            continue;
        uint64_t type = 0;
        prop_id(fd, plane_ids[i], DRM_MODE_OBJECT_PLANE, "type", &type);
        struct kms_plane *kp = &k->planes[k->nplanes];
        memset(kp, 0, sizeof *kp);
        kp->id = plane_ids[i];
        load_plane(fd, kp);
        if (!kp->p_fb || !kp->p_crtc)
            continue;
        /* The CRTC's own primary: the first primary-typed plane (the
         * other CRTC's primary can also show here, as an overlay). */
        if (type == DRM_PLANE_TYPE_PRIMARY && k->primary < 0) {
            kp->primary = 1;
            k->primary = k->nplanes;
            k->plane_id = kp->id;
        }
        k->nplanes++;
    }
    if (k->primary < 0) {
        fprintf(stderr, "kms: no primary plane for crtc %u\n", k->crtc_id);
        r = -ENODEV;
        goto fail;
    }

    uint64_t active = 0, cur_mode = 0;
    k->crtc_active_prop =
        prop_id(fd, k->crtc_id, DRM_MODE_OBJECT_CRTC, "ACTIVE", &active);
    k->crtc_mode_prop =
        prop_id(fd, k->crtc_id, DRM_MODE_OBJECT_CRTC, "MODE_ID", &cur_mode);
    k->crtc_dqe_fd = prop_id(fd, k->crtc_id, DRM_MODE_OBJECT_CRTC, "DQE_FD", NULL);
    k->crtc_vrr = prop_id(fd, k->crtc_id, DRM_MODE_OBJECT_CRTC, "VRR_ENABLED", NULL);
    k->crtc_color_mode = prop_id(fd, k->crtc_id, DRM_MODE_OBJECT_CRTC, "color mode", NULL);
    k->crtc_render_intent =
        prop_id(fd, k->crtc_id, DRM_MODE_OBJECT_CRTC, "render intent", NULL);
    k->conn_crtc_prop =
        prop_id(fd, k->conn_id, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID", NULL);
    if (k->wb_conn && k->wb_crtc && k->wb_crtc != k->crtc_id) {
        k->wb_conn_crtc = prop_id(fd, k->wb_conn, DRM_MODE_OBJECT_CONNECTOR, "CRTC_ID", NULL);
        k->wb_fb_prop = prop_id(fd, k->wb_conn, DRM_MODE_OBJECT_CONNECTOR,
                                "WRITEBACK_FB_ID", NULL);
        k->wb_fence_prop = prop_id(fd, k->wb_conn, DRM_MODE_OBJECT_CONNECTOR,
                                   "WRITEBACK_OUT_FENCE_PTR", NULL);
        k->wb_crtc_active = prop_id(fd, k->wb_crtc, DRM_MODE_OBJECT_CRTC, "ACTIVE", NULL);
        k->wb_crtc_mode = prop_id(fd, k->wb_crtc, DRM_MODE_OBJECT_CRTC, "MODE_ID", NULL);
    } else {
        k->wb_conn = 0;
    }
    if (!k->crtc_active_prop || !k->crtc_mode_prop || !k->conn_crtc_prop) {
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
            "kms: crtc %u (index %d, %s) connector %u, %d planes (primary %u), "
            "mode %ux%u@%d\n",
            k->crtc_id, k->crtc_index, k->crtc_active ? "active" : "inactive",
            k->conn_id, k->nplanes, k->plane_id, k->mode.hdisplay,
            k->mode.vdisplay, mode_hz(&k->mode));
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
    return kms_add_fb_offset(k, dmabuf_fd, width, height, pitch, 0, fourcc, fb_id);
}

int kms_add_fb_offset(struct kms *k, int dmabuf_fd, uint32_t width,
                      uint32_t height, uint32_t pitch, uint32_t offset,
                      uint32_t fourcc, uint32_t *fb_id) {
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
        .offsets = {offset},
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

#define MAX_PROPS (KMS_MAX_PLANES * 16 + 8)

struct req {
    uint32_t objs[KMS_MAX_PLANES + 4], counts[KMS_MAX_PLANES + 4];
    uint32_t props[MAX_PROPS];
    uint64_t vals[MAX_PROPS];
    int nobj, nprop;
};

static void add(struct req *q, uint32_t obj, uint32_t prop, uint64_t val) {
    if (!prop || q->nprop == MAX_PROPS)
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

static uint64_t fixed(float v) {
    return (uint64_t)llroundf(v * 65536.0f);
}

static void plane_on_crtc(struct req *q, const struct kms_plane *p,
                          const struct kms_layer *l, int zpos, uint32_t crtc);

static void plane_on(struct kms *k, struct req *q, const struct kms_plane *p,
                     const struct kms_layer *l, int zpos) {
    plane_on_crtc(q, p, l, zpos, k->crtc_id);
}

static void plane_on_crtc(struct req *q, const struct kms_plane *p,
                          const struct kms_layer *l, int zpos, uint32_t crtc) {
    add(q, p->id, p->p_fb, l->fb);
    add(q, p->id, p->p_crtc, crtc);
    add(q, p->id, p->p_sx, fixed(l->sx));
    add(q, p->id, p->p_sy, fixed(l->sy));
    add(q, p->id, p->p_sw, fixed(l->sw));
    add(q, p->id, p->p_sh, fixed(l->sh));
    add(q, p->id, p->p_cx, (uint64_t)(int64_t)l->dx);
    add(q, p->id, p->p_cy, (uint64_t)(int64_t)l->dy);
    add(q, p->id, p->p_cw, (uint64_t)l->dw);
    add(q, p->id, p->p_ch, (uint64_t)l->dh);
    add(q, p->id, p->p_alpha, l->alpha);
    add(q, p->id, p->p_zpos, (uint64_t)zpos);
    add(q, p->id, p->p_blend, l->premultiplied ? p->blend_premult : p->blend_none);
    if (l->in_fence >= 0)
        add(q, p->id, p->p_in_fence, (uint64_t)(int64_t)l->in_fence);
    /* exynos-drm resolves HDR_FD as a file descriptor of the committing
     * process on every commit and vmaps it as HDR coefficients. The HWC
     * leaves its own fd number in the plane state (16 on this firmware); in
     * another process that number is an unrelated file or one of our
     * dmabufs, which hdr_prepare_context() then memcpy()s past its buffer:
     * a kernel panic. Always clear it. */
    add(q, p->id, p->p_hdr_fd, (uint64_t)(int64_t)-1);
}

static void plane_off(struct req *q, const struct kms_plane *p) {
    add(q, p->id, p->p_fb, 0);
    add(q, p->id, p->p_crtc, 0);
    add(q, p->id, p->p_hdr_fd, (uint64_t)(int64_t)-1);
}

int kms_commit_layers(struct kms *k, const struct kms_layer *layers, int n,
                      int flags, uint64_t user_data) {
    struct req q = {0};
    if (flags & KMS_MODESET) {
        add(&q, k->crtc_id, k->crtc_active_prop, 1);
        add(&q, k->crtc_id, k->crtc_mode_prop, k->mode_blob);
        add(&q, k->crtc_id, k->crtc_dqe_fd, (uint64_t)(int64_t)-1);
        add(&q, k->crtc_id, k->crtc_vrr, 0);
        add(&q, k->crtc_id, k->crtc_color_mode, 0);
        add(&q, k->crtc_id, k->crtc_render_intent, 0);
        add(&q, k->conn_id, k->conn_crtc_prop, k->crtc_id);
    }
    int used[KMS_MAX_PLANES] = {0};
    for (int i = 0; i < n; i++) {
        int p = layers[i].plane;
        if (p < 0 || p >= k->nplanes || used[p])
            return -EINVAL;
        used[p] = 1;
        plane_on(k, &q, &k->planes[p], &layers[i], i);
    }
    for (int p = 0; p < k->nplanes; p++)
        if (!used[p])
            plane_off(&q, &k->planes[p]);
    uint32_t f = 0;
    if (flags & KMS_TEST)
        f |= DRM_MODE_ATOMIC_TEST_ONLY;
    if (flags & KMS_MODESET)
        f |= DRM_MODE_ATOMIC_ALLOW_MODESET;
    else if ((flags & KMS_FLIP) && !(flags & KMS_TEST))
        f |= DRM_MODE_ATOMIC_NONBLOCK | DRM_MODE_PAGE_FLIP_EVENT;
    int r = submit(k, &q, f, user_data);
    if (!r && (flags & KMS_MODESET) && !(flags & KMS_TEST))
        k->crtc_active = 1;
    return r;
}

static struct kms_layer full_screen(const struct kms *k, uint32_t fb,
                                    int in_fence) {
    struct kms_layer l = {
        .fb = fb,
        .sw = k->mode.hdisplay,
        .sh = k->mode.vdisplay,
        .dw = k->mode.hdisplay,
        .dh = k->mode.vdisplay,
        .alpha = 0xffff,
        .in_fence = in_fence,
        .plane = k->primary,
    };
    return l;
}

void kms_probe_scalers(struct kms *k, uint32_t fb) {
    k->nscalers = 0;
    for (int i = 0; i < k->nplanes; i++) {
        if (i == k->primary)
            continue;
        struct kms_layer l[2] = {full_screen(k, fb, -1), full_screen(k, fb, -1)};
        l[1].plane = i;
        l[1].dx = 100;
        l[1].dy = 200;
        l[1].dw = k->mode.hdisplay * 6 / 10;
        l[1].dh = k->mode.vdisplay * 6 / 10;
        k->planes[i].scaler = !kms_commit_layers(k, l, 2, KMS_TEST, 0);
        k->nscalers += k->planes[i].scaler;
    }
    fprintf(stderr, "kms: scaling planes:");
    for (int i = 0; i < k->nplanes; i++)
        if (k->planes[i].scaler)
            fprintf(stderr, " %u", k->planes[i].id);
    fprintf(stderr, "\n");
}

int kms_assign(struct kms *k, struct kms_layer *layers, int n) {
    return kms_assign_except(k, layers, n, 0);
}

int kms_assign_except(struct kms *k, struct kms_layer *layers, int n,
                      unsigned busy) {
    if (n > k->nplanes)
        return -1;
    int taken[KMS_MAX_PLANES] = {0};
    for (int p = 0; p < k->nplanes; p++)
        taken[p] = (busy >> p) & 1;
    for (int i = 0; i < n; i++) {
        struct kms_layer *l = &layers[i];
        int scaled = fabsf(l->sw - (float)l->dw) > 0.5f ||
                     fabsf(l->sh - (float)l->dh) > 0.5f;
        int pick = -1;
        if (!scaled && i == 0 && !taken[k->primary])
            pick = k->primary;
        /* Unscaled layers take non-scaling planes first, keeping the
         * scalers for the layers that need them. */
        for (int pass = 0; pass < 2 && pick < 0; pass++)
            for (int p = 0; p < k->nplanes && pick < 0; p++) {
                if (taken[p] || p == k->primary)
                    continue;
                if (scaled ? !k->planes[p].scaler
                           : k->planes[p].scaler != (pass == 1))
                    continue;
                pick = p;
            }
        if (pick < 0 && !scaled && !taken[k->primary])
            pick = k->primary;
        if (pick < 0)
            return -1;
        taken[pick] = 1;
        l->plane = pick;
    }
    return 0;
}

int kms_writeback(struct kms *k, const struct kms_layer *layers, int n,
                  uint32_t fb) {
    if (!k->wb_conn || !k->wb_fb_prop || !k->wb_crtc_active)
        return -ENOTSUP;
    int32_t fence = -1;
    struct req q = {0};
    add(&q, k->wb_crtc, k->wb_crtc_active, 1);
    add(&q, k->wb_crtc, k->wb_crtc_mode, k->mode_blob);
    add(&q, k->wb_conn, k->wb_conn_crtc, k->wb_crtc);
    add(&q, k->wb_conn, k->wb_fb_prop, fb);
    add(&q, k->wb_conn, k->wb_fence_prop, U64(&fence));
    for (int i = 0; i < n; i++)
        plane_on_crtc(&q, &k->planes[layers[i].plane], &layers[i], i, k->wb_crtc);
    int r = submit(k, &q, DRM_MODE_ATOMIC_ALLOW_MODESET, 0);
    if (!r && fence >= 0) {
        struct pollfd p = {.fd = fence, .events = POLLIN};
        if (poll(&p, 1, 1000) <= 0)
            r = -ETIMEDOUT;
    }
    if (fence >= 0)
        close(fence);
    /* Off again, whatever happened. */
    struct req off = {0};
    for (int i = 0; i < n; i++)
        plane_off(&off, &k->planes[layers[i].plane]);
    add(&off, k->wb_conn, k->wb_conn_crtc, 0);
    add(&off, k->wb_crtc, k->wb_crtc_active, 0);
    add(&off, k->wb_crtc, k->wb_crtc_mode, 0);
    int r2 = submit(k, &off, DRM_MODE_ATOMIC_ALLOW_MODESET, 0);
    if (r2)
        fprintf(stderr, "kms: writeback teardown: %s\n", strerror(-r2));
    return r;
}

int kms_commit(struct kms *k, uint32_t fb, int in_fence, int modeset,
               uint64_t user_data) {
    struct kms_layer l = full_screen(k, fb, in_fence);
    return kms_commit_layers(k, &l, 1, modeset ? KMS_MODESET : KMS_FLIP,
                             user_data);
}

int kms_commit_blocking(struct kms *k, uint32_t fb) {
    struct kms_layer l = full_screen(k, fb, -1);
    return kms_commit_layers(k, &l, 1, 0, 0);
}

int kms_set_active(struct kms *k, int active, uint32_t fb) {
    struct req q = {0};
    add(&q, k->crtc_id, k->crtc_active_prop, (uint64_t)active);
    if (active) {
        struct kms_layer l = full_screen(k, fb, -1);
        plane_on(k, &q, &k->planes[k->primary], &l, 0);
        for (int p = 0; p < k->nplanes; p++)
            if (p != k->primary)
                plane_off(&q, &k->planes[p]);
    }
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
