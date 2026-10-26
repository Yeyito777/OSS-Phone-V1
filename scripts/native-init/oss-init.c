/* OSS-Phone-V1 stage-2 init ("oss-init") -- containment, v4 (private rootfs).
 *
 * RECOVERY-SLOT ONLY. Packed with build-stage2-v4.sh + build-recovery-slot.sh
 * and tried once from the RECOVERY partition (CANARY-RUNBOOK.md). Never on BOOT:
 * flash-boot-inplace.sh refuses any image whose ramdisk /init differs.
 *
 * The kernel execs this as the real PID 1. It clones Android's whole init into
 * a new PID + mount namespace with a PRIVATE copy of the ramdisk in the normal
 * Magisk layout (/init = magiskinit), so Android sees getpid()==1 and does the
 * full stock bring-up unchanged, while OSS Phone keeps the real init seat.
 *
 * Why v4 (notes/stage2/V2-, V3-BOOTLOOP-ROOTCAUSE.md):
 *   v2: a tmpfs relay mounted in OUR namespace broke Android's SwitchRoot.
 *   v3: magiskinit ran from /init.magisk on the SHARED initramfs; it must be
 *       /init, and it rewrote the rootfs our fallback depended on.
 * So in v4:
 *   - oss-init makes NO mount in its own namespace (nothing for SwitchRoot to
 *     trip on, nothing to leak);
 *   - the container gets its own tmpfs holding a copy of the ramdisk with
 *     /init = magiskinit; everything magiskinit and init do lands there;
 *   - the fallback execs ONLY the stock init (/init.stock, static), through an
 *     fd held from the start, never magiskinit.
 *
 * Fallback rule (no mounts needed to tell the cases apart; we read /dev/kmsg):
 *   - Container fails BEFORE Samsung's sec_reboot module has registered its
 *     restart handler: kernel state is still clean (at most some modules are
 *     loaded, which init's modprobe tolerates as EEXIST), so exec the stock
 *     init as PID 1 = an ordinary stock boot in this boot. A reboot here would
 *     be unsafe: without sec_reboot the RECOVERY reason survives and the
 *     bootloader could pick the recovery slot again.
 *   - Container fails AFTER sec_reboot registered: dm devices / SELinux policy
 *     may already exist, so a second first-stage would not run cleanly. Do a
 *     controlled reboot(2) instead; sec_reboot writes the normal reason, so the
 *     phone boots BOOT (the known-good system).
 *   - The contained Android asks to reboot/power off: in a PID namespace that
 *     kills its init with SIGHUP/SIGINT; we perform the real one.
 *
 * Iron rule: no path in the PID-1 process returns or exits. Every path ends in
 * an exec of the stock init or a reboot, retried forever. Static, no heap.
 */

#define _GNU_SOURCE

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <linux/reboot.h>

extern char **environ;

#ifndef MAGISK_INIT
#define MAGISK_INIT "/init.magisk" /* real magiskinit, becomes the copy's /init */
#endif
#ifndef STOCK_INIT
#define STOCK_INIT "/init.stock"   /* static stock Android init (fallback)      */
#endif
#ifndef KMSG_READ
#define KMSG_READ "/dev/kmsg"
#endif
#ifndef STAGE_DIR
#define STAGE_DIR "/mnt"           /* existing ramdisk dir; tmpfs only in the child ns */
#endif
#ifndef LIVENESS_DEADLINE_MS
#define LIVENESS_DEADLINE_MS 60000
#endif
#define LIVE_MARKER "OSSPHONE_CONTAIN_LIVE"         /* overlay.d/oss-contain.rc */
#define REBOOT_SAFE_MARKER "restart handler"        /* "sec-reboot ...: register restart handler successfully" */
#define REBOOT_SAFE_SOURCE "sec-reboot"
#define POLL_MS 200

static void kmsg(const char *msg) {
#ifdef TEST_STDERR
    (void)!write(2, msg, strlen(msg));
#endif
    int fd = open("/dev/kmsg", O_WRONLY | O_CLOEXEC);
    if (fd < 0) return;
    (void)!write(fd, msg, strlen(msg));
    (void)close(fd);
}

