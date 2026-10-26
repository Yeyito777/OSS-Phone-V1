# Recovery-based stage 2: a full system boot from the RECOVERY partition

Goal: run the stage-2 containment boot (oss-init PID 1, Android in a PID+mount
namespace) from the **recovery** partition, so it is tested through the
brick-proof one-shot path (CANARY-RUNBOOK.md) with BOOT left at the known-good
`8c0461ee`. This is the follow-on to the canary; it is **built in stages**, and
only the foundation stage is built so far — the containment layer needs
on-device iteration, which the canary makes safe.

## Why recovery can boot a full system (research)
- The recovery partition (sda16, 96 MiB) holds a **standalone header-v2 image**:
  its own kernel + a **full 34 MiB recovery ramdisk** + dtb + recovery_dtbo, and
  **no dependence on vendor_boot**. The recovery ramdisk already carries
  `lib/modules` (304 `.ko` + `modules.load.recovery`), an fstab, `/system/bin/init`
  and `/system/bin/recovery` — i.e. everything needed to bring the hardware up
  and mount `/system` by itself. So, unlike the boot image (which pairs with
  vendor_boot), a recovery image is self-sufficient.
- **Magisk RECOVERYMODE** turns a recovery boot into a *system* boot. With
  `.backup/.magisk` containing `RECOVERYMODE=true`, magiskinit runs
  (`getinfo.cpp:177`): `skip_initramfs = emulator || !check_key_combo()`.
  `check_key_combo()` waits briefly for **Volume Up**.
  - Volume Up **not** held → `skip_initramfs = true` → `legacy_system_as_root()`
    → boots the **system** (rooted).
  - Volume Up **held** → stays in real recovery.
  This is exactly how Samsung Magisk-in-recovery installs boot the rooted
  system, and it is the mechanism the recovery stage-2 uses (entered with no
  keys via the BCB-free `reboot-recovery`).
- **Kernel swap for PID_NS:** the stock recovery kernel lacks `CONFIG_PID_NS`.
  Containment needs the **OSS kernel**. Because the OSS kernel keeps the frozen
  KMI, the stock recovery `.ko`s still load (the kernel agent verified all 306).
  So the recovery stage-2 image uses the OSS `Image` with the stock recovery
  ramdisk + dtb + recovery_dtbo.

## Build stages
1. **Foundation (built): `recovery-osskernel-A266MUBU1AYB8.img`** — stock
   recovery image with only the kernel swapped to the OSS `Image`
   (`scripts/native-init/canary/build-recovery-osskernel.sh`). Deterministic,
   sha256 `aab7a30153acb342edc397d3c471098f4cb0f5ceb55f196775a32950df3ac410`,
   on whale `~/oss-kernel/.canary/r2a/`. Verified: kernel = OSS Image; ramdisk,
   dtb unchanged. Booting it (post-canary, via the one-shot) shows the **OSS
   kernel running the stock recovery UI from the recovery slot** — proving the
   kernel swap and that a real (non-panic) recovery boot of our packaging
   returns to BOOT on normal exit. No containment yet.
2. **System-from-recovery (built): `recovery-slot-moon-A266MUBU1AYB8.img`** —
   a cleaner approach than the RECOVERYMODE trick above. Instead of patching the
   recovery ramdisk, pack the phone's **normal boot** into the recovery slot:
   the bootloader assembles a normal boot from BOOT (v4 kernel + generic
   ramdisk) + vendor_boot (vendor ramdisk fragments + dtb + cmdline/bootconfig),
   so the recovery image (`build-recovery-slot.sh`) is a standalone v2 image
   carrying exactly that — BOOT's kernel, the ramdisk
   `vendor_ramdisk00 + vendor_ramdisk01 + BOOT-ramdisk` in v4 load order, the
   stock recovery dtb (verified == vendor_boot dtb) and recovery_dtbo, and the
   stock recovery cmdline (which already carries `bootconfig
   buildtime_bootconfig=enable loop.max_part=7`). The merged ramdisk has **no**
   `/system/bin/recovery` or `/sbin/recovery`, so AOSP init's `IsRecoveryMode()`
   (`util.cpp:672`, a pure `access()` check) and magiskinit both take the normal
   2SI path — **no RECOVERYMODE needed**. Deterministic, sha256
   `fc97466761d1ba0173181d023f1c568186d8f98e6340e88d27c59213dddb03a6`
   (`images/oss-stage2/recovery-slot/`, whale `~/oss-kernel/.recslot/outA/`).
   Validated under qemu + namespaces: the real magiskinit, run on the merged
   tree, restored `.backup/init.xz` → `/init` and exec'd the real stock init,
   exactly as in a normal boot. Verified at build: every component byte-matches
   its source; header v2.
   - **Boot-reason caveat:** entered via `reboot-recovery` the boot reason is
     `recovery`, and `moon-direct-policy.sh:48` sends a `*recovery*` boot to
     **Android**, not Moon. So this image boots the **full stock system** from
     the recovery slot (the milestone: the whole vendor stack + Android come up
     from recovery, BOOT untouched), not Moon specifically. Showing Moon from
     recovery needs a one-line policy exception for a trusted recovery marker,
     or a different entry reason — a cosmetic follow-up; the hard part (the whole
     system boots from the standalone recovery image) is what this proves.
