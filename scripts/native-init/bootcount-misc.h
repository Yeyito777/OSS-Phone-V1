/* Boot-attempt counter in the misc partition: record format v1.
 * Spec: docs/bootcount-misc-record.md (kernel agent); policy and timing:
 * notes/stage2/PROMOTION-PLAN.md §2. Shared by oss-init's PID-1 helper, its
 * `--bootcount` CLI and the test bench; this is the ONLY writer.
 *
 * Layout: two 4 KiB A/B slots at 0x40000 and 0x41000 of the 1 MiB misc
 * partition. A slot holds a 32-byte LE record (magic OSSBOOTC, ver 1, size 32,
 * seq, count, max, flags, crc32 over 0x00-0x1b) followed by zeros.
 *
 * Write rules (hard): only full 4 KiB blocks at exactly 0x40000 or 0x41000;
 * never the BCB (first 4 KiB) or anything else; never a block that is neither
 * zero nor ours; O_DSYNC + fsync, then read back and compare.
 *
 * Slot classes:
 *   ZERO     all 4096 bytes zero
 *   VALID    our record (magic, version, size, crc) and the rest of the block zero
 *   DAMAGED  our magic, but version/size/crc wrong, rest zero: our own torn or
 *            corrupted write. Not valid, but OURS: it may be overwritten. (A
 *            literal "neither valid nor zero => foreign" rule would turn one
 *            torn write into a permanently disabled store.)
 *   FOREIGN  anything else: someone else uses the block. Write nothing. */
#pragma once
#define _GNU_SOURCE
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#define BC_SLOT_A    0x40000L
#define BC_SLOT_B    0x41000L
#define BC_BLOCK     4096
#define BC_MISC_SIZE 1048576LL
#define BC_MAX       3
#define BC_CAP       0xffffu
#define BC_MAGIC     "OSSBOOTC"

enum bc_slot { BC_ZERO, BC_VALID, BC_DAMAGED, BC_FOREIGN };
enum bc_status {
    BC_OK = 0,      /* a valid record was read (and written, for a write op) */
    BC_NONE,        /* no valid record (absent or only damaged): count 0     */
    BC_FOREIGN_BLK, /* a slot is foreign: nothing written                    */
    BC_IO,          /* read/write/fsync failed                               */
    BC_VERIFY,      /* read-back differs from what was written               */
    BC_NOSTORE,     /* misc not found, ambiguous, or the wrong size          */
};
static const char *bc_status_name(int s) {
    static const char *n[] = {"ok", "none", "foreign", "io", "verify", "nostore"};
    return (s >= 0 && s <= BC_NOSTORE) ? n[s] : "?";
}

struct bc_state {
    int status;
    int cur;            /* slot holding the current record: 0=A, 1=B, -1 none */
    uint32_t seq, count;
    int cls[2];
};

static uint32_t bc_crc32(const uint8_t *p, size_t n) {   /* IEEE, = zlib crc32(0,..) */
    uint32_t c = 0xffffffffu;
    while (n--) {
        c ^= *p++;
        for (int k = 0; k < 8; k++) c = (c >> 1) ^ (0xedb88320u & (0u - (c & 1u)));
    }
    return ~c;
}
static uint32_t bc_le32(const uint8_t *p) { return p[0] | p[1] << 8 | p[2] << 16 | (uint32_t)p[3] << 24; }
static uint16_t bc_le16(const uint8_t *p) { return (uint16_t)(p[0] | p[1] << 8); }
static void bc_put32(uint8_t *p, uint32_t v) { p[0] = v; p[1] = v >> 8; p[2] = v >> 16; p[3] = v >> 24; }
static void bc_put16(uint8_t *p, uint16_t v) { p[0] = v; p[1] = v >> 8; }

static int bc_all_zero(const uint8_t *p, size_t n) { while (n--) if (*p++) return 0; return 1; }

static int bc_classify(const uint8_t *b) {
    if (bc_all_zero(b, BC_BLOCK)) return BC_ZERO;
    if (memcmp(b, BC_MAGIC, 8) != 0) return BC_FOREIGN;
    if (!bc_all_zero(b + 32, BC_BLOCK - 32)) return BC_FOREIGN;  /* not just our record */
    if (bc_le16(b + 8) != 1 || bc_le16(b + 10) != 32) return BC_DAMAGED;
    if (bc_crc32(b, 0x1c) != bc_le32(b + 0x1c)) return BC_DAMAGED;
    return BC_VALID;
}

