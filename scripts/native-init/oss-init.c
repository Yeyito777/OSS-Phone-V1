/* OSS-Phone-V1 stage-2 init ("oss-init") -- containment, v3.
 *
 * On the OSS Phone kernel (CONFIG_PID_NS=y, CONFIG_DEVTMPFS=y, DEFEX removed)
 * this binary is the ramdisk /init, so the kernel execs it as the REAL PID 1
 * and it stays PID 1. Android's entire init (first + second stage, via the
 * existing magiskinit -> stock-init -> Moon stack) runs as its child inside a
 * new PID + mount namespace, so Android sees getpid()==1 in its namespace and
 * performs Samsung's full, proven bring-up unchanged. OSS Phone owns the real
 * init seat; Android is contained.
 *
 * --- Why v3 (the v2 boot loop) ---
 * v2 mounted a tmpfs relay at /oss-run in oss-init's own mount namespace before
 * cloning. Android's first-stage init SwitchRoot moves EVERY mount under / into
 * the new (read-only) system root; it could not create /system/oss-run, so
 * `Unable to move mount at 'oss-run'` -> LOG(FATAL) -> InitFatalReboot. That
 * killed the CONTAINED init, and because the /oss-run mount lived in oss-init's
 * namespace it was inherited by the uncontained fallback too, which hit the
 * exact same SwitchRoot failure as the real PID 1 -> real reboot -> loop with
 * no USB. Two lessons, both fixed here:
 *   1. oss-init must add NO mount that survives into the handoff. v3 mounts
 *      nothing. Liveness is observed through the global kernel log (/dev/kmsg),
 *      which needs no mount and is visible across namespaces.
 *   2. The fallback must run from a PRISTINE namespace. Because the container is
 *      cloned with CLONE_NEWNS, everything it mounts lives in ITS namespace and
 *      is torn down by the kernel when it dies; oss-init's own namespace stays
 *      exactly as the kernel handed it over, so re-exec'ing the stock stack is a
 *      clean, uncontained boot -- and it stays in THIS boot (no reboot), so a
 *      containment failure can never become a reset loop.
 *
 * --- The iron rule: oss-init can never exit or return ---
 * There is no reachable `return` or `_exit` in the PID-1 (parent) path. Every
 * path ends either in an exec of the stock stack or, if even that fails, in an
 * infinite loop that re-tries the exec and then issues a real reboot. The only
 * `_exit` is in the cloned child (a separate process), whose death the parent
 * observes and answers with the uncontained fallback.
 *
 * Keep this tiny, static and syscall-minimal: a crash here is a PID-1 crash.
 */

#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <linux/reboot.h>

extern char **environ;

#define PRIMARY_INIT "/init.magisk" /* magiskinit -> stock init -> Moon */
#define STOCK_INIT "/init.stock"    /* byte-exact stock Android init     */
/* The contained second-stage init emits this to the global kernel log (see
 * overlay.d/oss-contain.rc). Seeing it means the container is past first-stage
 * SwitchRoot and watchdogd is starting, so the early-hang breaker can stand
 * down. The string must match the overlay rc exactly. */
#define LIVENESS_MARKER "OSSPHONE_CONTAIN_LIVE"
#define LIVENESS_DEADLINE_MS 60000 /* kill+fallback if no liveness by here */
#define POLL_MS 200

