/* moon-gpu-proto-test: exercises moon-compositor's protocol v4 against the
 * running compositor, as a second client (it replaces Moon's connection:
 * stop Moon meanwhile, scripts/gpu/proto-test.sh). The panel must be on.
 *
 *   moon-gpu-proto-test SOCKET
 *
 * Checks the v4 handshake, MG_SURFACE_IDLE after the flip that takes a
 * surface off screen, MG_SURFACE_FAILED for a foreign memfd truncated
 * under the compositor (which must survive) and for an fd too small for
 * its surface, a sealed foreign memfd, freeing a surface on screen, and
 * that a v3 client gets no v4 messages. Exit status: failed checks. */
#define _GNU_SOURCE
#include "proto.h"

#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>
#include <linux/dma-heap.h>

static int sock = -1, failures;
static uint32_t serial;

#define CHECK(cond, ...)                                                       \
    do {                                                                       \
        int ok_ = (cond);                                                      \
        printf("%s: ", ok_ ? "ok  " : "FAIL");                                 \
        printf(__VA_ARGS__);                                                   \
        printf("\n");                                                          \
        failures += !ok_;                                                      \
    } while (0)

static uint64_t now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ts.tv_sec * 1000ull + ts.tv_nsec / 1000000;
}

static int open_link(const char *path) {
    int fd = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
    struct sockaddr_un a = {.sun_family = AF_UNIX};
    snprintf(a.sun_path, sizeof a.sun_path, "%s", path);
    if (fd < 0 || connect(fd, (struct sockaddr *)&a, sizeof a) < 0) {
        perror("connect");
        exit(100);
    }
    return fd;
}

static void send_msg(uint16_t type, const void *body, size_t len, int fd) {
    char buf[MG_MAX_PACKET];
    struct mg_header h = {.type = type, .length = (uint32_t)(sizeof h + len)};
    memcpy(buf, &h, sizeof h);
    memcpy(buf + sizeof h, body, len);
    struct iovec iov = {.iov_base = buf, .iov_len = sizeof h + len};
    union {
        char b[CMSG_SPACE(sizeof(int))];
        struct cmsghdr align;
    } u;
    struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1};
    if (fd >= 0) {
        m.msg_control = u.b;
        m.msg_controllen = sizeof u.b;
        struct cmsghdr *cm = CMSG_FIRSTHDR(&m);
        cm->cmsg_level = SOL_SOCKET;
        cm->cmsg_type = SCM_RIGHTS;
        cm->cmsg_len = CMSG_LEN(sizeof(int));
        memcpy(CMSG_DATA(cm), &fd, sizeof fd);
    }
    if (sendmsg(sock, &m, MSG_NOSIGNAL) < 0) {
        perror("send");
        exit(101);
    }
}

/* Waits up to `ms` for a message of `type` (any if 0); others are logged
 * into `seen`. Returns its type, or 0. */
static char msg[MG_MAX_PACKET];
static char seen[512];
static uint16_t wait_msg(uint16_t type, int ms) {
    uint64_t end = now_ms() + (uint64_t)ms;
    for (;;) {
        int left = (int)(end - now_ms());
        if (left < 0)
            return 0;
        struct pollfd p = {.fd = sock, .events = POLLIN};
        if (poll(&p, 1, left) <= 0)
            return 0;
        union {
            char b[CMSG_SPACE(4 * sizeof(int))];
            struct cmsghdr align;
        } u;
        struct iovec iov = {.iov_base = msg, .iov_len = sizeof msg};
        struct msghdr m = {.msg_iov = &iov, .msg_iovlen = 1,
                           .msg_control = u.b, .msg_controllen = sizeof u.b};
        ssize_t n = recvmsg(sock, &m, MSG_CMSG_CLOEXEC);
        if (n <= 0)
            return 0;
        for (struct cmsghdr *cm = CMSG_FIRSTHDR(&m); cm; cm = CMSG_NXTHDR(&m, cm))
            if (cm->cmsg_type == SCM_RIGHTS) {
                int fd;
                memcpy(&fd, CMSG_DATA(cm), sizeof fd);
                close(fd);
            }
        const struct mg_header *h = (const void *)msg;
        snprintf(seen + strlen(seen), sizeof seen - strlen(seen), " %u", h->type);
        if (!type || h->type == type)
            return h->type;
    }
}