static void bc_build(uint8_t *b, uint32_t seq, uint32_t count) {
    memset(b, 0, BC_BLOCK);
    memcpy(b, BC_MAGIC, 8);
    bc_put16(b + 8, 1); bc_put16(b + 10, 32);
    bc_put32(b + 0x0c, seq); bc_put32(b + 0x10, count);
    bc_put32(b + 0x14, BC_MAX); bc_put32(b + 0x18, 0);
    bc_put32(b + 0x1c, bc_crc32(b, 0x1c));
}

/* Aligned for O_DIRECT. */
static uint8_t bc_buf[2 * BC_BLOCK] __attribute__((aligned(4096)));
static uint8_t bc_wbuf[BC_BLOCK] __attribute__((aligned(4096)));

static ssize_t bc_pread_full(int fd, void *b, size_t n, off_t off) {
    size_t got = 0;
    while (got < n) {
        ssize_t r = pread(fd, (char *)b + got, n - got, off + (off_t)got);
        if (r < 0) { if (errno == EINTR) continue; return -1; }
        if (r == 0) return -1;
        got += (size_t)r;
    }
    return (ssize_t)got;
}

static struct bc_state bc_read(int fd) {
    struct bc_state st = {.status = BC_NONE, .cur = -1};
    if (bc_pread_full(fd, bc_buf, 2 * BC_BLOCK, BC_SLOT_A) < 0) { st.status = BC_IO; return st; }
    uint32_t seq[2] = {0, 0}, cnt[2] = {0, 0};
    for (int i = 0; i < 2; i++) {
        const uint8_t *b = bc_buf + i * BC_BLOCK;
        st.cls[i] = bc_classify(b);
        if (st.cls[i] == BC_VALID) { seq[i] = bc_le32(b + 0x0c); cnt[i] = bc_le32(b + 0x10); }
    }
    if (st.cls[0] == BC_FOREIGN || st.cls[1] == BC_FOREIGN) { st.status = BC_FOREIGN_BLK; return st; }
    int a = st.cls[0] == BC_VALID, b = st.cls[1] == BC_VALID;
    if (a && b) st.cur = ((int32_t)(seq[0] - seq[1]) > 0) ? 0 : 1;
    else if (a) st.cur = 0;
    else if (b) st.cur = 1;
    if (st.cur >= 0) { st.status = BC_OK; st.seq = seq[st.cur]; st.count = cnt[st.cur]; }
    return st;
}

/* The only function that writes misc. Exactly one 4 KiB block, at exactly
 * 0x40000 or 0x41000, and only over a block that is zero or ours. */
static int bc_write_slot(int fd, int slot, int cls, uint32_t seq, uint32_t count, int direct) {
    off_t off = slot == 0 ? BC_SLOT_A : slot == 1 ? BC_SLOT_B : -1;
    if (off != BC_SLOT_A && off != BC_SLOT_B) return BC_IO;      /* never anything else */
    if (off < BC_BLOCK) return BC_IO;                            /* never the BCB */
    if (cls == BC_FOREIGN) return BC_FOREIGN_BLK;                /* never someone else's block */
    bc_build(bc_wbuf, seq, count);
    size_t done = 0;
    while (done < BC_BLOCK) {
        ssize_t w = pwrite(fd, bc_wbuf + done, BC_BLOCK - done, off + (off_t)done);
        if (w < 0) { if (errno == EINTR) continue; return BC_IO; }
        if (w == 0) return BC_IO;
        done += (size_t)w;
    }
    if (fsync(fd) < 0) return BC_IO;
    if (!direct) (void)posix_fadvise(fd, off, BC_BLOCK, POSIX_FADV_DONTNEED);
    if (bc_pread_full(fd, bc_buf, BC_BLOCK, off) < 0) return BC_IO;
    return memcmp(bc_buf, bc_wbuf, BC_BLOCK) ? BC_VERIFY : BC_OK;
}

enum bc_op { BC_SHOW, BC_BUMP, BC_CLEAR, BC_INIT };

/* Read, then (for a write op) write the next record to the other slot.
 * out: the resulting state (count after the op). allow_create: whether a write
 * may create the first record when none is valid (bump/init yes; promoted-mode
 * bump no, that escalates instead). */
