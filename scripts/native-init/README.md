# Stage 2: `oss-pid1` — OSS Phone's own PID-1 pre-init

OSS Phone's own binary is the ramdisk `/init`, so the kernel execs **it** as
PID 1. It owns the first userspace instant, then chain-loads the proven
stage-1 boot and falls back to stock Android if anything fails. On the happy
path the boot is bit-for-bit today's Moon direct boot; PID 1 just belongs to
OSS Phone now.

See `notes/stage2/DESIGN.md` for the research (why full Linux-first containment
needs a custom kernel) and `notes/stage2/TEST-PLAN.md` for the attended morning
test. **Do not flash unattended** — there is no A/B slot.

## Pieces

| Where | File | Role |
| --- | --- | --- |
| source | `oss-pid1.c` | the PID-1 pre-init: kmsg marker, then exec chain |
| builder | `build-boot-image.sh` | adds oss-pid1 to the current Moon direct-boot image |
| boot ramdisk | `/init` | oss-pid1 (new PID 1) |
| boot ramdisk | `/init.magisk` | magiskinit (primary handoff; today's stack) |
| boot ramdisk | `/init.stock` | stock Android init (fallback handoff) |

## Boot flow

1. Kernel execs ramdisk `/init` = **oss-pid1 as real PID 1**.
2. oss-pid1 writes one `/dev/kmsg` marker (proof of PID-1 ownership).
3. `execve("/init.magisk", argv, environ)` → magiskinit → stock init → 306
   modules, ueventd, SELinux, property service, firmware, adbd/USB, charging,
   watchdogd → overlay.d stops zygote → Moon ≈34 s. Identical to today.
4. If the primary exec fails → `execve("/init.stock")` = normal stock Android.
5. If both fail (impossible in a valid ramdisk) → spin; the bootloader-armed
   watchdog resets into a clean boot. oss-pid1 never returns from PID 1.

The watchdog keepalive, module order, firmware, USB/adb and charging are all
unchanged because stock init still does every bit of the bring-up; oss-pid1
adds <1 ms before an identical handoff.

## Build (research host, e.g. whale)

```sh
# Needs: aarch64 cross gcc, magiskboot, avbtool, xz, the project AVB key, and
# the current Moon direct-boot image + Magisk stock-init backup (research ws).
scripts/native-init/build-boot-image.sh OUT_DIR
```

Base image is `boot-moon-direct-A266MUBU1AYB8.img` (sha256 `3bf29162…e9f6`).
The script verifies the kernel is byte-identical, `/init` is the compiled
oss-pid1, `/init.magisk` is the base's former `/init` byte-for-byte, the
overlay.d Moon files are unchanged, and `/init.stock` is the decompressed
Magisk backup. Output is a 64 MiB AVB-hashed BOOT image plus `SHA256SUMS`,
`boot-avb-info.txt`, and `README.txt`. The built image is git-ignored
(`/images/`).

## Rollback

A BOOT flash leaves data, Magisk and `/data/adb` untouched. To return to the
shipping stage-1 boot, flash the known-good `A266MUBU1AYB8-current/boot.img`
per `notes/direct-boot/RESCUE.md`.
