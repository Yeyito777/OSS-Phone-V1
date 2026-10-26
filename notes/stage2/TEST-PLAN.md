# Stage 2 (`oss-init` containment) — attended morning test plan

> **SUPERSEDED — DO NOT USE THIS PLAN. DO NOT FLASH `4d27b69b` OR `eaeb4494` TO
> BOOT.** Both boot-looped on the device and needed physical rescue. Per the
> user's hard rule (2026-10-26), nothing that changes the boot chain may be
> flashed to BOOT until a failed boot provably self-recovers. All stage-2
> testing now goes through `notes/stage2/ONESHOT-TEST-PLAN.md` (recovery
> partition, one-shot, BOOT untouched), and only after the corrected
> private-rootfs design (DESIGN.md §5.6) is validated. This file is kept for
> history only.

**Do not flash unattended.** This boots OSS Phone's own `oss-init` as the real
PID 1 and runs Android in a PID + mount namespace. The iron rule makes every
failure fall back to a clean uncontained boot in the same boot, but one narrow
case (a container hang after the liveness marker but before adbd) can still need
a physical Download-mode rescue. Run only with the device lead present and the
rescue kit ready.

> This is the **v3** image `eaeb4494…`. The first build `4d27b69b…` boot-looped
> (a relay tmpfs broke Android's SwitchRoot in both the contained init and the
> fallback). **Do not flash `4d27b69b`.** v3 mounts nothing and its fallback runs
> from a pristine namespace; see `V2-BOOTLOOP-ROOTCAUSE.md`.

This test **layers on the kernel agent's plan** (`images/kernel/TEST-PLAN.md`).
Do that one first: it proves the OSS kernel itself boots with the prebuilt
modules. Only then test stage-2 containment on top of it.

