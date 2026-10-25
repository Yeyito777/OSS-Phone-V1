/* OSS-Phone-V1 stage-2 pre-init ("oss-pid1").
 *
 * This is OSS Phone's own first userspace process: the kernel execs it as
 * PID 1 in place of the ramdisk's stock/Magisk init. Its single job today is
 * to OWN the first userspace moment and then chain-load the proven stage-1
 * boot, so that from this release onward PID 1 belongs to OSS Phone rather
 * than to a third party, while every piece of the working hardware bring-up
 * stays exactly as it is.
 *
 * It does NOT try to bring the hardware up itself (that path -- a native
 * PID-1 supervisor loading vendor modules -- was tried in the research
 * workspace as v5k..v5n and always died: the s3c2410 hardware watchdog node
 * was not ready in time, module ordering was wrong, and USB never came up).
 * Instead it execs the exact proven init, which becomes PID 1 (execve keeps
 * the PID) and performs Samsung's full, ordered bring-up untouched:
 * ueventd/coldboot, first-stage mounts, SELinux policy load, the property
 * service, 306 kernel modules, firmware, adbd/USB, charging, and watchdogd
 * petting /dev/watchdog0.
 *
 * Safety is the whole point of the design:
 *   1. Primary handoff is /init.magisk (the renamed magiskinit). That
 *      reproduces today's boot bit for bit: Magisk, the overlay.d Moon
 *      direct-boot rc, and Moon on screen in ~34 s.
 *   2. If that exec ever fails, we fall back to /init.stock, the byte-exact
 *      stock Android init, so the phone still becomes a normal (unrooted)
 *      Android boot instead of a brick.
 *   3. If even that fails (both files absent -- impossible in a correctly
 *      built ramdisk), we do NOT return from PID 1 (that would panic the
 *      kernel). We spin; the bootloader-armed watchdog then resets the
 *      device into a fresh boot.
 *
 * Keep this program tiny, static, no-heap, and syscall-minimal: a crash here
 * is a PID-1 crash, i.e. a kernel panic. Everything it does before the exec
 * must be strictly a subset of what the init we hand off to already tolerates
 * (so: nothing but a single best-effort /dev/kmsg marker).
 */

#define _GNU_SOURCE

#include <errno.h>
#include <fcntl.h>
#include <string.h>
#include <unistd.h>

extern char **environ;

/* Chain targets relative to the ramdisk root, in order of preference. */
#define PRIMARY_INIT "/init.magisk" /* magiskinit -> stock init -> Moon */
#define STOCK_INIT "/init.stock"    /* byte-exact stock Android init     */

/* Best-effort kernel-log marker. /dev/kmsg exists in the Samsung boot ramdisk
 * before any userspace has run (proven by the v6c experiment, whose pre-init
 * wrapper logged here). A failure to open or write is ignored on purpose:
 * logging must never be able to stop the handoff. */
static void kmsg(const char *msg) {
    int fd = open("/dev/kmsg", O_WRONLY | O_CLOEXEC);
    if (fd < 0) return;
    (void)!write(fd, msg, strlen(msg));
    (void)close(fd);
}

int main(int argc, char **argv) {
    (void)argc;

    kmsg("<6>oss-pid1: OSS-Phone pre-init running as PID 1; chaining to stage-1 boot\n");

    /* Primary: the full, proven stage-1 stack (Magisk + Moon direct-boot).
     * argv is passed through unchanged so magiskinit sees exactly what the
     * kernel would have given it (argv[0] == "/init", no "selinux_setup"),
     * driving its normal first-stage path. */
    execve(PRIMARY_INIT, argv, environ);
    kmsg("<4>oss-pid1: primary init exec failed; falling back to stock Android init\n");

    /* Fallback: plain stock Android init -- a normal unrooted boot, no Moon. */
    execve(STOCK_INIT, argv, environ);
    kmsg("<3>oss-pid1: FATAL both init handoffs failed; waiting for watchdog reset\n");

    /* Do not return: exiting PID 1 panics the kernel. Spin quietly and let the
     * bootloader-armed s3c2410 watchdog reset us into a clean boot. */
    for (;;) pause();
}
