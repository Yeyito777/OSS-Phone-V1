/* OSS-Phone-V1 recovery-slot pre-init ("oss-pid1").
 *
 * /init of the plain recovery-slot image (recovery-slot-moon): today's normal
 * boot -- magiskinit -> stock init -> Moon -- started once from the RECOVERY
 * partition. It does two things and then gets out of the way:
 *
 *   1. FIRST, load oss-rebootmode-clear.ko (rebootmode-clear.h): the slot was
 *      entered with reboot(2) "recovery", which leaves a recovery selection in
 *      PMU registers that survive a panic or a hard reset. Clearing it makes
 *      any later failure of this boot return to BOOT instead of looping here.
 *   2. Put magiskinit back at /init and exec it. magiskinit only works as /init
 *      (it backs up and hijacks whatever /init is; notes/stage2/V3-BOOTLOOP-
 *      ROOTCAUSE.md), so it is renamed over this binary's own path first. The
 *      running program is unaffected; its inode stays alive until the exec.
 *
 * If the clear fails, the PMU may still select recovery, so nothing here may
 * panic or reboot: exec the plain stock init (/init.stock, static) instead, and
 * if even that is impossible, halt (the Download-mode escape recovers it).
 * No path returns from PID 1.
 */
#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <unistd.h>

#include "rebootmode-clear.h"

extern char **environ;

#ifndef MAGISK_INIT
#define MAGISK_INIT "/init.magisk"
#endif
#ifndef STOCK_INIT
#define STOCK_INIT "/init.stock"
#endif

static void kmsg(const char *msg) {
#ifdef TEST_STDERR
    (void)!write(2, msg, strlen(msg));
#endif
    int fd = open("/dev/kmsg", O_WRONLY | O_CLOEXEC);
    if (fd < 0) return;
    (void)!write(fd, msg, strlen(msg));
    (void)close(fd);
}

static void exec_stock_or_halt(char **argv) {
    for (int attempt = 0; attempt < 3; attempt++) {
        execve(STOCK_INIT, argv, environ);
        usleep(200000);
    }
    kmsg("<3>oss-pid1: cannot exec the stock init and must not reboot; halting\n");
    for (;;) pause();
}

int main(int argc, char **argv) {
    (void)argc;
    int clear_err = load_rebootmode_clear();          /* before anything that can fail */
    (void)mknod("/dev/kmsg", S_IFCHR | 0600, makedev(1, 11));

    if (clear_err) {
        char line[160];
        kmsg(errno_line(line, sizeof(line),
             "<3>oss-pid1: oss-rebootmode-clear did not load; exec stock init, no reboot fallback; errno=", clear_err));
        exec_stock_or_halt(argv);
    }
    kmsg("<6>oss-pid1: PMU reboot state set to NORMAL; handing PID 1 to magiskinit as /init\n");

    if (rename(MAGISK_INIT, "/init") == 0) {
        execve("/init", argv, environ);
        kmsg("<3>oss-pid1: exec of magiskinit failed; exec stock init\n");
    } else {
        kmsg("<3>oss-pid1: cannot put magiskinit at /init; exec stock init\n");
    }
    /* The PMU now selects BOOT, so a later failure of the stock boot would at
     * worst return to BOOT; still, never panic deliberately. */
    exec_stock_or_halt(argv);
}
