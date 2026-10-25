/* OSS-Phone-V1 stage-2 init ("oss-init") -- the real containment step.
 *
 * On the OSS Phone kernel (CONFIG_PID_NS=y, CONFIG_DEVTMPFS=y, DEFEX removed)
 * this binary is the ramdisk /init, so the kernel execs it as the REAL PID 1
 * and it STAYS PID 1 for the life of the system. Android's entire init (first
 * and second stage) is run as its child inside a new PID + mount namespace, so
 * Android init sees getpid()==1 in its own namespace and performs Samsung's
 * full, proven hardware bring-up unchanged -- 306 modules, ueventd/coldboot,
 * first-stage mounts, SELinux policy load, the property service, firmware,
 * adbd/USB, charging, watchdogd petting /dev/watchdog0 -- while OSS Phone owns
 * the real init seat. This is the Halium-style "our init PID 1, Android
 * contained" architecture, reduced to exactly what is safe to run today: the
 * contained Android still drives all the hardware and still brings Moon up via
 * the existing direct-boot overlay; what changed is who is PID 1.
 *
 * THE IRON RULE: any failure in our path must leave the phone booting exactly
 * as it does today. This is enforced at every layer:
 *   - Setup failure (clone/mount) -> we exec the stock stack as the real PID 1
 *     (identical to today's boot, no containment).
 *   - The container dies without an intentional reboot (a crash, or an early
 *     failure before it ran anything) -> we exec the stock stack as the real
 *     PID 1 (a fresh, uncontained boot; no reboot loop).
 *   - The container hangs before it signals liveness -> we kill it and exec the
 *     stock stack as the real PID 1.
 *   - The container asks for a real reboot/shutdown -> we perform it for real
 *     (PID-namespace reboot(2) only tears the namespace down, so PID 1 must).
 *
 * Fallback execs use file descriptors opened at startup and run with
 * execveat(AT_EMPTY_PATH): Android's first-stage FreeRamdisk() unlinks the
 * shared initramfs files after switch_root, so a path-based exec could fail,
 * but an open fd survives the unlink.
 *
 * Keep this tiny, static and syscall-minimal: a crash here is a PID-1 crash.
 */

#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>
#include <linux/reboot.h>

extern char **environ;

#define PRIMARY_INIT "/init.magisk" /* magiskinit -> stock init -> Moon        */
#define STOCK_INIT "/init.stock"    /* byte-exact stock Android init (fallback)*/
#define RELAY_DIR "/oss-run"        /* tmpfs shared with the container          */
#define RELAY_ALIVE RELAY_DIR "/alive"
#define RELAY_POWERCTL RELAY_DIR "/powerctl"
#define LIVENESS_DEADLINE_MS 90000  /* kill+fallback if no liveness by here     */
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

/* exec a held init fd as the real PID 1: this is "boot exactly like today". */
static void exec_fd(int fd, char **argv) {
    if (fd >= 0) {
        syscall(SYS_execveat, fd, "", argv, environ, AT_EMPTY_PATH);
    }
}

/* Last-resort: exec the stock stack as the real PID 1 and never return. */
static void fallback_boot(int fd_magisk, int fd_stock, char **argv) {
    exec_fd(fd_magisk, argv);
    (void)execve(PRIMARY_INIT, argv, environ); /* in case the fd path failed */
    exec_fd(fd_stock, argv);
    (void)execve(STOCK_INIT, argv, environ);
    kmsg("<3>oss-init: FATAL no init to hand off to; waiting for watchdog reset\n");
    for (;;) pause();
}

/* Perform the real reboot/shutdown the contained Android asked for. The vendor
 * restart handler (exynos/sec reboot) is registered globally by modules the
 * container loaded, so reboot(2) from the initial namespace reaches it. */
static void do_real_powerctl(const char *req) {
    sync();
    if (strncmp(req, "shutdown", 8) == 0) {
        reboot(LINUX_REBOOT_CMD_POWER_OFF);
    } else {
        /* Android's sys.powerctl is "reboot[,reason]"; the reason is the
         * reboot(2) arg ("bootloader", "download", "recovery", ...). */
        const char *comma = strchr(req, ',');
        const char *reason = comma ? comma + 1 : "";
        syscall(SYS_reboot, LINUX_REBOOT_MAGIC1, LINUX_REBOOT_MAGIC2,
                LINUX_REBOOT_CMD_RESTART2, reason);
    }
    kmsg("<3>oss-init: real reboot syscall returned; waiting for watchdog\n");
    for (;;) pause();
}

