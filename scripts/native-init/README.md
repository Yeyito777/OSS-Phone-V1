# Stage 2: OSS Phone's own init as PID 1

Two artifacts, lowest-risk first. See `notes/stage2/DESIGN.md` for the full
research and `notes/stage2/TEST-PLAN.md` for the attended morning test. **Do not
flash unattended** — there is no A/B slot.

## `oss-init` — containment (the real stage 2)

On the **OSS Phone kernel** (`CONFIG_PID_NS`, `CONFIG_DEVTMPFS`, no DEFEX),
`oss-init` is the ramdisk `/init`, so the kernel execs it as the **real PID 1**,
and it stays PID 1. It runs Android's entire init (the existing magiskinit →
stock-init → Moon stack) as its child in a **new PID + mount namespace**, so
Android sees `getpid()==1` in its namespace and does the full hardware bring-up
unchanged. OSS Phone owns the real init seat; Android is contained.

Iron rule — **oss-init can never exit/return**; every path execs the stock stack
or loops+reboots. The fallback is always a re-exec in oss-init's own (pristine)
namespace, so a failure stays in THIS boot and goes uncontained — never a reset
loop. Validated under real namespaces on whale (healthy→supervise;
crash→uncontained with a switch_root-style mount-move pristine-ns proof;
hang→kill+uncontained):
- setup fails (e.g. no PID_NS) → boot stock stack uncontained (today's boot)
- container crash / early death / SwitchRoot failure → clean uncontained boot
- container hang (no kernel-log marker in 60 s) → kill + uncontained boot

v3 mounts **nothing** (v2's `/oss-run` tmpfs broke Android's SwitchRoot and
boot-looped — see `../../notes/stage2/V2-BOOTLOOP-ROOTCAUSE.md`). Liveness rides
the global kernel log; there is no reboot relay (a contained `adb reboot` drops
to an uncontained boot — the escape hatch).

| File | Role |
| --- | --- |
| `oss-init.c` | the real PID 1 / host supervisor |
| `overlay.d/oss-contain.rc` | one `/dev/kmsg` liveness marker (no mount) |
| `build-stage2-image.sh` | build on the OSS-kernel image `8c0461ee…` |

```sh
# research host (whale): needs aarch64 gcc, magiskboot, avbtool, xz, the AVB key,
# and the OSS-kernel base image ($HOME/oss-kernel/out/boot/boot-oss-mirror-…img).
scripts/native-init/build-stage2-image.sh OUT_DIR
```
Output `boot-oss-stage2-A266MUBU1AYB8.img`. The build verifies the kernel is
byte-identical to the OSS base, `/init` is oss-init, `/init.magisk` is the base
magiskinit byte-for-byte, the Moon overlay files are unchanged, `/init.stock` is
the decompressed backup, and the relay rc is present. Built image is git-ignored
(`/images/oss-stage2/`).

## `oss-pid1` — pre-init stepping stone (safe fallback)

`oss-pid1.c` + `build-boot-image.sh`: OSS Phone's own binary as `/init` that
owns PID 1 then *execs* the stack (no containment), built on the **stock**
kernel. Boot is bit-for-bit today's. It is the lowest-risk artifact and a
bisection point — "does our-init-as-PID-1 boot at all, before namespaces?" — if
`oss-init` containment misbehaves on-device.

## Rollback

A BOOT flash leaves data, Magisk and `/data/adb` untouched. To recover, flash
`images/kernel/boot-oss-mirror-…img` (OSS kernel, passthrough) or the
stock-kernel `images/kernel/rollback/boot-moon-direct-…img` per
`notes/direct-boot/RESCUE.md`.
