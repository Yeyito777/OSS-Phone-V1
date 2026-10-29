# Stage 2: OSS Phone's own init as PID 1

Everything here is tried **only from the RECOVERY partition**, once, through the
BCB-free `reboot-recovery` entry, so a failed boot returns to the untouched BOOT
(`notes/stage2/CANARY-RUNBOOK.md`). An image whose `/init` changes goes to
BOOT only if the owner says so: `flash-boot-inplace.sh` stops on it unless
given `--allow-init-change`, because v2/v3 (on BOOT) both reset-looped and
needed a Download-mode rescue (`notes/stage2/V2-BOOTLOOP-ROOTCAUSE.md`,
`V3-BOOTLOOP-ROOTCAUSE.md`).

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
| `build-slot-image.sh` | today's BOOT + oss-init (`containment`) or oss-pid1 (`plain`) → recovery-slot image |
| `rebootmode-clear.h` | the shared first action: load `oss-rebootmode-clear.ko` |
| `bootcount-misc.h` | boot-attempt counter in misc (record v1, `docs/bootcount-misc-record.md`); the only writer |
| `oss-pid1.c` | plain-slot `/init`: clear, put magiskinit back at `/init`, exec it |
| `canary/build-recovery-slot.sh` | any BOOT + vendor_boot → a v2 recovery-slot image (+ marker) |
| `canary/build-canary-recovery.sh`, `canary/*.c` | the panic canary and `reboot-recovery` |
| `test/run-tests.sh`, `test/stub-*.c` | the off-device test bench |
| `test/run-bootcount-tests.sh`, `test/misc-tool.py` | counter bench: file, CLI, real block device |

```sh
# research host (whale): aarch64 gcc, magiskboot, mkbootimg/unpack_bootimg,
# avbtool, xz, lz4, the project AVB key, the OSS-kernel BOOT and vendor_boot.
scripts/native-init/build-slot-image.sh OUT_DIR containment   # recovery-slot-stage2-v4-…img
scripts/native-init/build-slot-image.sh OUT_DIR plain         # recovery-slot-moon-…img
scripts/native-init/canary/build-canary-recovery.sh OUT_DIR   # canary v2 + reboot-recovery
# (canary/build-recovery-slot.sh OUT_DIR PAYLOAD NAME is called by build-slot-image.sh)
scripts/native-init/test/run-tests.sh all            # containment matrix + no-clear and handoff cases
scripts/native-init/test/run-bootcount-tests.sh all  # misc boot counter (device tier needs sudo + losetup)
```
Outputs are deterministic and whitelisted by sha256 in
`scripts/direct-boot/flash-recovery-inplace.sh`; built images are git-ignored
(`/images/oss-stage2/`).

## The clear comes first

Every recovery-slot `/init` (oss-init, oss-pid1, the canary) first loads the
kernel agent's `oss-rebootmode-clear.ko`, which sets the PMU reboot registers to
a NORMAL boot; otherwise a failed slot boot would loop in recovery
(`notes/stage2/RECOVERY-PERSISTENCE-ROOTCAUSE.md`). If the load fails, none of
them panics or reboots on purpose: oss-init and oss-pid1 exec the stock init,
the canary execs the stock recovery init. `build-recovery-slot.sh` refuses a
payload whose `/init` is not one of these pre-inits.
