# One-shot, brick-proof test path for boot-chain changes

Hard rule (user, 2026-10-26): **nothing that changes the boot chain may be
flashed to BOOT until a failed boot provably returns to the working setup on its
own.** Three stage-2 BOOT images have now reset-looped and needed physical
Download-mode rescue. This document designs a test path where a failed test boot
returns to the known-good system **by itself, with no hands**, and the evidence
for it. **BOOT (`8c0461ee`, OSS-kernel passthrough, which passes all tests) is
never touched by this path.**

## The mechanism: one-shot boot of the RECOVERY partition

Put the experimental image in the **recovery** partition and start it exactly
once with `adb reboot recovery`. If it panics/resets, the bootloader's next boot
is normal (reboot reason is no longer "recovery"), so the phone comes back up on
the untouched BOOT.

### Evidence gathered read-only (2026-10-26, device R5CY323ES7X on `8c0461ee`)
- **Separate recovery partition exists:** `by-name/recovery → sda16`, 196608
  sectors (96 MiB), distinct from `boot`(sda14) and `vendor_boot`(sda15).
- **Recovery is currently the stock image:** `sha256(recovery) =
  9ab6cefe06b1…a91a7e8`, identical to the rescue copy
  `A266MUBU1AYB8-current/recovery.img`. So recovery is known-good and freely
  reflashable.
- **The BCB/misc is empty:** `by-name/misc → sda21`, first 32 bytes all `\0`,
  no `boot-recovery` string anywhere. So on this device recovery is **not**
  selected via a sticky BCB command during normal operation — it is selected by
  the one-shot reboot reason. (A sticky BCB would be the thing that could cause a
  recovery boot loop; it is absent.)
- **Precedent:** Magisk's Samsung "recovery mode" install relies on exactly this
  — Magisk lives in recovery, you boot to recovery to get the modified system,
  and a normal power-on boots stock unrooted. That only works if recovery
  selection is one-shot.
- Current `/proc/cmdline` has no `force_normal_boot`/`skip_initramfs`; reboot
  reason is reflected (`ro.boot.bootreason`).

### What this still does NOT prove (must be confirmed on-device, attended)
1. That a **kernel panic** during a recovery boot returns to BOOT rather than
   re-selecting recovery. This follows from one-shot reason-based selection
   (a panic sets reason=`kernel panic`, not `recovery`), but it is not proven
   read-only.
2. That the bootloader loads a **boot-format (v4) image from the recovery
   partition** usefully. The stock recovery is a **header-v2 standalone** image
   (own kernel + 34 MiB ramdisk + dtb/dtbo; no `vendor_boot` dependency), so a
   v4 boot image placed in recovery may not get the `vendor_boot` ramdisk and
   may not boot. **A recovery test image must be built on the stock
   `recovery.img` base** (inject oss-init into the recovery ramdisk,
   Magisk-recovery style), not on `boot.img`.
3. That our image, booted as recovery, runs the full system rather than the
   recovery UI (needs the `force_normal_boot`/RECOVERYMODE handling Magisk uses).

## MANDATORY self-test FIRST — prove one-shot selection with ZERO risk
Before trusting the mechanism with any experimental image, prove the selection
is one-shot using the **stock recovery that is already installed** (cannot loop,
because stock recovery always boots):
1. `adb reboot recovery` → the phone boots the **stock recovery UI**
   ("No command" / Android recovery menu).
2. In recovery, choose **Reboot system now**.
3. The phone must come up in the **normal system** (Moon on `8c0461ee`).
4. `adb reboot` (a plain reboot) → must boot the **normal system again**, NOT
   recovery.
→ If steps 3–4 hold, recovery selection is **one-shot** (not sticky): safe to
build on. If the phone re-enters recovery on a normal reboot, the mechanism is
**unsafe on this device — stop and do not use it.**

## Then (optional, attended) confirm panic-returns-to-BOOT
Only if the user wants certainty on open question #1, with rescue ready: flash a
**deliberately fast-failing** recovery image (e.g. a recovery image whose init
panics immediately), `adb reboot recovery`, and confirm the phone returns to
BOOT on its own within ~30 s. Rollback is always `tools/samloader flash -p
RECOVERY A266MUBU1AYB8-current/recovery.img` (or in-OS dd) — BOOT is untouched.

## Using it to test stage-2 containment (once selection is proven)
1. Keep BOOT = `8c0461ee` (untouched).
2. Build the containment image **on the stock `recovery.img` base** (not
   `boot.img`): the recovery ramdisk + oss-init + the private-rootfs magiskinit
   container (see DESIGN.md §5), with Magisk-recovery `force_normal_boot`
   handling so a recovery boot runs the full system.
3. Flash it to **recovery** only. `adb reboot recovery` to try it once.
4. Pass → oss-init is PID 1, Android contained, Moon up. Fail/panic → phone
   returns to BOOT by itself; reflash stock recovery at leisure.

## Rollback (always available, BOOT never at risk)
- Recovery is non-critical. Restore it anytime:
  `samloader flash -p RECOVERY ~/Desktop/a26-rescue/A266MUBU1AYB8-current/recovery.img`
  (sha256 `9ab6cefe…`), or in-OS `dd` with read-back verify.
- BOOT stays `8c0461ee` throughout, so a normal power-on is always the
  known-good passthrough system.

## Recommended PRIMARY validation first: a zero-risk userspace harness
The one-shot path still boots experimental code on real hardware. A strictly
safer validation that changes **no** partition: on a booted phone (OSS kernel,
has PID_NS), run oss-init's containment logic as a **non-PID-1 root process** —
`clone(CLONE_NEWPID|CLONE_NEWNS)`, set up the private rootfs, run the real
magiskinit inside — and observe how far a contained Android gets, without being
init and without touching the live system's rootfs. This proves the
private-rootfs + magiskinit design on real hardware with **no boot risk at
all**. It needs the phone (currently the UI agent's) and care not to disturb the
live session (run in a throwaway namespace, no real partition mounts). This
should gate any recovery-partition flash.

## Alternatives considered
- **kexec from the OSS kernel:** would let a tested kernel hand off to another
  without a bootloader round-trip, but `CONFIG_KEXEC` is not enabled in the OSS
  kernel and adding it is another kernel change; not pursued now.
- **A/B slot:** none on this device.
- **BCB-based recovery:** rejected — writing a sticky `boot-recovery` BCB is the
  one thing that could cause a recovery loop; we rely on the reason-based
  one-shot instead and keep the BCB empty.
