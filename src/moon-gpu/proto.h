/* moon-compositor's wire protocol (version 4; versions 2 to 4 accepted).
 *
 * One client (Moon) on a SOCK_SEQPACKET Unix socket: every message is one
 * packet, a header followed by its body, little-endian, 4-byte aligned. File
 * descriptors travel as SCM_RIGHTS with the message that names them. Moon's
 * side is src/a26-shell/src/gpu.rs; keep the two in step.
 *
 * Surfaces are client-numbered textures (id > 0). MG_MIRROR_ID names the live
 * X mirror: a dmabuf the compositor allocates and hands out (MG_MIRROR),
 * which Xorg writes the visible root into on Moon's request.
 */
#ifndef MOON_GPU_PROTO_H
#define MOON_GPU_PROTO_H

#include <stdint.h>

/* The newest version this side speaks, and the oldest it still accepts.
 * The client's MG_HELLO names the version it speaks; MG_WELCOME echoes it
 * when accepted (a client must not use newer fields than it said).
 *   v2: the October 2026 protocol.
 *   v3: mg_surface_shm.offset, the byte offset of the pixels in the fd
 *       (wl_shm buffers live inside a pool). Absent (v2 messages), it is 0.
 *   v4: client memory that may shrink (MG_SURFACE_FOREIGN), MG_SURFACE_IDLE
 *       for surfaces that ask (MG_SURFACE_REPORT_IDLE), MG_SURFACE_FAILED.
 *       The compositor sends v4 messages and honours v4 flags only to and
 *       from a client that said v4.
 *   v5: the X mirror is several buffers, so Xorg never writes the one the
 *       display scans out (no tearing). MG_MIRROR comes once per buffer,
 *       mg_surface_shm.id holding its index. The client writes only a
 *       buffer the compositor gave it with MG_MIRROR_RELEASED, then names
 *       it in MG_MIRROR_DAMAGE (mg_damage.reserved = its index, with the
 *       rectangles written); frames show that buffer from then on, and the
 *       one they showed before comes back released once nothing scans or
 *       samples it. After the hello every buffer the client may write is
 *       released. A v4 client gets buffer 0 alone, written in place.
 * Compatibility: an older client works with a newer compositor unchanged;
 * a newer client needs a compositor that speaks its version (deploy the
 * compositor first). Compositors before v4 drop a client whose version
 * differs from theirs, so a client steps down when its hello is refused. */
#define MG_PROTO_VERSION 5
#define MG_PROTO_MIN_VERSION 2
#define MG_MIRROR_ID 0xffffffffu
#define MG_MAX_PACKET (64 * 1024)

enum mg_type {
    /* client -> compositor */
    MG_HELLO = 1,           /* mg_hello */
    MG_SURFACE_SHM = 2,     /* mg_surface_shm + fd (memfd, stride * height) */
    MG_SURFACE_DAMAGE = 3,  /* mg_damage + rects: upload from its memory */
    MG_SURFACE_COPY = 4,    /* mg_surface_copy: new surface from a region */
    MG_SURFACE_FREE = 5,    /* mg_id */
    MG_MIRROR_DAMAGE = 6,   /* mg_damage (id 0) + rects: Xorg wrote these */
    MG_FRAME = 7,           /* mg_frame + nodes */
    MG_POWER = 8,           /* mg_power */
    /* compositor -> client */
    MG_WELCOME = 64,        /* mg_welcome */
    MG_MIRROR = 65,         /* mg_mirror + fd (dmabuf) */
    MG_UPLOADED = 66,       /* mg_uploaded: the surface's memory is free */
    MG_PRESENTED = 67,      /* mg_presented */
    MG_POWERED = 68,        /* mg_power: the panel state took effect */
    MG_SURFACE_IDLE = 69,   /* v4, mg_id: nothing references it any more */
    MG_SURFACE_FAILED = 70, /* v4, mg_failed: the surface is gone */
    MG_MIRROR_RELEASED = 71, /* v5, mg_id: a mirror buffer (index) to write */
};

struct mg_header {
    uint16_t type;
    uint16_t flags;
    uint32_t length; /* whole packet, header included */
};

struct mg_rect {
    int32_t x0, y0, x1, y1;
};

struct mg_hello {
    uint32_t version;
};

enum mg_caps {
    MG_CAP_GPU = 1,    /* frames the planes cannot show are drawn by the GPU */
    MG_CAP_PLANES = 2, /* frames that fit go straight to display planes */
};

struct mg_welcome {
    uint32_t version;
    uint32_t width, height;
    uint32_t refresh_mhz; /* 120000 at 120 Hz */
    uint64_t period_ns;
    uint32_t caps;
    uint32_t reserved;
};

/* Pixel formats of shared-memory surfaces: Moon's canvases, 32 bits per
 * pixel, native-endian 0xAARRGGBB (bytes B, G, R, A). */
enum mg_format {
    MG_XRGB8888 = 1, /* opaque, alpha byte ignored */
    MG_ARGB8888 = 2, /* premultiplied */
};

