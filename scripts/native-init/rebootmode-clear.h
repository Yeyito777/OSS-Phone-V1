/* The first action of every recovery-slot /init (oss-init, oss-pid1, canary).
 *
 * A recovery-slot test image is entered with reboot(2) "recovery", which leaves
 * a recovery selection in the PMU alive-domain registers (exynos-reboot 0x810,
 * sec_reboot magic-inform 0x808 / panic-inform 0x80c). A hard reset or a kernel
 * panic does not clear them, so without this a failed test boot would loop in
 * the recovery slot (notes/stage2/RECOVERY-PERSISTENCE-ROOTCAUSE.md). The
 * kernel agent's oss-rebootmode-clear.ko (src/oss-rebootmode-clear, branch
 * rebootmode-clear) rewrites them to a NORMAL boot through the built-in syscon
 * regmap, so it works before any vendor module is loaded, and logs its
 * before:/after: values to the kernel log. Loading it must come before anything
 * that can fail.
 *
 * Returns 0 when the registers now select BOOT (loaded now, or EEXIST: already
 * loaded), otherwise an errno. Callers must not panic or reboot on failure:
 * with the selection still on recovery that would loop.
 */
#ifndef OSS_REBOOTMODE_CLEAR_H
#define OSS_REBOOTMODE_CLEAR_H

#include <errno.h>
#include <fcntl.h>
#include <sys/syscall.h>
#include <unistd.h>

#ifndef CLEAR_KO
#define CLEAR_KO "/oss-rebootmode-clear.ko"
#endif

/* params: "" for oss-init and oss-pid1 (succeed whenever the registers now
 * select NORMAL); "require_mode=1" for the canary, which must only panic when
 * this load actually found and cleared a recovery-style mode in reboot-cmd.
 * A module that lacks a parameter refuses the load, so an older .ko fails safe.
 * eexist_ok: whether "already loaded" counts as success. The canary passes 0:
 * only a load that just did the clear is evidence enough to panic on. */
static int load_rebootmode_clear_params(const char *params, int eexist_ok) {
#ifdef TEST_FAKE_CLEAR
    /* Test bench only: a user namespace can never load a module. */
    (void)params;
    return TEST_FAKE_CLEAR;
#else
    int fd = open(CLEAR_KO, O_RDONLY | O_CLOEXEC);
    if (fd < 0) return errno ? errno : ENOENT;
    long r = syscall(SYS_finit_module, fd, params, 0);
    int e = errno;
    close(fd);
    if (r == 0 || (eexist_ok && e == EEXIST)) return 0;
    return e ? e : EIO;
#endif
}

__attribute__((unused))
static int load_rebootmode_clear(void) { return load_rebootmode_clear_params("", 1); }

/* "<prefix> <errno>\n" into buf for a kmsg line, no stdio needed. */
static const char *errno_line(char *buf, size_t cap, const char *prefix, int e) {
    size_t n = 0;
    while (prefix[n] && n + 16 < cap) { buf[n] = prefix[n]; n++; }
    char d[12]; int k = 0;
    unsigned v = (unsigned)e;
    do { d[k++] = (char)('0' + v % 10); v /= 10; } while (v && k < 11);
    while (k && n + 2 < cap) buf[n++] = d[--k];
    buf[n++] = '\n'; buf[n] = 0;
    return buf;
}

#endif
