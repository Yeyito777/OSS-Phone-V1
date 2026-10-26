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
2. **Moon-from-recovery (next, needs device iteration):** Magisk-patch the stock
   recovery ramdisk (add magiskinit as `/init`, `.backup/.magisk`
   `RECOVERYMODE=true`, the `scripts/direct-boot/overlay.d` Moon overlay),
   keep the OSS kernel. Entered with no keys → magiskinit → system boot → the
   existing direct-boot overlay stops zygote and starts Moon. Milestone: **Moon
   on screen, booted from the recovery partition.** Still no oss-init.
3. **Containment (the actual stage-2 goal):** add oss-init as the real `/init`
   per DESIGN.md §5.6 — oss-init PID 1, which clones a PID+mount namespace,
   gives the container a **private rootfs** with the normal Magisk layout, and
   runs magiskinit→stock-init→Moon inside it; fallback execs the stock init.
   The Step-1 harness (STEP1-HARNESS-RESULT.md) already proved magiskinit runs
   correctly as a namespaced PID 1 in a private rootfs on this hardware; stage 3
   wires that into the recovery boot.

## Why this is safe to iterate (unlike the BOOT attempts)
Every stage is flashed only to **recovery**, entered via the BCB-free one-shot,
and a failure (panic, hang, wrong path) resets to the untouched BOOT by itself —
the property the canary proves. So stages 2–3 can be iterated on-device without
the no-A/B brick risk that stopped the BOOT-partition attempts. BOOT stays
`8c0461ee` throughout; stock recovery is restored with
`flash-recovery-inplace.sh --restore-stock` between iterations.

## Open points for stages 2–3 (to resolve on-device)
- Does `legacy_system_as_root()` + the recovery ramdisk's fstab mount `/system`
  and bring up the vendor stack fully enough for Moon? (Recovery mounts system
  for OTA, so the pieces exist, but a full UI boot from recovery is unproven
  here.)
- SELinux: the recovery ramdisk ships its own sepolicy; a full system boot from
  it may need the system sepolicy path Magisk normally handles.
- oss-init's private-rootfs construction in PID 1 (copy vs overlayfs) — the
  remaining implementation risk flagged in DESIGN.md §5.6.
- `check_key_combo()` waits for input: confirm the no-key timeout path is taken
  under the BCB-free entry (no user at the keys) and that it is short.

## Status
Foundation built and verified. Stages 2–3 are specified here and gated on the
canary proving the recovery slot returns to BOOT on failure. Nothing flashed.
