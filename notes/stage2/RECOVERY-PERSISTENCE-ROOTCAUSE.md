# Why a reset during a recovery boot re-enters recovery (Stage 0), and the fix

Stage 0 (2026-10-26): `reboot(2)` "recovery" → stock recovery; the user forced a
restart (Power+VolDown) while recovery was on screen; the phone **booted back
into recovery**. "Reboot system now" then returned it to the system cleanly.
BOOT `8c0461ee`, RECOVERY `9ab6cefe`, misc BCB all zero throughout — nothing we
wrote. Logs copied read-only to `~/Desktop/a26-rescue/stage0-20261026/`.

## Root cause: recovery selection lives in PMU "alive" registers, not misc
- The recovery binary did **not** choose to re-enter: both sessions log
  `read_reboot_recovery_cause_file … No such file … [UNKNOWN]` and
  `save_recovery_cause() : reboot_reason=UNKNOWN`. The **bootloader** booted the
  recovery partition both times; the second session merely noticed the first was
  interrupted (`copy_file /efs/recovery/prev_tmp_recovery.log → recovery_sudden_reset.log.0`).
- The selector is a PMU register in the `11860000.exynos-pmu pmu_alive` block
  (confirmed in `/proc/iomem`), written by the kernel's restart handlers:
  - `exynos-reboot` (`drivers/power/reset/exynos-reboot.c`): on `reboot(…,"recovery")`
    it writes `REBOOT_MODE_RECOVERY (0x01)` to `reboot-cmd-offset = 0x810`
    (PMU base `0x11860000`). **Nothing writes it back to `0x00` (NORMAL)** —
    not the probe, not a normal reboot (`exynos_reboot_parse` only writes for
    charge/bootloader/recovery/sfactory; a normal/empty cmd writes nothing).
  - `sec_reboot` (`drivers/samsung/sec_reboot.c`): writes `panic_inform` =
    `SEC_RESET_REASON_RECOVERY` for "recovery", and on a normal reboot calls
    `sec_set_reboot_magic(SEC_REBOOT_NORMAL=0x4E, …)` on `magic_inform` and
    writes `panic_inform = SEC_RESET_REASON_UNKNOWN`. (DT offsets: `magic-inform`,
    `panic-inform`.)
- These are **alive-domain** registers: retained across a warm/SW reset and a
  **PMIC hard reset** (Power+VolDown). They are cleared only by (a) a clean
  through-kernel reboot that runs `sec_reboot` (which rewrites the magic to
  NORMAL and the reason to UNKNOWN — this is what "Reboot system now" does), or
  (b) a full power-off (alive domain loses power).

## Why this breaks the canary as designed
A **kernel panic does not clear these registers.** On panic, `sec_reboot`'s
handler runs with `cmd="panic"`, whose branch is *intentionally blank* for
`panic_inform`, and `exynos_reboot_parse(NULL)` writes nothing to `0x810`. So
after our canary panics, the recovery selection is still set → the bootloader
boots the recovery slot again → **loop.** Stage 0's manual reset exercised the
same persistence, safely (stock recovery, no flash), and revealed that the
panic-canary would have looped. Good that we used the zero-write stage first.

(Whether the bootloader keys off `0x810` or the `sec` informs we cannot tell
from the kernel alone — we don't have sboot. The fix below clears all of them,
so it holds either way.)

## Fix: clear the recovery selection as the test image's FIRST action
A recovery-slot test image must, before doing anything that can fail, set every
recovery-selection PMU register back to NORMAL. Then a later panic/hang/reset
leaves NORMAL selected and the next boot is BOOT. Exactly the operations the
kernel already performs, done up-front:

```
regmap_write(pmureg, 0x810 /*reboot-cmd*/, 0x00 /*REBOOT_MODE_NORMAL*/);
sec_set_reboot_magic(SEC_REBOOT_NORMAL /*0x4E*/, SEC_REBOOT_END_OFFSET /*16*/, 0xFF);
regmap_write(pmureg, panic_inform, SEC_RESET_REASON_UNKNOWN /*SEC_RESET_REASON_PREFIX|0x0*/);
```
`pmureg`, `panic_inform` and `magic_inform` come from the `exynos-reboot` /
`sec-reboot` DT nodes (`samsung,syscon-phandle`, `reboot-cmd-offset`,
`magic-inform`, `panic-inform`), so the code needs no hard-coded base.

No userspace path can do this: `CONFIG_DEVMEM` is **off** (no `/dev/mem`), there
is no regmap debugfs and no reboot-mode sysfs. It must be kernel code. Two ways,
**for the kernel agent** (who owns the KMI-matched build and can load-test it):

1. **Module `oss-rebootmode-clear.ko`** in the recovery-slot ramdisk, doing the
   three writes in `module_init` (look up the syscon + offsets from DT), loaded
   by the test image's init as its first syscall (`finit_module`). Simplest;
   runs at first userspace, before the risky containment/panic.
2. **Early-initcall patch** in a recovery-slot-only kernel variant: the same
   writes in a `core_initcall`, before any userspace. Most robust (even an
   early-userspace panic can't loop), but a third kernel variant. Never in the
   BOOT kernel.

Either is used **only** in recovery-slot test images. The BOOT kernel is
untouched, so normal recovery/download requests still work.

## Validation path (nothing flashed yet)
The new canary = a recovery image that does the clear, then exits PID 1 to
panic. If it returns to BOOT **hands-free**, the mechanism is proven and the
same clear-first prologue goes into the stage-2 containment image (oss-init's
first action becomes `finit_module(oss-rebootmode-clear.ko)` before the clone).

## Escape if the clear is absent or fails (deterministic, attended)
A stuck recovery selection is **recoverable without a full power-off**, because
Download mode is a bootloader key combo (VolUp+VolDown+USB), independent of the
PMU reboot-cmd:
1. Enter Download mode (the escape watcher then flashes **stock recovery** —
   `9ab6cefe` — plus the MISC-clear to the recovery slot).
2. Restart (Power+VolDown). The PMU still says recovery, so the bootloader boots
   the recovery slot — now **stock recovery**.
3. In stock recovery, Volume keys → **"Reboot system now"**, Power. Its clean
   reboot rewrites the PMU to NORMAL → the phone boots **BOOT**.
BOOT (`8c0461ee`) is never touched on any path.

## Recommendation
Route the kernel `oss-rebootmode-clear` piece to the kernel agent (option 1,
the module, is enough and lowest-risk). Until it exists and a new canary proves
hands-free return, recovery-slot boots of our own images are **attended-only**
with the escape above. The containment work itself (oss-init v4) is unaffected
and already validated off-device.
