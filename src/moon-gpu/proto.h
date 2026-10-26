/* moon-compositor's wire protocol (version 1).
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

#define MG_PROTO_VERSION 1
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

struct mg_welcome {
    uint32_t version;
    uint32_t width, height;
    uint32_t refresh_mhz; /* 120000 at 120 Hz */
    uint64_t period_ns;
};

/* Pixel formats of shared-memory surfaces: Moon's canvases, 32 bits per
 * pixel, native-endian 0xAARRGGBB (bytes B, G, R, A). */
enum mg_format {
    MG_XRGB8888 = 1, /* opaque, alpha byte ignored */
    MG_ARGB8888 = 2, /* premultiplied */
};

struct mg_surface_shm {
    uint32_t id;
    uint32_t width, height, stride;
    uint32_t format;
};

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
 * width x height surface `id`. */
struct mg_surface_copy {
    uint32_t id;
    uint32_t source;
    float src_x, src_y, src_w, src_h;
    uint32_t width, height;
};

struct mg_id {
    uint32_t id;
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

struct mg_presented {
    uint32_t serial;
    uint32_t sequence; /* vblank counter */
    uint64_t time_ns;  /* CLOCK_MONOTONIC vblank time of the flip */
    uint64_t render_ns; /* submit to GPU done */
};

struct mg_power {
    uint32_t on;
};

#endif
