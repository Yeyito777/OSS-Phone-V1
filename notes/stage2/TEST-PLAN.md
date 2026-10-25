# Stage 2 (`oss-pid1`) — attended morning test plan

**Do not flash unattended.** This test puts OSS Phone's own binary at ramdisk
`/init` (PID 1). The happy path is bit-for-bit today's Moon boot; the one new
risk is an oss-pid1 crash *before* its handoff, which would panic PID 1 and
likely need a physical Download-mode rescue. Run it only with the device lead
present and the Mac + rescue kit at hand.

## Preconditions
- Battery ≥ 50 %, phone on the Mac's USB, adb authorized (`adb devices` shows
  `R5CY323ES7X  device`).
- Rescue kit ready (`notes/direct-boot/RESCUE.md`): `~/Desktop/a26-rescue/`
  with `A266MUBU1AYB8-current/boot.img` (sha256 `365a8b3c…d4cfc12e08a0`) and
  `tools/samloader`.
- Image present and hashed:
  `images/oss-pid1/boot-oss-pid1-A266MUBU1AYB8.img`
  sha256 `9a63cf28b7dc0fef3c3cd2261c135f1a78c39f283ed0d7466dacef652e64dea4`.
- Confirm the phone currently boots Moon normally (baseline) before changing
  anything.

## What the user must do physically
- **Have hands on the phone for the whole test.** Hold **Power + Volume Down**
  ~7–10 s if it ever stops responding.
- Flashing requires Download mode and a manual restart (this device does not
  auto-reboot out of Download mode — see RESCUE.md). Be ready to do the
  Power+VolDown restart after every samloader flash.
- During the Moon boot's volume-key window you may press **Volume Up** to force
  a normal Android boot if desired (unchanged from today).

## Steps
1. **Verify the image** on the Mac:
   `shasum -a 256 images/oss-pid1/boot-oss-pid1-A266MUBU1AYB8.img`
   must equal `9a63cf28…e64dea4`.
2. **Flash BOOT** (Download mode, per RESCUE.md Level 2):
   ```
   adb reboot download
   cd ~/Desktop/a26-rescue
   tools/samloader detect
   tools/samloader flash -p BOOT <path>/boot-oss-pid1-A266MUBU1AYB8.img
   ```
   Then **manually restart** the phone (Power+VolDown ~7–10 s).
3. **Observe the boot.** Expected: normal boot animation, then Moon on screen.
4. **Confirm Moon reached readiness** and timing matches baseline:
   `adb wait-for-device && adb shell su -c 'getprop moon.session.ready'` → `1`,
   and kernel-time to Moon ≈ 34 s (adb up ≈ 30 s), i.e. **no regression** vs.
   today's direct boot.
5. **Confirm oss-pid1 actually owned PID 1** (the point of stage 2):
   ```
   adb shell su -c 'dmesg | grep oss-pid1'
   # expect: oss-pid1: OSS-Phone pre-init running as PID 1; chaining to stage-1 boot
   # if it rolled out of the ring buffer, check pstore:
   adb shell su -c 'cat /sys/fs/pstore/console-ramoops* 2>/dev/null | grep oss-pid1'
   ```
   Also confirm PID 1 is stock init as expected afterward:
   `adb shell su -c 'cat /proc/1/comm'` → `init`.
6. **Exercise the Moon fallbacks still work** (regression check, unchanged from
   stage 1): `autostart.sh skip-once` → reboots to Android; a volume-key press
   in the window → Android. Then re-enable and confirm Moon again.

## Expected result
- Boot time and behavior **indistinguishable from today's direct boot**, plus
  the `oss-pid1` PID-1 marker present. That is a pass: OSS Phone's own init now
  owns PID 1 with zero behavioral regression.

## If it fails
- **Black screen / boot loop / no adb within ~90 s:** the pre-init likely
  crashed before handoff (risk #2 in DESIGN.md). Rescue: RESCUE.md Level 3
  (Power+VolDown restart; at black screen hold VolUp+VolDown with USB; VolUp to
  enter Download) → RESCUE.md Level 2 flash of the known-good
  `A266MUBU1AYB8-current/boot.img`. Data, Magisk and /data/adb are untouched by
  a BOOT flash.
- **Boots plain Android (no Moon), adb works:** the primary handoff failed and
  the `/init.stock` fallback ran (dmesg will show
  `oss-pid1: primary init exec failed; falling back to stock Android init`).
  The phone is safe; restore the known-good BOOT at leisure and investigate the
  magiskinit handoff before retrying.
- **Boots Moon but no oss-pid1 marker:** build/flash mismatch — re-verify the
  flashed partition hash (`adb shell su -c 'sha256sum /dev/block/by-name/boot'`
  should match the built image) before drawing conclusions.

## Risk rank for this test
1. oss-pid1 pre-handoff crash → PID-1 panic → physical Download-mode rescue.
   Low probability, high effort to recover; **this is why the test is attended.**
2. Everything else ≈ today's risk; the happy path is byte-identical and the
   fallback path is a normal Android boot.

Recommendation: worth running **once, attended**, as it validates real PID-1
ownership at near-zero behavioral cost. If a bad boot occurs, restore the
known-good BOOT and keep stage 1 as the shipping default; stage 2's value is
the groundwork + proof, not a required daily-driver change.