3. **Containment (the actual stage-2 goal):** add oss-init as the real `/init`
   per DESIGN.md §5.6 — oss-init PID 1, which clones a PID+mount namespace,
   gives the container a **private rootfs** with the normal Magisk layout, and
   runs magiskinit→stock-init→Moon inside it; fallback execs the stock init.
   The Step-1 harness (STEP1-HARNESS-RESULT.md) already proved magiskinit runs
   correctly as a namespaced PID 1 in a private rootfs on this hardware.
   **Packaging is now trivial given stage 2:** `build-recovery-slot.sh` takes
   the source BOOT image as its 2nd argument, so once a BOOT image with oss-init
   as `/init` exists (the §5.6 build, not yet implemented), running
   `build-recovery-slot.sh OUT <that-boot.img>` drops it straight into the
   recovery slot. The remaining work is the oss-init private-rootfs
   implementation itself, not the recovery packaging.

## Why this is safe to iterate (unlike the BOOT attempts)
Every stage is flashed only to **recovery**, entered via the BCB-free one-shot,
and a failure (panic, hang, wrong path) resets to the untouched BOOT by itself —
the property the canary proves. So stages 2–3 can be iterated on-device without
the no-A/B brick risk that stopped the BOOT-partition attempts. BOOT stays
`8c0461ee` throughout; stock recovery is restored with
`flash-recovery-inplace.sh --restore-stock` between iterations.

## Open points (to resolve on-device, after the canary)
- Does the bootloader load the standalone v2 recovery-slot image and boot the
  full system from it as expected? (qemu validates the ramdisk logic, not the
  bootloader's handling of a system-shaped payload in the recovery slot.)
- SELinux / first-stage mount of `/system` from the merged ramdisk on the real
  device — the vendor fragments carry the fstab and modules, so this should
  match a normal boot, but it is unproven from the recovery slot.
- oss-init's private-rootfs construction in PID 1 (copy vs overlayfs) — the
  remaining implementation risk flagged in DESIGN.md §5.6 (stage 3).
- The boot-reason → Android policy (above): decide whether to add a trusted
  recovery-marker exception so Moon (not plain Android) comes up from recovery.

## Status (2026-10-26, rebuilt with oss-rebootmode-clear.ko v2 `7130cf3c`)
Built, deterministic, whitelisted, unflashed; every image loads
`oss-rebootmode-clear.ko` first (RECOVERY-PERSISTENCE-ROOTCAUSE.md):
1. Canary `71d68ced` (CANARY-RUNBOOK.md Stage 1; module loaded with `require_mode=1`).
2. System-from-recovery `cc406f02` (`build-slot-image.sh plain`: oss-pid1 →
   magiskinit → Moon, with the `oss-recovery-slot` marker for the policy).
3. Stage-2 v4 containment `f0b7263a` (`build-slot-image.sh containment`).
The earlier OSS-kernel calibration (`aab7a301`) and slot images without the
clear (`fc974667`, `5a445db7`, `b67b68d3`) are retired and off the whitelist.
Order: module on-device test (kernel agent) → canary → 2 → 3. Stage 2 needs the
updated `moon-direct-policy.sh` on /data first.