/* Read a small relay file; returns length (0 if absent/empty/error). */
static int read_relay(const char *path, char *buf, int cap) {
    int fd = open(path, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return 0;
    int n = (int)read(fd, buf, cap - 1);
    (void)close(fd);
    if (n < 0) n = 0;
    buf[n] = '\0';
    /* trim trailing whitespace/newline */
    while (n > 0 && (buf[n - 1] == '\n' || buf[n - 1] == ' ' || buf[n - 1] == '\t')) {
        buf[--n] = '\0';
    }
    return n;
}

int main(int argc, char **argv) {
    (void)argc;
    kmsg("<6>oss-init: OSS-Phone init is the real PID 1; containing Android init\n");

    /* Hold fds to both fallback inits so the iron rule survives FreeRamdisk. */
    int fd_magisk = open(PRIMARY_INIT, O_RDONLY | O_CLOEXEC);
    int fd_stock = open(STOCK_INIT, O_RDONLY | O_CLOEXEC);

    /* Relay tmpfs shared (same superblock) with the container, for liveness and
     * the reboot target. Best-effort: if it fails we degrade safely (no hang
     * breaker, reboots become a fresh uncontained boot instead). */
    int relay_ok = 0;
    (void)mkdir(RELAY_DIR, 0755);
    if (mount("tmpfs", RELAY_DIR, "tmpfs", 0, "mode=0755") == 0) {
        relay_ok = 1;
    } else {
        kmsg("<4>oss-init: relay tmpfs mount failed; reboot relay/hang-breaker disabled\n");
    }

    /* Create the container: Android init as PID 1 of a new PID + mount ns.
     * Raw clone with a NULL stack behaves like fork (no CLONE_VM) but places
     * the child in the new namespaces; parent's namespaces are unchanged. */
    long pid = syscall(SYS_clone,
                       (long)(CLONE_NEWPID | CLONE_NEWNS | SIGCHLD),
                       0L, 0L, 0L, 0L);
    if (pid < 0) {
        kmsg("<4>oss-init: clone(NEWPID|NEWNS) failed; booting uncontained (as today)\n");
        fallback_boot(fd_magisk, fd_stock, argv); /* iron rule */
    }

    if (pid == 0) {
        /* Container PID 1. Keep our mount changes from propagating back to the
         * host, then run the exact stock stack; it does all the bring-up. */
        (void)mount(NULL, "/", NULL, MS_REC | MS_PRIVATE, NULL);
        (void)execve(PRIMARY_INIT, argv, environ);
        (void)execve(STOCK_INIT, argv, environ);
        /* Could not exec Android at all: die so the parent falls back cleanly. */
        _exit(127);
    }

    /* --- Real PID 1 (host supervisor) ---
     * Reap, hold the namespace, break an early hang, proxy reboots, and honour
     * the iron rule on any unexpected container death. */
    long deadline = now_ms() + LIVENESS_DEADLINE_MS;
    int alive_seen = 0;
    for (;;) {
        int status;
        pid_t r = waitpid(-1, &status, WNOHANG);
        if (r == (pid_t)pid) {
            /* The container exited. Intentional reboot => powerctl was written;
             * anything else (crash / early bail) => fall back to today's boot. */
            char req[64];
            if (relay_ok && read_relay(RELAY_POWERCTL, req, sizeof(req)) > 0) {
                kmsg("<6>oss-init: container requested powerctl; performing real reboot\n");
                do_real_powerctl(req);
            }
            kmsg("<4>oss-init: container exited without a reboot request; booting uncontained\n");
            fallback_boot(fd_magisk, fd_stock, argv); /* iron rule */
        }
        if (r > 0) continue; /* reaped an orphan zombie; keep going */

        /* r == 0: nobody exited. Watch for early liveness / hang. */
        if (relay_ok && !alive_seen) {
            if (access(RELAY_ALIVE, F_OK) == 0) {
                alive_seen = 1;
                kmsg("<6>oss-init: container reported liveness; supervising\n");
            } else if (now_ms() > deadline) {
                kmsg("<4>oss-init: container never signalled liveness; killing and booting uncontained\n");
                kill((pid_t)pid, SIGKILL);
                (void)waitpid((pid_t)pid, &status, 0);
                fallback_boot(fd_magisk, fd_stock, argv); /* iron rule */
            }
        }
        sleep_ms(POLL_MS);
    }
}
