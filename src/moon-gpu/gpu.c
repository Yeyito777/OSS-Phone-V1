#include "gpu.h"

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <unistd.h>
#include <linux/dma-heap.h>
#include <linux/sync_file.h>

#ifndef EGL_LINUX_DMA_BUF_EXT
#define EGL_LINUX_DMA_BUF_EXT 0x3270
#define EGL_LINUX_DRM_FOURCC_EXT 0x3271
#define EGL_DMA_BUF_PLANE0_FD_EXT 0x3272
#define EGL_DMA_BUF_PLANE0_OFFSET_EXT 0x3273
#define EGL_DMA_BUF_PLANE0_PITCH_EXT 0x3274
#endif
#ifndef EGL_CONTEXT_PRIORITY_LEVEL_IMG
#define EGL_CONTEXT_PRIORITY_LEVEL_IMG 0x3100
#define EGL_CONTEXT_PRIORITY_HIGH_IMG 0x3101
#endif

static int has_ext(const char *list, const char *name) {
    size_t n = strlen(name);
    for (const char *p = list; p && (p = strstr(p, name)); p += n)
        if ((p == list || p[-1] == ' ') && (p[n] == ' ' || !p[n]))
            return 1;
    return 0;
}

int gpu_init(struct gpu *g) {
    memset(g, 0, sizeof *g);
    g->dpy = eglGetDisplay(EGL_DEFAULT_DISPLAY);
    EGLint major, minor;
    if (g->dpy == EGL_NO_DISPLAY || !eglInitialize(g->dpy, &major, &minor)) {
        fprintf(stderr, "gpu: eglInitialize failed: 0x%x\n", eglGetError());
        return -1;
    }
    const char *ext = eglQueryString(g->dpy, EGL_EXTENSIONS);
    fprintf(stderr, "gpu: EGL %d.%d %s\n", major, minor,
            eglQueryString(g->dpy, EGL_VENDOR));
    if (getenv("MOON_GPU_VERBOSE"))
        fprintf(stderr, "gpu: EGL extensions: %s\n", ext);
    eglBindAPI(EGL_OPENGL_ES_API);

    EGLint cfg_attr[] = {EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT_KHR,
                         EGL_SURFACE_TYPE, EGL_PBUFFER_BIT, EGL_RED_SIZE, 8,
                         EGL_GREEN_SIZE, 8, EGL_BLUE_SIZE, 8, EGL_NONE};
    EGLConfig cfg;
    EGLint n = 0;
    if (!eglChooseConfig(g->dpy, cfg_attr, &cfg, 1, &n) || n < 1) {
        fprintf(stderr, "gpu: no ES3 config\n");
        return -1;
    }
    EGLint ctx_attr[] = {EGL_CONTEXT_CLIENT_VERSION, 3,
                         EGL_CONTEXT_PRIORITY_LEVEL_IMG,
                         EGL_CONTEXT_PRIORITY_HIGH_IMG, EGL_NONE};
    if (!has_ext(ext, "EGL_IMG_context_priority"))
        ctx_attr[2] = EGL_NONE;
    g->ctx = eglCreateContext(g->dpy, cfg, EGL_NO_CONTEXT, ctx_attr);
    if (g->ctx == EGL_NO_CONTEXT) {
        fprintf(stderr, "gpu: eglCreateContext: 0x%x\n", eglGetError());
        return -1;
    }
    if (ctx_attr[2] != EGL_NONE) {
        EGLint prio = 0;
        eglQueryContext(g->dpy, g->ctx, EGL_CONTEXT_PRIORITY_LEVEL_IMG, &prio);
        g->high_priority = prio == EGL_CONTEXT_PRIORITY_HIGH_IMG;
    }
    if (!eglMakeCurrent(g->dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, g->ctx)) {
        EGLint pb_attr[] = {EGL_WIDTH, 1, EGL_HEIGHT, 1, EGL_NONE};
        g->pbuf = eglCreatePbufferSurface(g->dpy, cfg, pb_attr);
        if (!eglMakeCurrent(g->dpy, g->pbuf, g->pbuf, g->ctx)) {
            fprintf(stderr, "gpu: eglMakeCurrent: 0x%x\n", eglGetError());
            return -1;
        }
    }
    g->create_image =
        (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    g->destroy_image =
        (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
    g->create_sync =
        (PFNEGLCREATESYNCKHRPROC)eglGetProcAddress("eglCreateSyncKHR");
    g->destroy_sync =
        (PFNEGLDESTROYSYNCKHRPROC)eglGetProcAddress("eglDestroySyncKHR");
    g->dup_fence = (PFNEGLDUPNATIVEFENCEFDANDROIDPROC)eglGetProcAddress(
        "eglDupNativeFenceFDANDROID");
    g->image_target_rb = (void (*)(GLenum, void *))eglGetProcAddress(
        "glEGLImageTargetRenderbufferStorageOES");
    g->image_target_tex = (void (*)(GLenum, void *))eglGetProcAddress(
        "glEGLImageTargetTexture2DOES");
    if (!has_ext(ext, "EGL_ANDROID_native_fence_sync"))
        g->dup_fence = NULL;
    fprintf(stderr, "gpu: GL %s | %s | %s; fences %s, priority %s\n",
            glGetString(GL_VERSION), glGetString(GL_RENDERER),
            has_ext(ext, "EGL_EXT_image_dma_buf_import") ? "dma_buf import"
                                                          : "no dma_buf ext",
            g->dup_fence ? "native" : "glFinish",
            g->high_priority ? "high" : "default");
    if (!g->create_image || !g->image_target_rb) {
        fprintf(stderr, "gpu: EGLImage entry points missing\n");
        return -1;
    }
    return 0;
}

void gpu_fini(struct gpu *g) {
    if (g->dpy == EGL_NO_DISPLAY)
        return;
    eglMakeCurrent(g->dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    if (g->pbuf)
        eglDestroySurface(g->dpy, g->pbuf);
    if (g->ctx)
        eglDestroyContext(g->dpy, g->ctx);
    eglTerminate(g->dpy);
}

int gpu_dmabuf_alloc(const char *heap, size_t size) {
    char path[96];
    snprintf(path, sizeof path, "/dev/dma_heap/%s", heap);
    int h = open(path, O_RDONLY | O_CLOEXEC);
    if (h < 0)
        return -errno;
    struct dma_heap_allocation_data a = {.len = size,
                                         .fd_flags = O_RDWR | O_CLOEXEC};
    int r = ioctl(h, DMA_HEAP_IOCTL_ALLOC, &a);
    int e = errno;
    close(h);
    return r < 0 ? -e : (int)a.fd;
}

int gpu_target_init(struct gpu *g, struct gpu_target *t, int fd,
                    uint32_t width, uint32_t height, uint32_t pitch,
                    uint32_t fourcc) {
    memset(t, 0, sizeof *t);
    t->fd = fd;
    t->width = width;
    t->height = height;
    t->pitch = pitch;
    t->fourcc = fourcc;
    EGLint attr[] = {EGL_WIDTH,
                     (EGLint)width,
                     EGL_HEIGHT,
                     (EGLint)height,
                     EGL_LINUX_DRM_FOURCC_EXT,
                     (EGLint)fourcc,
                     EGL_DMA_BUF_PLANE0_FD_EXT,
                     fd,
                     EGL_DMA_BUF_PLANE0_OFFSET_EXT,
                     0,
                     EGL_DMA_BUF_PLANE0_PITCH_EXT,
                     (EGLint)pitch,
                     EGL_NONE};
    t->image = g->create_image(g->dpy, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT,
                               NULL, attr);
    if (t->image == EGL_NO_IMAGE_KHR) {
        fprintf(stderr, "gpu: dma_buf EGLImage: 0x%x\n", eglGetError());
        return -1;
    }
    glGenRenderbuffers(1, &t->rb);
    glBindRenderbuffer(GL_RENDERBUFFER, t->rb);
    g->image_target_rb(GL_RENDERBUFFER, t->image);
    glGenFramebuffers(1, &t->fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, t->fbo);
    glFramebufferRenderbuffer(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0,
                              GL_RENDERBUFFER, t->rb);
    GLenum st = glCheckFramebufferStatus(GL_FRAMEBUFFER);
    if (st != GL_FRAMEBUFFER_COMPLETE) {
        fprintf(stderr, "gpu: dmabuf framebuffer incomplete: 0x%x\n", st);
        return -1;
    }
    return 0;
}

void gpu_target_fini(struct gpu *g, struct gpu_target *t) {
    if (t->fbo)
        glDeleteFramebuffers(1, &t->fbo);
    if (t->rb)
        glDeleteRenderbuffers(1, &t->rb);
    if (t->image)
        g->destroy_image(g->dpy, t->image);
    if (t->fd >= 0)
        close(t->fd);
    memset(t, 0, sizeof *t);
    t->fd = -1;
}

int gpu_fence(struct gpu *g) {
    if (g->dup_fence) {
        EGLint attr[] = {EGL_SYNC_NATIVE_FENCE_FD_ANDROID,
                         EGL_NO_NATIVE_FENCE_FD_ANDROID, EGL_NONE};
        EGLSyncKHR s =
            g->create_sync(g->dpy, EGL_SYNC_NATIVE_FENCE_ANDROID, attr);
        if (s != EGL_NO_SYNC_KHR) {
            glFlush();
            int fd = g->dup_fence(g->dpy, s);
            g->destroy_sync(g->dpy, s);
            if (fd >= 0)
                return fd;
        }
    }
    glFinish();
    return -1;
}

uint64_t gpu_fence_time(int fd) {
    struct sync_fence_info fi[4];
    struct sync_file_info info = {.num_fences = 4,
                                  .sync_fence_info = (uint64_t)(uintptr_t)fi};
    if (ioctl(fd, SYNC_IOC_FILE_INFO, &info) < 0 || info.status != 1)
        return 0;
    uint64_t t = 0;
    for (uint32_t i = 0; i < info.num_fences && i < 4; i++)
        if (fi[i].timestamp_ns > t)
            t = fi[i].timestamp_ns;
    return t;
}

static GLuint shader(GLenum type, const char *src) {
    GLuint s = glCreateShader(type);
    glShaderSource(s, 1, &src, NULL);
    glCompileShader(s);
    GLint ok = 0;
    glGetShaderiv(s, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char log[1024];
        glGetShaderInfoLog(s, sizeof log, NULL, log);
        fprintf(stderr, "gpu: shader: %s\n", log);
        return 0;
    }
    return s;
}

GLuint gpu_program(const char *vs, const char *fs) {
    GLuint v = shader(GL_VERTEX_SHADER, vs), f = shader(GL_FRAGMENT_SHADER, fs);
    if (!v || !f)
        return 0;
    GLuint p = glCreateProgram();
    glAttachShader(p, v);
    glAttachShader(p, f);
    glLinkProgram(p);
    glDeleteShader(v);
    glDeleteShader(f);
    GLint ok = 0;
    glGetProgramiv(p, GL_LINK_STATUS, &ok);
    if (!ok) {
        char log[1024];
        glGetProgramInfoLog(p, sizeof log, NULL, log);
        fprintf(stderr, "gpu: link: %s\n", log);
        return 0;
    }
    return p;
}