enum mg_surface_flags {
    /* The fd is a dmabuf (a cached dma-heap buffer the client writes with
     * DMA_BUF_IOCTL_SYNC): the compositor samples it in place and may scan
     * it out on a plane. Otherwise a memfd, copied on each damage. */
    MG_SURFACE_DMABUF = 1,
    /* v4: a memfd that is not the client's own (a Wayland client's wl_shm
     * pool): it may be truncated under the compositor. Unless it is sealed
     * against shrinking (F_SEAL_SHRINK), the compositor reads it only into
     * its own copy, under a SIGBUS guard; a fault ends the surface
     * (MG_SURFACE_FAILED, MG_FAIL_FAULT). Ignored for dmabufs. */
    MG_SURFACE_FOREIGN = 2,
    /* v4: send MG_SURFACE_IDLE each time the compositor stops referencing
     * the surface (below). */
    MG_SURFACE_REPORT_IDLE = 4,
};

struct mg_surface_shm {
    uint32_t id;
    uint32_t width, height, stride;
    uint32_t format;
    uint32_t flags;
    /* v3: where the pixels start in the fd, in bytes (a multiple of 4; any
     * value for a memfd, which the compositor maps from the page below; a
     * dmabuf offset goes to the GPU import and the KMS framebuffer). A v2
     * message ends before this field. */
    uint32_t offset;
};
#define MG_SURFACE_SHM_V2_SIZE 24 /* bytes of mg_surface_shm in v2 */

struct mg_damage {
    uint32_t id;     /* surface, or 0 for the mirror */
    uint32_t serial; /* echoed in MG_UPLOADED */
    uint32_t count;
    uint32_t reserved;
    /* struct mg_rect rects[count]; */
};

struct mg_uploaded {
    uint32_t id, serial;
};

/* Copies `src` (surface or MG_MIRROR_ID, in its pixels) scaled to a new
 * width x height surface `id`, with transparent rounded corners of `radius`
 * (in the copy's pixels) when it is positive: such a copy can show on a
 * display plane where a frame asks for those corners. */
struct mg_surface_copy {
    uint32_t id;
    uint32_t source;
    float src_x, src_y, src_w, src_h;
    uint32_t width, height;
    float radius;
};

struct mg_id {
    uint32_t id;
};

/* MG_SURFACE_IDLE (v4), for surfaces created with MG_SURFACE_REPORT_IDLE:
 * the compositor no longer references the surface's memory, so its owner
 * may write or reuse it. Sent each time the surface stops being named by
 * the queued frame, the current frame and the frame on screen, which is
 * after the flip that took it off screen (and at once for a queued frame
 * that was replaced before it was shown). Damage uploads and
 * MG_SURFACE_COPY read the memory before the compositor reads its next
 * message, so they never hold it. A surface that a frame names again is
 * referenced again and gets another MG_SURFACE_IDLE later. None follows
 * MG_SURFACE_FREE, which ends the id: once the compositor has read it the
 * memory is no longer referenced. */

enum mg_fail_reason {
    MG_FAIL_INVALID = 1, /* bad parameters, or the fd is smaller than them */
    MG_FAIL_IMPORT = 2,  /* the memory cannot be mapped, imported or copied */
    MG_FAIL_FAULT = 3,   /* the memory shrank under the compositor (SIGBUS) */
};

/* MG_SURFACE_FAILED (v4): surface `id` (from MG_SURFACE_SHM or
 * MG_SURFACE_COPY) does not exist, or no longer does; frames that name it
 * skip it. The id may be used again. */
struct mg_failed {
    uint32_t id;
    uint32_t reason; /* mg_fail_reason */
};

enum mg_node_kind {
    MG_NODE_IMAGE = 1,
    MG_NODE_FILL = 2,
};

enum mg_node_flags {
    MG_NODE_SMOOTH = 1, /* bilinear when scaled, nearest otherwise */
};

struct mg_node {
    uint32_t kind;
    uint32_t surface; /* image: surface id or MG_MIRROR_ID */
    float src_x, src_y, src_w, src_h; /* image: surface pixels */
    float dst_x, dst_y, dst_w, dst_h; /* screen pixels */
    float alpha;  /* 0..1, multiplies everything */
    float dim;    /* image: 0..1 brightness factor */
    float radius; /* rounded-corner radius in screen pixels */
    uint32_t flags;
    uint32_t color; /* fill: 0xRRGGBB */
    uint32_t reserved;
};

enum mg_frame_flags {
    MG_FRAME_HAS_BASE = 1,
};

/* Draws `base` (a full-screen surface at 1:1) then the nodes, at the next
 * vblank; MG_PRESENTED with this serial follows the flip. A frame that
 * arrives while one is pending replaces it. */
struct mg_frame {
    uint32_t serial;
    uint32_t flags;
    uint32_t base;
    uint32_t count;
    /* struct mg_node nodes[count]; */
};

enum mg_path {
    MG_PATH_PLANES = 1,
    MG_PATH_GPU = 2,
    MG_PATH_CPU = 3,
};

struct mg_presented {
    uint32_t serial;
    uint32_t sequence; /* vblank counter */
    uint64_t time_ns;  /* CLOCK_MONOTONIC vblank time of the flip */
    uint64_t render_ns; /* submit to drawing done (0 for planes) */
    uint32_t path;      /* mg_path */
    uint32_t reserved;
};

struct mg_power {
    uint32_t on;
};

#endif