static long now_ms(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (long)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}

static void sleep_ms(long ms) {
    struct timespec ts = {.tv_sec = ms / 1000, .tv_nsec = (ms % 1000) * 1000000L};
    while (nanosleep(&ts, &ts) < 0 && errno == EINTR) {}
}

/* ---- terminal actions: never return -------------------------------------- */

static void real_reboot(int poweroff) {
    for (;;) {
        sync();
        if (poweroff)
            reboot(LINUX_REBOOT_CMD_POWER_OFF);
        else
            syscall(SYS_reboot, LINUX_REBOOT_MAGIC1, LINUX_REBOOT_MAGIC2,
                    LINUX_REBOOT_CMD_RESTART2, "");
        sleep_ms(1000);
    }
}

/* Exec the stock init as the real PID 1 (an ordinary stock boot). Only the
 * stock init, never magiskinit. Held fd first (immune to any rootfs change),
 * then the path; if neither can be exec'd, reboot. */
static void exec_stock_init(int fd_stock, char **argv) {
    /* The fd is O_CLOEXEC so the container never inherits it. Clear that now:
     * execveat(AT_EMPTY_PATH) on a close-on-exec fd fails with ENOENT whenever
     * the binary is handed to an interpreter as /dev/fd/N (seen on the binfmt
     * test bench). Stock init merely inherits one extra open fd. */
    if (fd_stock >= 0) (void)fcntl(fd_stock, F_SETFD, 0);
    for (int attempt = 0; attempt < 3; attempt++) {
        if (fd_stock >= 0)
            syscall(SYS_execveat, fd_stock, "", argv, environ, AT_EMPTY_PATH);
        (void)execve(STOCK_INIT, argv, environ);
        sleep_ms(200);
    }
    kmsg("<3>oss-init: cannot exec the stock init; rebooting\n");
    real_reboot(0);
}

/* ---- kernel-log watcher --------------------------------------------------- */

struct watch {
    int fd;
    int live;        /* container reached second stage */
    int reboot_safe; /* sec_reboot registered its restart handler */
};

static void watch_kmsg(struct watch *w) {
    if (w->fd < 0) return;
    char buf[2048];
    for (;;) {
        ssize_t n = read(w->fd, buf, sizeof(buf) - 1);
        if (n > 0) {
            buf[n] = '\0';
            if (strstr(buf, LIVE_MARKER)) w->live = 1;
            if (strstr(buf, REBOOT_SAFE_SOURCE) && strstr(buf, REBOOT_SAFE_MARKER))
                w->reboot_safe = 1;
            continue;
        }
        if (n < 0 && errno == EPIPE) continue; /* ring overran: resync */
        return;                                /* EAGAIN: nothing new */
    }
}

/* The single decision point for a failed container. Never returns. */
static void fall_back(const char *why, struct watch *w, int fd_stock, char **argv) {
    watch_kmsg(w); /* pick up anything logged just before the failure */
    if (w->reboot_safe) {
        kmsg("<4>oss-init: container failed after sec_reboot loaded; controlled reboot to BOOT\n");
        (void)why;
        real_reboot(0);
    }
    kmsg("<4>oss-init: container failed early; exec stock init (uncontained stock boot)\n");
    exec_stock_init(fd_stock, argv);
}

/* ---- the container's private rootfs --------------------------------------- */

static char pathbuf_src[4096], pathbuf_dst[4096];

static int copy_file(const char *src, const char *dst, const struct stat *st) {
    int in = open(src, O_RDONLY | O_CLOEXEC);
    if (in < 0) return -1;
    int out = open(dst, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, st->st_mode & 07777);
    if (out < 0) { close(in); return -1; }
    char buf[65536];
    for (;;) {
        ssize_t n = read(in, buf, sizeof(buf));
        if (n == 0) break;
        if (n < 0) { if (errno == EINTR) continue; close(in); close(out); return -1; }
        for (ssize_t off = 0; off < n;) {
            ssize_t m = write(out, buf + off, (size_t)(n - off));
            if (m < 0) { if (errno == EINTR) continue; close(in); close(out); return -1; }
            off += m;
        }
    }
    close(in);
    if (fchown(out, st->st_uid, st->st_gid) < 0 && errno != EPERM && errno != EINVAL) { close(out); return -1; }
    if (fchmod(out, st->st_mode & 07777) < 0) { close(out); return -1; }
    return close(out);
}