static const void *body(void) { return msg + sizeof(struct mg_header); }

static void hello(uint32_t version) {
    send_msg(MG_HELLO, &(struct mg_hello){version}, sizeof(struct mg_hello), -1);
}

static void surface(uint32_t id, int fd, uint32_t w, uint32_t h, uint32_t flags) {
    struct mg_surface_shm s = {.id = id, .width = w, .height = h, .stride = w * 4,
                               .format = MG_XRGB8888, .flags = flags};
    send_msg(MG_SURFACE_SHM, &s, sizeof s, fd);
}

static void damage(uint32_t id, uint32_t w, uint32_t h) {
    struct {
        struct mg_damage d;
        struct mg_rect r;
    } m = {{.id = id, .serial = ++serial, .count = 1}, {0, 0, (int)w, (int)h}};
    send_msg(MG_SURFACE_DAMAGE, &m, sizeof m, -1);
}

/* A frame: black, and surface `id` (0: none) at 100,300 size w x h. */
static uint32_t frame(uint32_t id, float w, float h) {
    struct {
        struct mg_frame f;
        struct mg_node n[2];
    } m = {0};
    m.f.serial = ++serial;
    m.f.count = id ? 2 : 1;
    m.n[0] = (struct mg_node){.kind = MG_NODE_FILL, .dst_w = 1080, .dst_h = 2340,
                              .alpha = 1, .dim = 1, .color = 0x202020};
    m.n[1] = (struct mg_node){.kind = MG_NODE_IMAGE, .surface = id, .src_w = w,
                              .src_h = h, .dst_x = 100, .dst_y = 300, .dst_w = w,
                              .dst_h = h, .alpha = 1, .dim = 1};
    send_msg(MG_FRAME, &m, sizeof m.f + m.f.count * sizeof m.n[0], -1);
    return m.f.serial;
}

/* Waits for MG_PRESENTED of `s`. */
static int presented(uint32_t s) {
    uint64_t end = now_ms() + 1000;
    while (now_ms() < end)
        if (wait_msg(MG_PRESENTED, 200) &&
            ((const struct mg_presented *)body())->serial == s)
            return 1;
    return 0;
}

static int dmabuf(size_t len) {
    int heap = open("/dev/dma_heap/system", O_RDONLY | O_CLOEXEC);
    struct dma_heap_allocation_data a = {.len = len, .fd_flags = O_RDWR | O_CLOEXEC};
    if (heap < 0 || ioctl(heap, DMA_HEAP_IOCTL_ALLOC, &a) < 0) {
        perror("dma_heap");
        exit(102);
    }
    close(heap);
    return (int)a.fd;
}

static int memfd(size_t len, int sealing) {
    int fd = memfd_create("proto-test", MFD_CLOEXEC | (sealing ? MFD_ALLOW_SEALING : 0));
    if (fd < 0 || ftruncate(fd, (off_t)len) < 0) {
        perror("memfd");
        exit(103);
    }
    uint32_t *p = mmap(NULL, len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0);
    for (size_t i = 0; p != MAP_FAILED && i < len / 4; i++)
        p[i] = 0xff40a0ff;
    if (p != MAP_FAILED)
        munmap(p, len);
    return fd;
}