## Preconditions
- Battery ≥ 50 %, phone on the Mac's USB, adb authorized.
- Rescue ready (`notes/direct-boot/RESCUE.md`). Keep these images handy:
  - `images/kernel/boot-oss-mirror-A266MUBU1AYB8.img` (`8c0461ee…`) — OSS kernel,
    **passthrough** (today's boot on the OSS kernel). The fallback if stage 2
    misbehaves.
  - `images/kernel/rollback/boot-moon-direct-A266MUBU1AYB8.img` (`3bf29162…`) —
    stock kernel, today's shipping boot. The ultimate rollback.
- Stage-2 image present:
  `images/oss-stage2/boot-oss-stage2-A266MUBU1AYB8.img`
  sha256 `eaeb4494567f10459bc91f6c3eb156709f56bb066da0913bc307940b303c17e2`.

## What the user must do physically
- Stay at the phone the whole test. If it stops responding, hold **Power +
  Volume Down** ~7–10 s.
- With containment active, a software reboot is proxied by our PID 1 via the
  relay; if the relay is not working, `adb reboot download` will NOT reach
  Download mode — use the physical key combo (RESCUE.md Level 3).

## Stage A — OSS kernel alone (passthrough)
Run the kernel agent's **Test 2** (`images/kernel/TEST-PLAN.md`) with
`boot-oss-mirror-A266MUBU1AYB8.img`. Must pass fully (Moon ≈34 s; 306 modules;
no KMI/module errors; root, display, audio, Wi-Fi, charging, suspend, clean
reboot). Also confirm the namespace primitives the stage-2 design relies on, in
the Alpine chroot as root:
```
unshare -p -f --mount-proc sh -c 'echo $$'        # prints 1
mkdir -p /tmp/d && mount -t devtmpfs devtmpfs /tmp/d && ls /tmp/d   # populated
```
If Stage A fails, stop — stage 2 cannot be more stable than its kernel.

## Stage B — stage-2 containment
1. **Verify the image:**
   `shasum -a 256 images/oss-stage2/boot-oss-stage2-A266MUBU1AYB8.img` =
   `eaeb4494…`.
2. **Flash.** The image carries the OSS kernel, so a kernel change from whatever
   is installed is expected — use the device lead's in-OS script with the kernel
   flag (it backs up and read-back-verifies):
   ```
   scripts/direct-boot/flash-boot-inplace.sh --allow-kernel-change \
       images/oss-stage2/boot-oss-stage2-A266MUBU1AYB8.img
   ```
   (Or samloader per RESCUE.md, then a manual restart.) Consider `--dry-run`
   first to exercise the checks and make the backup without writing.
3. **Reboot and observe.** Expected: normal boot animation, then Moon on screen,
   in roughly the usual time plus a little (the container boots Android as
   today).
4. **Confirm oss-init is the real PID 1 and Android is contained:**
   ```
   adb wait-for-device
   adb shell su -c 'cat /proc/1/comm'            # expect: oss-init
   adb shell su -c 'readlink /proc/1/exe'        # expect: /init (deleted) = oss-init
   # Android's init should be PID 1 *inside* its namespace, i.e. a non-1 host PID:
   adb shell su -c 'pgrep -f "^/system/bin/init|init.*second" ; ps -A | grep -w init | head'
   adb shell su -c 'dmesg | grep oss-init'       # markers: "the real PID 1; containing Android init" + "container reached second stage; supervising"
   adb shell su -c 'cat /proc/1/ns/pid'          # host pid ns
   adb shell su -c 'cat /proc/$(pgrep -o -f system/bin/init)/ns/pid'  # different ns id
   ```
   Pass if `/proc/1/comm` is `oss-init`, the Android init lives in a *different*
   PID namespace, and `moon.session.ready=1`.
5. **Confirm no regression** in the contained Android/Moon: Moon UI responsive,
   touch, display, audio, Wi-Fi, charging (`dumpsys battery` / sysfs), one
   suspend/resume.
6. **Exercise the iron-rule fallback deliberately** (should land safely):
   - *Crash → uncontained fallback:* kill the contained Android init
     (`adb shell su -c 'kill -9 $(pgrep -o -f system/bin/init)'`) → the container
     tears down and oss-init should boot **uncontained** in the same boot (Moon
     returns; `dmesg` shows `container exited; booting uncontained`;
     `/proc/1/comm` becomes `init`). This proves the fallback without a reflash,
     and is also the **escape hatch**: it drops to a plain uncontained boot you
     can reflash from.
   - *Reboot note:* in containment mode a software `adb reboot` does **not**
     reboot — it tears down the container, and oss-init answers with an
     uncontained boot (the escape hatch above). To actually power-cycle or reach
     Download mode, use the physical key combo (RESCUE.md). v3 intentionally has
     no reboot relay; this is documented, not a bug.

## Expected result
Moon on screen with `/proc/1/comm = oss-init` and Android init in its own PID
namespace, no behavioral regression, and the deliberate crash falling back to a
normal boot. That is a pass: OSS Phone is the real PID 1 with Android contained.

## If it fails
- **Black screen / boot loop / no adb within ~90 s:** a container hang (risk #1)
  or an oss-init crash. Rescue: RESCUE.md Level 3 → flash
  `images/kernel/boot-oss-mirror-A266MUBU1AYB8.img` (OSS kernel, passthrough) to
  get a working boot, or the stock-kernel `rollback/…` image.
- **Boots uncontained (Moon up, `/proc/1/comm = init`):** containment fell back
  early (see `dmesg | grep oss-init`). The phone is safe and fully working;
  investigate the containment handoff before retrying. This is the iron rule
  doing its job, not a failure of the device.
- **Moon never appears but adb works:** the container booted but wedged, or our
  PID 1 was SELinux-denied its fallback (risk #2). Reflash the passthrough image
  in-OS. If useful, rebuild with SELinux forced permissive to isolate.

## Risk rank for this test
1. Container hang after liveness, before adbd/watchdogd → watchdog reset loop →
   physical rescue. Low probability, high recovery effort. **Attended only.**
2. SELinux denies our PID-1 fallback after a late container death → Moon down,
   maybe-live adb. Medium uncertainty; mitigated by the passthrough fallback
   image and a permissive test build.
3. Everything else ≈ today's risk (iron rule → today's boot).

Recommendation: run attended after Stage A passes. Containment is the real
stage-2 milestone; if it is not yet stable on-device, keep the OSS-kernel
passthrough (Stage A) or stage 1 as the shipping default while iterating — the
groundwork, image, and validated fallback logic are the deliverable either way.
