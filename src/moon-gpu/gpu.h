/* EGL/GLES on Android's Mali driver without a window system: a surfaceless
 * context rendering into dmabufs that KMS scans out directly. */
#ifndef MOON_GPU_GPU_H
#define MOON_GPU_GPU_H

#include <stdint.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl3.h>

struct gpu {
    EGLDisplay dpy;
    EGLContext ctx;
    EGLSurface pbuf; /* only when surfaceless contexts are unavailable */
    PFNEGLCREATEIMAGEKHRPROC create_image;
    PFNEGLDESTROYIMAGEKHRPROC destroy_image;
    PFNEGLCREATESYNCKHRPROC create_sync;
    PFNEGLDESTROYSYNCKHRPROC destroy_sync;
    PFNEGLDUPNATIVEFENCEFDANDROIDPROC dup_fence;
    void (*image_target_rb)(GLenum, void *);
    void (*image_target_tex)(GLenum, void *);
    int high_priority;
};

/* A dmabuf the GPU renders into and KMS scans out. */
struct gpu_target {
    int fd; /* dmabuf */
    uint32_t width, height, pitch, fourcc;
    EGLImageKHR image;
    GLuint rb, fbo;
};

int gpu_init(struct gpu *g);
void gpu_fini(struct gpu *g);

/* Allocates from /dev/dma_heap/<heap>. Returns fd or -errno. */
int gpu_dmabuf_alloc(const char *heap, size_t size);

int gpu_target_init(struct gpu *g, struct gpu_target *t, int fd,
                    uint32_t width, uint32_t height, uint32_t pitch,
                    uint32_t fourcc);
void gpu_target_fini(struct gpu *g, struct gpu_target *t);

/* Imports a dmabuf as an EGLImage and a texture bound to it (GL_TEXTURE_2D,
 * or GL_TEXTURE_EXTERNAL_OES if the driver insists). Returns the target or
 * 0 on failure. */
GLenum gpu_import_texture(struct gpu *g, int fd, uint32_t width, uint32_t height,
                          uint32_t pitch, uint32_t fourcc, EGLImageKHR *image,
                          GLuint *texture);
/* The same, with the pixels `offset` bytes into the buffer. */
GLenum gpu_import_texture_at(struct gpu *g, int fd, uint32_t width,
                             uint32_t height, uint32_t pitch, uint32_t offset,
                             uint32_t fourcc, EGLImageKHR *image, GLuint *texture);

/* After the frame's draws: a native fence fd that signals when the GPU is
 * done (flushes), or -1 (then the frame was finished synchronously). */
int gpu_fence(struct gpu *g);
/* CLOCK_MONOTONIC signal time of a signaled sync_file, or 0. */
uint64_t gpu_fence_time(int fd);

GLuint gpu_program(const char *vs, const char *fs);

#endif