static struct bc_state bc_op(int fd, int op, int direct, int allow_create, int *wrote) {
    struct bc_state st = bc_read(fd);
    *wrote = 0;
    if (op == BC_SHOW || st.status == BC_FOREIGN_BLK || st.status == BC_IO) return st;
    if (st.status == BC_NONE && !allow_create && op == BC_BUMP) return st;
    uint32_t cnt = st.status == BC_OK ? st.count : 0, nc;
    switch (op) {
    case BC_BUMP:  nc = cnt >= BC_CAP ? BC_CAP : cnt + 1; break;
    case BC_CLEAR: if (cnt == 0) return st; nc = 0; break;           /* idempotent: no wear */
    case BC_INIT:  if (st.status == BC_OK && cnt == 0) return st; nc = 0; break;
    default: return st;
    }
    int target = st.cur == 0 ? 1 : 0;                               /* none valid => A */
    int r = bc_write_slot(fd, target, st.cls[target], st.seq + 1, nc, direct);
    if (r != BC_OK) { st.status = r; return st; }
    *wrote = 1;
    st.status = BC_OK; st.cur = target; st.seq = st.seq + 1; st.count = nc;
    return st;
}

/* Open a misc node for the counter; check it is exactly 1 MiB. *direct says
 * whether O_DIRECT stuck (a block device: yes; a test file on tmpfs: no). */
static int bc_open(const char *node, int *direct) {
    *direct = 1;
    int fd = open(node, O_RDWR | O_DSYNC | O_DIRECT | O_CLOEXEC);
    if (fd < 0 && errno == EINVAL) { *direct = 0; fd = open(node, O_RDWR | O_DSYNC | O_CLOEXEC); }
    if (fd < 0) return -1;
    off_t sz = lseek(fd, 0, SEEK_END);
    if (sz != (off_t)BC_MISC_SIZE) { close(fd); errno = EFBIG; return -2; }
    return fd;
}

/* Find the partition named "misc" under SYSROOT/class/block: exactly one match,
 * size exactly 2048 512-byte sectors. Fills dev numbers and DEVNAME. */
static int bc_find_misc(const char *sysroot, unsigned *maj, unsigned *min, char *devname, size_t dn) {
    char path[512], buf[1024];
    snprintf(path, sizeof path, "%s/class/block", sysroot);
    DIR *d = opendir(path);
    if (!d) return -1;
    int found = 0; struct dirent *e;
    while ((e = readdir(d))) {
        if (e->d_name[0] == '.') continue;
        snprintf(path, sizeof path, "%s/class/block/%s/uevent", sysroot, e->d_name);
        int fd = open(path, O_RDONLY | O_CLOEXEC);
        if (fd < 0) continue;
        ssize_t n = read(fd, buf, sizeof buf - 1); close(fd);
        if (n <= 0) continue;
        buf[n] = 0;
        int is_misc = 0; unsigned M = 0, m = 0; int hm = 0, hn = 0; char dname[64] = "";
        for (char *l = strtok(buf, "\n"); l; l = strtok(NULL, "\n")) {
            if (!strcmp(l, "PARTNAME=misc")) is_misc = 1;
            else if (!strncmp(l, "MAJOR=", 6)) { M = (unsigned)strtoul(l + 6, 0, 10); hm = 1; }
            else if (!strncmp(l, "MINOR=", 6)) { m = (unsigned)strtoul(l + 6, 0, 10); hn = 1; }
            else if (!strncmp(l, "DEVNAME=", 8)) snprintf(dname, sizeof dname, "%s", l + 8);
        }
        if (!is_misc || !hm || !hn) continue;
        snprintf(path, sizeof path, "%s/class/block/%s/size", sysroot, e->d_name);
        fd = open(path, O_RDONLY | O_CLOEXEC);
        if (fd < 0) continue;
        n = read(fd, buf, sizeof buf - 1); close(fd);
        if (n <= 0) continue;
        buf[n] = 0;
        if (strtoull(buf, 0, 10) * 512ULL != (unsigned long long)BC_MISC_SIZE) { closedir(d); return -3; }
        if (++found > 1) { closedir(d); return -2; }               /* ambiguous */
        *maj = M; *min = m; snprintf(devname, dn, "%s", dname);
    }
    closedir(d);
    return found == 1 ? 0 : -1;
}
