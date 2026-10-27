/* OSS-Phone-V1 stage-2 init ("oss-init") -- containment, v4 (private rootfs).
 *
 * RECOVERY-SLOT ONLY. Packed with build-slot-image.sh containment (which uses
 * canary/build-recovery-slot.sh). Its first action loads oss-rebootmode-clear.ko
 * (rebootmode-clear.h); if that fails it does not contain and execs stock init.
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
#include <stdlib.h>
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

#include "rebootmode-clear.h"
#include "bootcount-misc.h"
#include <sched.h>
#include <sys/mount.h>

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

/* A write fd opened once at startup. After the container loads its enforcing
 * SELinux policy, opening /dev/kmsg afresh is denied for the kernel domain, but
 * writing an already-open fd needs only `write`, which the kernel domain has on
 * the node. So keep this open for the life of PID 1 and log through it. */
static int g_kmsg_fd = -1;

static void kmsg(const char *msg) {
#ifdef TEST_STDERR
    (void)!write(2, msg, strlen(msg));
#endif
    if (g_kmsg_fd >= 0) {
        (void)!write(g_kmsg_fd, msg, strlen(msg));
        return;
    }
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

static void bc_helper_kill(void);

static void real_reboot(int poweroff) {
    bc_helper_kill();
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
static void exec_stock_init(int fd_stock, char **argv, int allow_reboot) {
    bc_helper_kill();
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
    if (allow_reboot) {
        kmsg("<3>oss-init: cannot exec the stock init; rebooting\n");
        real_reboot(0);
    }
    /* The PMU may still select recovery: a reboot could loop. Stay put; the
     * Download-mode escape (CANARY-RUNBOOK.md) recovers the phone. */
    kmsg("<3>oss-init: cannot exec the stock init and must not reboot; halting\n");
    for (;;) pause();
}

/* ---- boot-attempt counter in misc (BOOT-promotion safety) -----------------
 * notes/stage2/PROMOTION-PLAN.md §2; record format docs/bootcount-misc-record.md;
 * implementation bootcount-misc.h (the only writer).
 *
 * Timing: the misc partition does not exist at oss-init's first action. UFS
 * (ufs-exynos-core.ko, needing 29 modules plus regulator/PHY providers) is
 * loaded by the container's stock first stage; misc appears ~1.9 s, first
 * stage switches root ~4.7 s (v5 on-device log). So a helper process, in its
 * OWN private mount namespace (oss-init's namespace stays mount-free), waits for
 * PARTNAME=misc in sysfs, FREEZES the container (SIGSTOP via pidfd), bumps the
 * record, and reports; oss-init then thaws the container or kills it and skips
 * containment. The freeze happens during module loading, before any dm device
 * or mount exists, so the skip path sees the same state as an early container
 * failure (already handled by fall_back).
 *
 * Decision (after the bump):
 *   valid record, new count >= BC_MAX              -> skip containment
 *   OSS_PROMOTED and no trustworthy count (absent, damaged, foreign, I/O or
 *     verify error, wrong size, ambiguous)          -> skip containment
 *   otherwise (incl. a normal boot with no/foreign store) -> contain as usual
 * misc never appearing within the deadline: contain as usual in both modes (if
 * UFS never comes up, no boot can mount anything; killing a container that is
 * already past first stage would not help).
 * Skip = today's proven boot: magiskinit as /init (as oss-pid1 does), the stock
 * init via the held fd if that exec fails. A skipped boot carries no
 * /debug_ramdisk/oss-contained marker, so moon-boot-confirm.sh does not clear
 * the count: the boot stays skipped until someone clears it on purpose. */
#ifndef OSS_PROMOTED
#define OSS_PROMOTED 0      /* 1 only in a BOOT-promotion build */
#endif
#ifndef BC_FIND_DEADLINE_MS
#define BC_FIND_DEADLINE_MS 15000
#endif
#ifndef SYS_pidfd_open
#define SYS_pidfd_open 434
#endif
#ifndef SYS_pidfd_send_signal
#define SYS_pidfd_send_signal 424
#endif

struct bc_result {
    int done, status, count, stopped, wrote, found;  /* found: 0, -1 timeout, -2 ambiguous, -3 size */
};
static pid_t g_bc_helper = -1;

static void bc_helper_kill(void) {
    if (g_bc_helper > 0) {
        (void)kill(g_bc_helper, SIGKILL);
        (void)waitpid(g_bc_helper, 0, 0);
        g_bc_helper = -1;
    }
}

static int ctr_signal(pid_t ctr, int pfd, int sig) {
    if (pfd >= 0 && syscall(SYS_pidfd_send_signal, pfd, sig, (void *)0, 0) == 0) return 0;
    return kill(ctr, sig);
}

static void bc_helper_main(pid_t ctr, int pfd, int out) {
    struct bc_result res = {.done = 1, .status = BC_NOSTORE, .found = -1};
#ifdef TEST_SYSFS
    const char *sysroot = TEST_SYSFS, *node = TEST_MISC_NODE;
#else
    const char *sysroot = "/sys", *node = "/mnt/oss-misc";
    if (unshare(CLONE_NEWNS) < 0 || mount(NULL, "/", NULL, MS_REC | MS_PRIVATE, NULL) < 0 ||
        mount("sysfs", "/sys", "sysfs", 0, NULL) < 0) { res.status = BC_IO; goto report; }
#endif
    unsigned maj = 0, min = 0; char dn[64];
    long end = now_ms() + BC_FIND_DEADLINE_MS;
    int f;
    for (;;) {
        f = bc_find_misc(sysroot, &maj, &min, dn, sizeof dn);
        if (f != -1 || now_ms() > end) break;
        sleep_ms(50);
    }
    res.found = f;
    if (f != 0) goto report;
    if (ctr_signal(ctr, pfd, SIGSTOP) == 0) res.stopped = 1;     /* freeze while we decide */
#ifndef TEST_SYSFS
    if (mount("tmpfs", "/mnt", "tmpfs", 0, "mode=0700") < 0 ||
        mknod(node, S_IFBLK | 0600, makedev(maj, min)) < 0) { res.status = BC_IO; goto report; }
#endif
    {
        int direct, fd = bc_open(node, &direct);
        if (fd < 0) { res.status = fd == -2 ? BC_NOSTORE : BC_IO; if (fd == -2) res.found = -3; goto report; }
        struct bc_state st = bc_op(fd, BC_BUMP, direct, !OSS_PROMOTED, &res.wrote);
        (void)close(fd);
        res.status = st.status; res.count = (int)st.count;
    }
report:
    (void)!write(out, &res, sizeof res);
    _exit(0);
}

/* Magiskinit as /init: today's proven boot (oss-pid1's path). Never returns. */
static void skip_containment(int fd_stock, char **argv, const char *why) {
    char line[160];
    snprintf(line, sizeof line, "<4>oss-init: skipping containment (%s); booting magiskinit as /init\n", why);
    kmsg(line);
    bc_helper_kill();
    if (rename(MAGISK_INIT, "/init") == 0) {
        execve("/init", argv, environ);
        kmsg("<3>oss-init: exec of magiskinit failed; exec stock init\n");
    } else {
        kmsg("<3>oss-init: cannot put magiskinit at /init; exec stock init\n");
    }
    exec_stock_init(fd_stock, argv, 1);
}

/* Act on the helper's report. Returns only if the container goes on. A
 * container that is already live (past first stage: dm devices, mounts) is
 * never killed for the count: that would collide with its state. On the phone
 * the report comes at ~1.9 s and liveness at ~6 s, so this only guards an
 * abnormally late misc. */
static void bc_decide(const struct bc_result *r, pid_t ctr, int pfd, int fd_stock, char **argv, int live) {
    char line[200];
    snprintf(line, sizeof line,
             "<6>oss-init: bootcount status=%s count=%d found=%d wrote=%d froze=%d promoted=%d\n",
             bc_status_name(r->status), r->count, r->found, r->wrote, r->stopped, OSS_PROMOTED);
    kmsg(line);
    const char *why = 0;
    if (r->status == BC_OK && r->count >= BC_MAX)
        why = "consecutive failed boots reached the limit";
    else if (OSS_PROMOTED && r->status != BC_OK && !(r->status == BC_NOSTORE && r->found == -1))
        why = "promoted boot without a trustworthy boot count";
    if (why && live) {
        snprintf(line, sizeof line, "<4>oss-init: would skip containment (%s), but the container is "
                 "already live; not interrupting it\n", why);
        kmsg(line);
        why = 0;
    }
    if (why) {
        (void)ctr_signal(ctr, pfd, SIGKILL);
        (void)waitpid(ctr, 0, 0);
        skip_containment(fd_stock, argv, why);
    }
    if (r->stopped) (void)ctr_signal(ctr, pfd, SIGCONT);
}

/* `oss-init --bootcount show|clear|init|bump`, from Android userspace (never as
 * PID 1). Finds misc in /sys, opens /dev/block/<DEVNAME> after checking it is
 * the same block device. OSS_BC_SYSROOT / OSS_BC_NODE override both (bench). */
static int bootcount_cli(const char *opname) {
    int op = !strcmp(opname, "show") ? BC_SHOW : !strcmp(opname, "clear") ? BC_CLEAR :
             !strcmp(opname, "init") ? BC_INIT : !strcmp(opname, "bump") ? BC_BUMP : -1;
    if (op < 0) { fprintf(stderr, "usage: oss-init --bootcount show|clear|init|bump\n"); return 2; }
    const char *sysroot = getenv("OSS_BC_SYSROOT") ? getenv("OSS_BC_SYSROOT") : "/sys";
    const char *forced = getenv("OSS_BC_NODE");
    unsigned maj, min; char dn[64], node[96];
    int f = bc_find_misc(sysroot, &maj, &min, dn, sizeof dn);
    if (f != 0) {
        printf("bootcount: status=nostore (%s)\n", f == -2 ? "ambiguous PARTNAME=misc" :
               f == -3 ? "misc is not 1 MiB" : "no PARTNAME=misc");
        return 2 + BC_NOSTORE;
    }
    if (forced) snprintf(node, sizeof node, "%s", forced);
    else {
        /* Android: /dev/block/<DEVNAME>; desktop Linux: /dev/<DEVNAME>. Either
         * way it must be exactly the block device sysfs named. */
        static const char *dirs[] = {"/dev/block/", "/dev/"};
        int ok = 0;
        for (int i = 0; i < 2 && !ok; i++) {
            snprintf(node, sizeof node, "%s%s", dirs[i], dn);
            struct stat sb;
            ok = stat(node, &sb) == 0 && S_ISBLK(sb.st_mode) && sb.st_rdev == makedev(maj, min);
        }
        if (!ok) {
            printf("bootcount: status=nostore (no block node for %s is %u:%u)\n", dn, maj, min);
            return 2 + BC_NOSTORE;
        }
    }
    int direct, fd = bc_open(node, &direct);
    if (fd < 0) { printf("bootcount: status=%s (open %s)\n", fd == -2 ? "nostore" : "io", node); return 2 + (fd == -2 ? BC_NOSTORE : BC_IO); }
    int wrote;
    struct bc_state st = bc_op(fd, op, direct, 1, &wrote);
    (void)close(fd);
    printf("bootcount: status=%s slot=%s seq=%u count=%u wrote=%d slotA=%d slotB=%d direct=%d\n",
           bc_status_name(st.status), st.cur == 0 ? "A" : st.cur == 1 ? "B" : "-",
           st.seq, st.count, wrote, st.cls[0], st.cls[1], direct);
    return (st.status == BC_OK || st.status == BC_NONE) ? 0 : 2 + st.status;
}

/* ---- kernel-log watcher --------------------------------------------------- */

struct watch {
    int fd;
    int live;        /* container reached second stage */
    int reboot_safe; /* sec_reboot registered its restart handler */
    int denied;      /* reads now denied: the container loaded its SELinux policy */
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
        if (n < 0 && (errno == EACCES || errno == EPERM)) {
            /* The container loaded its enforcing SELinux policy, which revalidates
             * our open fd and denies further reads (avc: kernel -> rootfs chr_file
             * read). That load happens in second-stage init, after first stage
             * loaded sec_reboot, so the container is BOTH live and reboot-safe.
             * Record that, stop reading (don't flood the audit log), and drop the
             * fd; the write fd (opened O_WRONLY) keeps working for logging. */
            w->live = 1;
            w->reboot_safe = 1;
            w->denied = 1;
            (void)close(w->fd);
            w->fd = -1;
            return;
        }
        return;                                /* EAGAIN: nothing new */
    }
}

/* The single decision point for a failed container. Never returns. */
static void fall_back(const char *why, struct watch *w, int fd_stock, char **argv) {
    bc_helper_kill();
    watch_kmsg(w); /* pick up anything logged just before the failure */
    if (w->reboot_safe) {
        kmsg("<4>oss-init: container failed after sec_reboot loaded; controlled reboot to BOOT\n");
        (void)why;
        real_reboot(0);
    }
    kmsg("<4>oss-init: container failed early; exec stock init (uncontained stock boot)\n");
    exec_stock_init(fd_stock, argv, 1);
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
    /* Marker for Android: magiskinit copies overlay.d/sbin to /debug_ramdisk, so a
     * contained boot shows /debug_ramdisk/oss-contained (moon-boot-confirm.sh
     * clears the boot count only then; a skipped boot has no marker). */
    (void)mkdir(STAGE_DIR "/overlay.d", 0750);
    (void)mkdir(STAGE_DIR "/overlay.d/sbin", 0750);
    {
        int mf = open(STAGE_DIR "/overlay.d/sbin/oss-contained", O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0644);
        if (mf >= 0) { (void)!write(mf, "oss-init v7 containment\n", 24); (void)close(mf); }
    }
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
    if (argc >= 3 && !strcmp(argv[1], "--bootcount") && getpid() != 1)
        return bootcount_cli(argv[2]);
    /* FIRST, before anything that can fail: select a NORMAL boot in the PMU,
     * so any later panic, hang or reset returns to BOOT, not to this slot. */
    int clear_err = load_rebootmode_clear();
    /* /dev/kmsg may not exist yet on the bare rootfs; creating the node is a
     * file, not a mount. Stock first-stage mounts its own tmpfs over /dev. */
    (void)mknod("/dev/kmsg", S_IFCHR | 0600, makedev(1, 11));
    g_kmsg_fd = open("/dev/kmsg", O_WRONLY | O_CLOEXEC);
    kmsg("<6>oss-init: OSS-Phone init is the real PID 1 (v7, private-rootfs containment)\n");

    int fd_stock = open(STOCK_INIT, O_RDONLY | O_CLOEXEC);
    if (clear_err) {
        char line[160];
        kmsg(errno_line(line, sizeof(line),
             "<3>oss-init: oss-rebootmode-clear did not load; not containing, exec stock init without a reboot fallback; errno=", clear_err));
        exec_stock_init(fd_stock, argv, 0);
    }
    kmsg("<6>oss-init: PMU reboot state set to NORMAL (oss-rebootmode-clear loaded)\n");

    struct watch w = {.fd = open(KMSG_READ, O_RDONLY | O_NONBLOCK | O_CLOEXEC)};

    if (fd_stock < 0) {
        /* Without the fallback in hand, don't attempt containment at all. */
        kmsg("<3>oss-init: no stock init fd; not containing\n");
        exec_stock_init(fd_stock, argv, 1);
    }

    long pid = syscall(SYS_clone, (long)(CLONE_NEWPID | CLONE_NEWNS | SIGCHLD), 0L, 0L, 0L, 0L);
    if (pid == 0) container_main(argv);          /* never returns */
    if (pid < 0) fall_back("clone failed", &w, fd_stock, argv);

    /* Boot-attempt counter: helper waits for misc, freezes the container, bumps. */
    int pfd = (int)syscall(SYS_pidfd_open, pid, 0);
    int bc_pipe[2] = {-1, -1}, bc_have = 0;
    struct bc_result bres = {0};
    if (pipe2(bc_pipe, O_CLOEXEC | O_NONBLOCK) == 0) {
        pid_t h = fork();
        if (h == 0) { (void)close(bc_pipe[0]); bc_helper_main((pid_t)pid, pfd, bc_pipe[1]); }
        if (h > 0) g_bc_helper = h;
        (void)close(bc_pipe[1]);
        if (h < 0) { (void)close(bc_pipe[0]); bc_pipe[0] = -1; }
    }
    if (g_bc_helper <= 0) {
        bres.done = 1; bres.status = BC_IO; bres.found = -1;
        bc_decide(&bres, (pid_t)pid, pfd, fd_stock, argv, 0);   /* no helper: promoted escalates */
        bc_have = 1;
    }

    long deadline = now_ms() + LIVENESS_DEADLINE_MS;
    for (;;) {
        int status = 0;
        if (!bc_have && bc_pipe[0] >= 0 && read(bc_pipe[0], &bres, sizeof bres) == (ssize_t)sizeof bres) {
            bc_have = 1;
            watch_kmsg(&w);
            bc_decide(&bres, (pid_t)pid, pfd, fd_stock, argv, w.live);
        }
        pid_t r = waitpid(-1, &status, WNOHANG);
        if (r > 0 && r == g_bc_helper) {
            g_bc_helper = -1;
            if (!bc_have && read(bc_pipe[0], &bres, sizeof bres) != (ssize_t)sizeof bres) {
                /* The helper died without a report; it may have frozen the container. */
                bres = (struct bc_result){.done = 1, .status = BC_IO, .found = -1, .stopped = 1};
            }
            if (!bc_have) { bc_have = 1; watch_kmsg(&w); bc_decide(&bres, (pid_t)pid, pfd, fd_stock, argv, w.live); }
            continue;
        }
        if (r == (pid_t)pid || (r < 0 && errno == ECHILD)) {
#ifdef TEST_STDERR
            dprintf(2, "[oss-init test] container wait status 0x%x\n", status);
#endif
            /* A reboot/power-off requested INSIDE the container is turned by the
             * kernel (kernel/pid_namespace.c reboot_pid_ns + zap_pid_ns_processes)
             * into pid_ns->reboot = SIGHUP for reboot/restart, SIGINT for
             * power-off/halt, carried on the container PID 1's wait status. When
             * PID 1 itself called reboot(2) that status is WIFSIGNALED with that
             * signal (measured: 0x1 / 0x2 on this kernel); some code paths
             * surface it as WIFEXITED with the same number instead -- accept
             * both. If a non-PID-1 process in the container called reboot(2) the
             * kernel SIGKILLs PID 1 and the reboot/power-off distinction is lost
             * (status reports SIGKILL): that drops through to fall_back, which
             * still reboots safely to BOOT. */
            int req = -1;
            if (r == (pid_t)pid) {
                if (WIFSIGNALED(status) &&
                    (WTERMSIG(status) == SIGHUP || WTERMSIG(status) == SIGINT))
                    req = WTERMSIG(status);
                else if (WIFEXITED(status) &&
                    (WEXITSTATUS(status) == SIGHUP || WEXITSTATUS(status) == SIGINT))
                    req = WEXITSTATUS(status);
            }
            if (req != -1) {
                int poweroff = (req == SIGINT);
                kmsg(poweroff
                     ? "<6>oss-init: contained Android requested power-off; powering off\n"
                     : "<6>oss-init: contained Android requested reboot; rebooting to BOOT\n");
                real_reboot(poweroff);   /* PMU is already NORMAL; never returns */
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
            if (kill((pid_t)pid, SIGKILL) != 0) {
                /* The kill was refused (e.g. SELinux kernel -> init). Do not block
                 * in waitpid forever; the PMU clear already succeeded, so a reboot
                 * lands on BOOT (measured on device). Reboot now. */
                kmsg("<4>oss-init: cannot signal the container; rebooting to BOOT now\n");
                real_reboot(0);
            }
            (void)waitpid((pid_t)pid, &status, 0);
            fall_back("hang", &w, fd_stock, argv);
        }
        sleep_ms(POLL_MS);
    }
}
