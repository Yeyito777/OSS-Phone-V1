/* OSS-Phone panic canary: /init of a recovery image (stock recovery kernel,
 * dtb, dtbo and ramdisk; only /init replaced and oss-rebootmode-clear.ko added).
 *
 * Purpose: prove that a recovery-slot boot that dies as PID 1 (the stage-2
 * v2/v3 failure) comes back to BOOT on its own, once the PMU recovery selection
 * has been cleared. Order:
 *   1. FIRST, load oss-rebootmode-clear.ko (rebootmode-clear.h). Its kernel-log
 *      lines on a recovery entry should read
 *        before: reboot-cmd=0x1 ... panic-inform=0x12345674
 *        after:  reboot-cmd=0x0 ... magic bits 23:16=0x4e, panic-inform=0x12345670
 *   2. If it loaded: log OSS-CANARY, then exit PID 1 -> "Attempted to kill
 *      init" -> panic -> reset. Expected: the phone boots BOOT, hands-free.
 *   3. If it did NOT load: the PMU may still select recovery, so a panic would
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
    int clear_err = load_rebootmode_clear();         /* before anything that can fail */
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
