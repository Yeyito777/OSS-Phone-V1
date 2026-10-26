# Stage 2: OSS Phone's own init as PID 1

Everything here is tried **only from the RECOVERY partition**, once, through the
BCB-free `reboot-recovery` entry, so a failed boot returns to the untouched BOOT
(`notes/stage2/CANARY-RUNBOOK.md`). No image whose `/init` changes ever goes to
BOOT: `flash-boot-inplace.sh` refuses it, and v2/v3 (on BOOT) both reset-looped
(`notes/stage2/V2-BOOTLOOP-ROOTCAUSE.md`, `V3-BOOTLOOP-ROOTCAUSE.md`).

## `oss-init` v4 — containment

On the OSS Phone kernel (`CONFIG_PID_NS`, `CONFIG_DEVTMPFS`, no DEFEX) the
kernel execs `oss-init` as the real PID 1. It clones Android's whole init into
a new PID + mount namespace whose root is a **private tmpfs copy of the ramdisk
in the normal Magisk layout** (`/init` = magiskinit), so magiskinit and stock
init run exactly as in today's boot without touching oss-init's rootfs.

- **No mounts** in oss-init's own namespace; it watches `/dev/kmsg` instead.
- **Fallback = the stock init only**, exec'd through an fd held from the start.
- **Early failure** (before Samsung's `sec_reboot` registered) → exec stock init.
  **Late failure** → controlled `reboot(2)`, which then writes the normal reason
  and lands on BOOT. A reboot requested by the contained Android → performed.
- No `return`/`exit` on the PID-1 path: every path ends in an exec or a reboot.

Design: `notes/stage2/DESIGN.md` §5.6. Validation (real namespaces, the real
merged ramdisk, the real magiskinit and stock init):
`notes/stage2/V4-CONTAINMENT-VALIDATION.md`.

| File | Role |
| --- | --- |
| `oss-init.c` | the real PID 1 / supervisor |
| `overlay.d/oss-contain.rc` | liveness line on `/dev/kmsg` (no mount) |
| `build-stage2-v4.sh` | today's BOOT + oss-init → recovery-slot image |
| `canary/build-recovery-slot.sh` | any BOOT + vendor_boot → a v2 recovery-slot image (+ marker) |
| `canary/build-canary-recovery.sh`, `canary/*.c` | the panic canary and `reboot-recovery` |
| `test/run-tests.sh`, `test/stub-*.c` | the off-device test bench |

```sh
# research host (whale): aarch64 gcc, magiskboot, mkbootimg/unpack_bootimg,
# avbtool, xz, lz4, the project AVB key, the OSS-kernel BOOT and vendor_boot.
scripts/native-init/build-stage2-v4.sh OUT_DIR       # recovery-slot-stage2-v4-…img
scripts/native-init/canary/build-recovery-slot.sh OUT_DIR   # recovery-slot-moon-…img
scripts/native-init/test/run-tests.sh all            # live, layout-crash, hang, late-reboot, contained-reboot
```
Outputs are deterministic and whitelisted by sha256 in
`scripts/direct-boot/flash-recovery-inplace.sh`; built images are git-ignored
(`/images/oss-stage2/`).

## `oss-pid1` — pre-init stepping stone

`oss-pid1.c` + `build-boot-image.sh`: an `/init` that owns PID 1 and then execs
the stack (no containment). Kept as a bisection point; like everything changing
`/init`, it is only to be tried from the recovery slot.