/* Copy the tree under src (len-terminated in pathbuf_src) to dst, staying on
 * the root filesystem (skips anything mounted, incl. the staging tmpfs). */
static int copy_tree(size_t slen, size_t dlen, dev_t rootdev) {
    DIR *d = opendir(slen ? pathbuf_src : "/");
    if (!d) return -1;
    struct dirent *de;
    int rc = 0;
    while (rc == 0 && (de = readdir(d)) != NULL) {
        if (!strcmp(de->d_name, ".") || !strcmp(de->d_name, "..")) continue;
        size_t nl = strlen(de->d_name);
        if (slen + 1 + nl >= sizeof(pathbuf_src) || dlen + 1 + nl >= sizeof(pathbuf_dst)) { rc = -1; break; }
        pathbuf_src[slen] = '/'; memcpy(pathbuf_src + slen + 1, de->d_name, nl + 1);
        pathbuf_dst[dlen] = '/'; memcpy(pathbuf_dst + dlen + 1, de->d_name, nl + 1);
        struct stat st;
        if (lstat(pathbuf_src, &st) < 0) { rc = -1; break; }
        if (st.st_dev != rootdev) {
            /* A mountpoint (e.g. the staging tmpfs on /mnt): keep it as an empty
             * dir so init can mount on it, but do not descend. */
            if (S_ISDIR(st.st_mode) && mkdir(pathbuf_dst, 0755) < 0 && errno != EEXIST) rc = -1;
            pathbuf_src[slen] = 0; pathbuf_dst[dlen] = 0;
            continue;
        }
        if (S_ISDIR(st.st_mode)) {
            if (mkdir(pathbuf_dst, st.st_mode & 07777) < 0 && errno != EEXIST) rc = -1;
            else {
                (void)lchown(pathbuf_dst, st.st_uid, st.st_gid);
                (void)chmod(pathbuf_dst, st.st_mode & 07777);
                /* /dev, /proc, /sys stay empty: init creates/mounts them. */
                int skip_contents = (slen == 0) && (!strcmp(de->d_name, "dev") ||
                                    !strcmp(de->d_name, "proc") || !strcmp(de->d_name, "sys"));
                if (!skip_contents) rc = copy_tree(slen + 1 + nl, dlen + 1 + nl, rootdev);
            }
        } else if (S_ISREG(st.st_mode)) {
            rc = copy_file(pathbuf_src, pathbuf_dst, &st);
        } else if (S_ISLNK(st.st_mode)) {
            char target[4096];
            ssize_t n = readlink(pathbuf_src, target, sizeof(target) - 1);
            if (n < 0) rc = -1;
            else { target[n] = 0; if (symlink(target, pathbuf_dst) < 0) rc = -1;
                   else (void)lchown(pathbuf_dst, st.st_uid, st.st_gid); }
        } else {
            /* device nodes, fifos, sockets: recreate where permitted */
            if (mknod(pathbuf_dst, st.st_mode, st.st_rdev) < 0 && errno != EPERM) rc = -1;
        }
        pathbuf_src[slen] = 0; pathbuf_dst[dlen] = 0;
    }
    closedir(d);
    return rc;
}

/* Runs in the CHILD (container PID 1, own mount namespace). Builds the private
 * normal-Magisk rootfs and execs magiskinit as /init. Any failure _exits: the
 * parent sees it and falls back. Nothing here touches the parent namespace. */
#ifdef TEST_STDERR
#include <stdio.h>
#define CFAIL(code) do { dprintf(2, "[oss-init test] container step %d failed: %s\n", code, strerror(errno)); _exit(code); } while (0)
#else
#define CFAIL(code) _exit(code)
#endif