static void kmsg(const char *msg) {
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

/* Boot the stock stack as the REAL PID 1 and NEVER return. This is "boot
 * exactly like today, uncontained". It runs from a pristine namespace (oss-init
 * mounted nothing; the dead container's mounts were in its own namespace), so
 * first-stage mounts and SwitchRoot are clean. Tries the held fds first (they
 * survive Android's FreeRamdisk unlinking the initramfs), then the paths, then
 * -- only if nothing can be exec'd -- reboots, forever. */
static void boot_uncontained(int fd_magisk, int fd_stock, char **argv) {
    for (;;) {
        if (fd_magisk >= 0)
            syscall(SYS_execveat, fd_magisk, "", argv, environ, AT_EMPTY_PATH);
        (void)execve(PRIMARY_INIT, argv, environ);
        if (fd_stock >= 0)
            syscall(SYS_execveat, fd_stock, "", argv, environ, AT_EMPTY_PATH);
        (void)execve(STOCK_INIT, argv, environ);
        kmsg("<3>oss-init: no init to exec; rebooting to a fresh boot\n");
        sync();
        syscall(SYS_reboot, LINUX_REBOOT_MAGIC1, LINUX_REBOOT_MAGIC2,
                LINUX_REBOOT_CMD_RESTART, NULL);
        sleep_ms(1000); /* if reboot somehow returns, retry everything */
    }
}

/* Drain available /dev/kmsg records (opened O_NONBLOCK) and report whether the
 * liveness marker has appeared. Reads are across the whole kernel log; EPIPE
 * (ring overrun) just means some records were skipped -- keep going. */
static int saw_liveness(int kfd) {
    if (kfd < 0) return 0;
    char buf[1024];
    for (;;) {
        ssize_t n = read(kfd, buf, sizeof(buf) - 1);
        if (n > 0) {
            buf[n] = '\0';
            if (strstr(buf, LIVENESS_MARKER)) return 1;
            continue;
        }
        if (n < 0 && errno == EPIPE) continue; /* ring overran; resync */
        return 0;                              /* EAGAIN / EOF: nothing more now */
    }
}

/* Reap, watch for early liveness, and answer any container exit with the
 * uncontained fallback. NEVER returns. */
static void supervise(pid_t child, int fd_magisk, int fd_stock, int kfd, char **argv) {
    long deadline = now_ms() + LIVENESS_DEADLINE_MS;
    int alive_seen = 0;
    for (;;) {
        int status;
        pid_t r = waitpid(-1, &status, WNOHANG);
        if (r == child || (r < 0 && errno == ECHILD)) {
            /* The container is gone (reaped, or no children remain at all). */
            kmsg("<4>oss-init: container exited; booting uncontained\n");
            boot_uncontained(fd_magisk, fd_stock, argv); /* never returns */
        }
        if (r > 0) continue; /* reaped an orphan; keep going */

        if (!alive_seen && kfd >= 0) {
            if (saw_liveness(kfd)) {
                alive_seen = 1;
                kmsg("<6>oss-init: container reached second stage; supervising\n");
            } else if (now_ms() > deadline) {
                kmsg("<4>oss-init: no liveness by deadline; killing and booting uncontained\n");
                kill(child, SIGKILL);
                (void)waitpid(child, &status, 0);
                boot_uncontained(fd_magisk, fd_stock, argv); /* never returns */
            }
        }
        sleep_ms(POLL_MS);
    }
}

int main(int argc, char **argv) {
    (void)argc;
    kmsg("<6>oss-init: OSS-Phone init is the real PID 1; containing Android init\n");

    /* Held fds so the fallback survives Android's FreeRamdisk. */
    int fd_magisk = open(PRIMARY_INIT, O_RDONLY | O_CLOEXEC);
    int fd_stock = open(STOCK_INIT, O_RDONLY | O_CLOEXEC);
    /* Read side of the global kernel log, for liveness (no mount needed). */
    int kfd = open("/dev/kmsg", O_RDONLY | O_NONBLOCK | O_CLOEXEC);

    /* Create the container: Android init as PID 1 of a new PID + mount ns.
     * Raw clone with a NULL stack is fork-like (no CLONE_VM). On the stock
     * kernel (no PID_NS) this returns EPERM and we fall through to an
     * uncontained boot, so the image is still safe there. */
    long pid = syscall(SYS_clone,
                       (long)(CLONE_NEWPID | CLONE_NEWNS | SIGCHLD),
                       0L, 0L, 0L, 0L);

    if (pid == 0) {
        /* Container PID 1. Keep our mount changes from propagating to the host,
         * then run the exact stock stack; it does all the bring-up. Everything
         * it mounts stays in THIS namespace and is freed when it dies. */
        (void)mount(NULL, "/", NULL, MS_REC | MS_PRIVATE, NULL);
        (void)execve(PRIMARY_INIT, argv, environ);
        (void)execve(STOCK_INIT, argv, environ);
        _exit(127); /* could not exec Android; parent falls back cleanly */
    }

    if (pid > 0) {
        supervise((pid_t)pid, fd_magisk, fd_stock, kfd, argv); /* never returns */
    }

    /* clone failed (e.g. stock kernel without PID_NS): nothing was dirtied, so
     * boot the stock stack uncontained -- exactly today's boot. */
    kmsg("<4>oss-init: clone(NEWPID|NEWNS) failed; booting uncontained\n");
    boot_uncontained(fd_magisk, fd_stock, argv); /* never returns */
    return 0; /* unreachable; present only to satisfy the compiler */
}
