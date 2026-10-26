/* OSS-Phone panic canary: /init of a recovery image (stock recovery kernel,
 * dtb, dtbo and ramdisk; only /init replaced and oss-rebootmode-clear.ko added).
 *
 * Purpose: prove that a recovery-slot boot that dies as PID 1 (the stage-2
 * v2/v3 failure) comes back to BOOT on its own, once the PMU recovery selection
 * has been cleared. Order:
 *   1. FIRST, load oss-rebootmode-clear.ko with require_mode=1
 *      (rebootmode-clear.h): the load succeeds only if it found a recovery-style
 *      mode in reboot-cmd (0x810, mode bits 2:0) and cleared it, with the read-
 *      back verified. Its before:/after: kernel-log lines are the measurement of
 *      what a recovery boot really holds (CANARY-RUNBOOK.md table).
 *   2. If it loaded: log OSS-CANARY, then exit PID 1 -> "Attempted to kill
 *      init" -> panic -> reset. Expected: the phone boots BOOT, hands-free.
 *   3. If it did NOT load (no mode found, refused, or an older module without
 *      require_mode): the PMU may still select recovery, so a panic could
 *      loop. Do not panic: exec the stock recovery init (/system/bin/init, what
 *      the stock ramdisk's /init symlink points at), so stock recovery comes up
 *      and the user leaves it with "Reboot system now" (a clean reboot).
 * It makes no mounts and touches no partition.
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <unistd.h>

#include "../rebootmode-clear.h"

extern char **environ;

#ifndef RECOVERY_INIT
#define RECOVERY_INIT "/system/bin/init"
#endif

static void kmsg(const char *msg) {
#ifdef TEST_STDERR
    (void)!write(2, msg, strlen(msg));
#endif
    int fd = open("/dev/kmsg", O_WRONLY | O_CLOEXEC);
    if (fd < 0) return;
    (void)!write(fd, msg, strlen(msg));
    close(fd);
}

int main(int argc, char **argv) {
    (void)argc;
    /* require_mode=1: succeed only if this load found and cleared a recovery-
     * style mode in reboot-cmd (0x810). Only then is the deliberate panic
     * backed by evidence that the real selector was cleared. */
    int clear_err = load_rebootmode_clear_params("require_mode=1", 0);   /* first */
    mkdir("/dev", 0755);
    mknod("/dev/kmsg", S_IFCHR | 0600, makedev(1, 11));

    if (clear_err) {
        char line[200];
        kmsg(errno_line(line, sizeof(line),
             "<3>OSS-CANARY: oss-rebootmode-clear did not load; NOT panicking, booting stock recovery; errno=", clear_err));
        char *av[] = {"/init", NULL};
        for (int attempt = 0; attempt < 3; attempt++) {
            execve(RECOVERY_INIT, argv && argv[0] ? argv : av, environ);
            usleep(200000);
        }
        kmsg("<3>OSS-CANARY: cannot exec stock recovery init; halting (no panic)\n");
        for (;;) pause();
    }

    kmsg("<3>OSS-CANARY: PMU reboot state cleared; exiting PID 1 now to force a panic/reset (expected: return to BOOT)\n");
    sync();   /* let the kmsg lines reach pstore before PID 1 dies */
    _exit(0);
}