static void container_main(char **argv) {
    if (mount(NULL, "/", NULL, MS_REC | MS_PRIVATE, NULL) < 0) CFAIL(10);
    if (mount("tmpfs", STAGE_DIR, "tmpfs", 0, "mode=0755") < 0) CFAIL(11);
    struct stat rst;
    if (stat("/", &rst) < 0) CFAIL(12);
    pathbuf_src[0] = 0;                      /* "" + "/name" == "/name" */
    strcpy(pathbuf_dst, STAGE_DIR);
    if (copy_tree(0, strlen(STAGE_DIR), rst.st_dev) < 0) CFAIL(13);
    /* Normal Magisk layout: /init = magiskinit; our launch files removed. */
    if (unlink(STAGE_DIR "/init") < 0) CFAIL(14);
    if (rename(STAGE_DIR MAGISK_INIT, STAGE_DIR "/init") < 0) CFAIL(15);
    (void)unlink(STAGE_DIR STOCK_INIT);
    /* Make the copy this namespace's root, as init's own SwitchRoot does. */
    if (chdir(STAGE_DIR) < 0) CFAIL(16);
    if (mount(STAGE_DIR, "/", NULL, MS_MOVE, NULL) < 0) CFAIL(17);
    if (chroot(".") < 0) CFAIL(18);
    if (chdir("/") < 0) CFAIL(19);
    char *av[] = {"/init", NULL};
    execve("/init", argv && argv[0] ? argv : av, environ);
    _exit(127);
}

int main(int argc, char **argv) {
    (void)argc;
    /* /dev/kmsg may not exist yet on the bare rootfs; creating the node is a
     * file, not a mount. Stock first-stage mounts its own tmpfs over /dev. */
    (void)mknod("/dev/kmsg", S_IFCHR | 0600, makedev(1, 11));
    kmsg("<6>oss-init: OSS-Phone init is the real PID 1 (v4, private-rootfs containment)\n");

    int fd_stock = open(STOCK_INIT, O_RDONLY | O_CLOEXEC);
    struct watch w = {.fd = open(KMSG_READ, O_RDONLY | O_NONBLOCK | O_CLOEXEC)};

    if (fd_stock < 0) {
        /* Without the fallback in hand, don't attempt containment at all. */
        kmsg("<3>oss-init: no stock init fd; not containing\n");
        exec_stock_init(fd_stock, argv);
    }

    long pid = syscall(SYS_clone, (long)(CLONE_NEWPID | CLONE_NEWNS | SIGCHLD), 0L, 0L, 0L, 0L);
    if (pid == 0) container_main(argv);          /* never returns */
    if (pid < 0) fall_back("clone failed", &w, fd_stock, argv);

    long deadline = now_ms() + LIVENESS_DEADLINE_MS;
    for (;;) {
        int status = 0;
        pid_t r = waitpid(-1, &status, WNOHANG);
        if (r == (pid_t)pid || (r < 0 && errno == ECHILD)) {
#ifdef TEST_STDERR
            dprintf(2, "[oss-init test] container wait status 0x%x\n", status);
#endif
            if (r == (pid_t)pid && WIFSIGNALED(status) &&
                (WTERMSIG(status) == SIGHUP || WTERMSIG(status) == SIGINT)) {
                /* reboot(2) from inside the container's PID namespace. */
                watch_kmsg(&w);
                if (w.reboot_safe) {
                    kmsg("<6>oss-init: contained Android requested reboot/poweroff; performing it\n");
                    real_reboot(WTERMSIG(status) == SIGINT);
                }
            }
            fall_back("container exited", &w, fd_stock, argv);
        }
        int was_live = w.live;
        watch_kmsg(&w);
        if (w.live && !was_live)
            /* The only proof visible from inside the container (adb runs
             * there, so its /proc/1 is Android's init): this global line. */
            kmsg("<6>oss-init: container is live (Android second stage reached); "
                 "oss-init supervising as the real PID 1\n");
        if (!w.live && now_ms() > deadline) {
            kmsg("<4>oss-init: no liveness by the deadline; stopping the container\n");
            kill((pid_t)pid, SIGKILL);
            (void)waitpid((pid_t)pid, &status, 0);
            fall_back("hang", &w, fd_stock, argv);
        }
        sleep_ms(POLL_MS);
    }
}