int main(int argc, char **argv) {
    if (argc != 2) {
        fprintf(stderr, "usage: %s SOCKET\n", argv[0]);
        return 2;
    }
    const uint32_t W = 256, H = 256;
    const size_t len = (size_t)W * 4 * H;

    sock = open_link(argv[1]);
    hello(4);
    CHECK(wait_msg(MG_WELCOME, 2000) && ((const struct mg_welcome *)body())->version == 4,
          "v4 hello welcomed as v4");
    CHECK(wait_msg(MG_MIRROR, 1000), "mirror offered");

    /* MG_SURFACE_IDLE: after the flip of the first frame without it. */
    int db = dmabuf(len);
    surface(101, db, W, H, MG_SURFACE_DMABUF | MG_SURFACE_REPORT_IDLE);
    uint32_t f1 = frame(101, W, H);
    CHECK(presented(f1), "frame with dmabuf surface 101 presented");
    seen[0] = 0;
    CHECK(!wait_msg(MG_SURFACE_IDLE, 300), "no IDLE while 101 is on screen (%s)", seen);
    uint32_t f2 = frame(0, 0, 0);
    seen[0] = 0;
    int got = wait_msg(MG_SURFACE_IDLE, 1000);
    CHECK(got && ((const struct mg_id *)body())->id == 101 && strstr(seen, " 67 "),
          "IDLE 101 after the next frame was presented (messages:%s)", seen);
    (void)f2;
    /* Named again, then taken off: another IDLE. */
    uint32_t f3 = frame(101, W, H);
    presented(f3);
    frame(0, 0, 0);
    CHECK(wait_msg(MG_SURFACE_IDLE, 1000) && ((const struct mg_id *)body())->id == 101,
          "a second IDLE 101 after it was shown again");

    /* A foreign, unsealed memfd truncated under the compositor. */
    int mf = memfd(len, 0);
    surface(102, mf, W, H, MG_SURFACE_FOREIGN);
    uint32_t f4 = frame(102, W, H);
    CHECK(presented(f4), "frame with foreign memfd 102 presented");
    damage(102, W, H);
    CHECK(wait_msg(MG_UPLOADED, 500), "damage of 102 uploaded");
    if (ftruncate(mf, 0))
        perror("ftruncate");
    damage(102, W, H);
    seen[0] = 0;
    got = wait_msg(MG_SURFACE_FAILED, 1000);
    CHECK(got && ((const struct mg_failed *)body())->id == 102 &&
              ((const struct mg_failed *)body())->reason == MG_FAIL_FAULT,
          "truncated 102: FAILED with MG_FAIL_FAULT");
    CHECK(wait_msg(MG_UPLOADED, 500) || strstr(seen, " 66"), "its damage still answered");
    uint32_t f5 = frame(102, W, H);
    CHECK(presented(f5), "the compositor survived and shows frames");
    close(mf);

    /* A sealed foreign memfd: read in place, nothing to report. */
    int sf = memfd(len, 1);
    CHECK(fcntl(sf, F_ADD_SEALS, F_SEAL_SHRINK) == 0, "sealed against shrinking");
    surface(103, sf, W, H, MG_SURFACE_FOREIGN);
    damage(103, W, H);
    seen[0] = 0;
    CHECK(wait_msg(MG_UPLOADED, 500) && !strstr(seen, " 70"), "sealed 103 uploaded, no FAILED");
    CHECK(presented(frame(103, W, H)), "frame with sealed 103 presented");
    close(sf);

    /* An fd smaller than its surface. */
    int small = memfd(1000, 0);
    surface(104, small, W, H, MG_SURFACE_FOREIGN);
    got = wait_msg(MG_SURFACE_FAILED, 500);
    CHECK(got && ((const struct mg_failed *)body())->id == 104 &&
              ((const struct mg_failed *)body())->reason == MG_FAIL_INVALID,
          "undersized 104: FAILED with MG_FAIL_INVALID");
    close(small);

    /* Freeing a surface that is on screen. */
    int db2 = dmabuf(len);
    surface(105, db2, W, H, MG_SURFACE_DMABUF);
    frame(105, W, H);
    struct mg_id fr = {105};
    send_msg(MG_SURFACE_FREE, &fr, sizeof fr, -1);
    CHECK(presented(frame(0, 0, 0)), "freed on-screen 105, frames continue");
    close(db2);
    close(db);
    close(sock);

    /* A v3 client: welcomed as v3, no v4 messages. */
    sock = open_link(argv[1]);
    hello(3);
    CHECK(wait_msg(MG_WELCOME, 2000) && ((const struct mg_welcome *)body())->version == 3,
          "v3 hello welcomed as v3");
    int bad = memfd(1000, 0);
    surface(106, bad, W, H, MG_SURFACE_FOREIGN | MG_SURFACE_REPORT_IDLE);
    seen[0] = 0;
    CHECK(!wait_msg(MG_SURFACE_FAILED, 500), "v3: no FAILED for a bad surface");
    close(bad);
    close(sock);
    printf("%d failed\n", failures);
    return failures;
}
