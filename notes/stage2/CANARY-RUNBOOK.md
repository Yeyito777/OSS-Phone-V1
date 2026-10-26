# Panic-canary recovery runbook (attended, morning)

> ⛔ **STAGE 0 FAILED 2026-10-26 AND THE CANARY IS ON HOLD.** A reset during a
> recovery boot re-enters recovery: the recovery selection lives in PMU
> alive-domain registers that a hard reset and a kernel panic do NOT clear
> (only a clean "Reboot system now" or a full power-off does). So the
> panic canary would loop. See `RECOVERY-PERSISTENCE-ROOTCAUSE.md`. Do not
> run Stage 0/1 until a recovery-slot image clears the PMU reboot state as
> its first action (the `oss-rebootmode-clear` kernel piece). The escape
> below is updated for the real persistence.


Purpose: prove that a boot which **panics** (PID-1 death, the exact stage-2
v2/v3 failure) started in the **recovery** partition returns to the working
**BOOT on its own**, with no hands. If it does, the recovery partition is a
brick-proof one-shot test slot for stage 2. BOOT (`8c0461ee`) is never written.

## Why the entry path matters (correction from the round-trip test)
`adb reboot recovery` is **not** a safe one-shot: AOSP init writes
`boot-recovery` into the misc BCB (`system/core/init/reboot.cpp`), which sticks
until recovery clears it. The July 2026 history shows a failed custom recovery
soft-bricked the phone this way until the battery died. So we enter recovery
**without touching the BCB**, using a raw `reboot(2)` syscall with arg
`"recovery"` (the `reboot-recovery` tool). The Samsung kernel restart handlers
(`exynos-reboot`, `sec_reboot`) record the request only in a one-shot PMU reason
register; a later panic/watchdog/manual reset sets a different reason, so the
bootloader then boots BOOT. We assert the BCB is all-zero before and after.

## Artifacts (all built unflashed; in `images/oss-stage2/canary/`)
| File | sha256 | role |
| --- | --- | --- |
| `recovery-canary-A266MUBU1AYB8.img` | `25540aa1…f89a96ef` | stock recovery kernel/dtb/dtbo + tiny ramdisk whose `/init` writes one kmsg line then exits → PID-1 panic. Exactly 100663296 B, AVB footer, header v2. |
| `reboot-recovery` | `89c036e0…a823c40e` | static aarch64 tool: raw `reboot(2)` "recovery", BCB-free entry |
| `misc-bcb-clear.img` | `42ba364c…4ab1ddac` | 1048576 B; today's misc with the first 4 KiB (BCB) zeroed, vendor region preserved — the escape from a stuck BCB |
| stock recovery (rescue) | `9ab6cefe…a91a7e8` | `~/Desktop/a26-rescue/A266MUBU1AYB8-current/recovery.img`, the restore target |

Scripts: `scripts/direct-boot/flash-recovery-inplace.sh` (write/restore RECOVERY
with backup + read-back verify; `--check-bcb`, `--restore-stock`, `--dry-run`).

## GPU mode (2026-10-26)
GPU mode (Moon drawing through `moon-compositor`) changes nothing here: the
canary and the OSS-kernel calibration boot from the RECOVERY slot with their own
ramdisk, and `flash-recovery-inplace.sh` only reads/writes block devices. Nothing
in this path assumes Xorg owns KMS. Only the post-return expectation is affected:
the first boot back may be plain Android by policy (recovery reason / panic).

## Preconditions
- Attended, phone on USB, battery ≥ 50 %.
- `adb shell su -c 'sha256sum /dev/block/by-name/boot'` == `8c0461ee…` (BOOT good).
- `scripts/direct-boot/flash-recovery-inplace.sh --check-bcb` → "all zero".
- `adb shell su -c 'sha256sum /dev/block/by-name/recovery'` == `9ab6cefe…` (stock).
- Download-mode rescue kit ready (samloader, stock recovery, `misc-bcb-clear.img`).

## STAGE 0 — zero-write mechanism check (no partition writes at all)
Proves recovery entry + "a reset during recovery returns to BOOT", using the
**stock** recovery already installed (cannot loop — stock recovery always boots).
1. `--check-bcb` → all zero.
2. Push and hash-check the BCB-free entry tool, but **do not run it yet**:
   `adb push images/oss-stage2/canary/reboot-recovery /data/local/tmp/ && adb shell su -c 'chmod 0755 /data/local/tmp/reboot-recovery'`,
   verify `adb shell sha256sum /data/local/tmp/reboot-recovery` == `89c036e0…`.
   It is run in step 4 with `adb shell su -c /data/local/tmp/reboot-recovery`
   (writes no partition; it only calls `reboot(2)`).
3. **Before the operator runs the tool, the user must be holding the phone
   and have read step 4.** The forced restart happens *during the boot into
   recovery*, NOT at the recovery menu.
   Why: in stock recovery, **Power is the "select" key and acts on press**.
   Starting a Power + Volume Down hold at the recovery menu registers Power
   first and selects the highlighted item, which is **"Reboot system now"** by
   default. That is almost certainly what happened on 2026-10-26, and it skips
   the test. Before the recovery UI is up (the first ~25 s), keys do nothing in
   recovery, and a reset then also matches when the canary panics (early in the
   recovery boot).
4. **Read this to the user word for word, then run the tool:**

   > ⚠️ **DO NOT WAIT FOR THE RECOVERY MENU. DO NOT SELECT ANYTHING.**
   > If you see the "Android Recovery" menu or the robot, you were too late:
   > do NOT press Power; just tell us. We will start this step again.
   >
   > 1. Hold the phone, screen facing you. When I say "go", the screen goes
   >    black and the phone restarts.
   > 2. Watch for the **yellow/orange warning screen** ("The bootloader is
   >    unlocked…"), which appears a few seconds after the screen goes black.
   > 3. **As soon as that warning appears**, press and **hold Power and Volume
   >    Down together**. Do not let go.
   > 4. Keep holding for **about 10 seconds**, until the screen **goes black**
   >    again.
   > 5. **Let go of both keys the moment it goes black.** Don't press any
   >    other key (Volume Up + Volume Down now would enter Download mode).
   > 6. Put the phone down and wait about a minute without touching it.

   Operator timing (2026-10-26 run): `reboot-recovery` → screen black at once
   → recovery's adbd as `unauthorized` at about 30–35 s. The hold therefore
   starts at about 3–8 s and the reset lands at about 13–18 s, well before the
   recovery UI.
   If the user is unsure what happened, ask what the screen shows after the
   minute. Do not guess.
5. Expected: the phone boots the **normal system** from BOOT, NOT recovery.
   It may come up as **plain Android rather than Moon**: `moon-direct-policy.sh`
   sends a boot whose reason contains `recovery` to Android once (line 48).
   Pass = BOOT booted (adb authorized, `/proc/1/comm=init`, BOOT sha), not Moon.
6. Back in the system: `--check-bcb` → still all zero; BOOT and RECOVERY hashes
   unchanged.
→ If step 5 lands in the system, the PMU-reason entry is one-shot even across a
reset during the recovery boot, with no BCB involved: proceed to Stage 1.
→ If it lands in **Android Recovery again**, the reason survived the reset:
**stop, the canary is unsafe on this bootloader.** Recovery is still stock, so
the way out is the normal menu: Volume keys to highlight **Reboot system now**,
**Power** to select. (Here, and only here, selecting it is correct.)
→ If the user saw the recovery menu before holding the keys (too late), nothing
was tested. Exit with **Reboot system now**, check the BCB, and redo Stage 0.

## STAGE 1 — the panic canary
1. `--check-bcb` → all zero (re-assert).
2. **Backup + today's dry run (optional, writes only a backup file):**
   `scripts/direct-boot/flash-recovery-inplace.sh --dry-run images/oss-stage2/canary/recovery-canary-A266MUBU1AYB8.img`
3. **Flash the canary to RECOVERY** (BOOT untouched; backs up stock recovery to
   phone + Mac, writes, reads back and verifies sha):
   `scripts/direct-boot/flash-recovery-inplace.sh images/oss-stage2/canary/recovery-canary-A266MUBU1AYB8.img`
4. **Enter recovery BCB-free:** `adb shell su -c /data/local/tmp/reboot-recovery` (pushed and hash-checked in Stage 0).
5. Expected within ~10–60 s, hands-off:
   - the bootloader's unlocked-orange warning, briefly,
   - a kernel panic ~1 s into the recovery boot (the canary exits as PID 1),
   - a reset, then the phone boots **BOOT** on its own.
6. **That first boot is plain Android, not Moon** — the boot policy gives one
   Android boot after a kernel panic. That is fine.
7. **First checks after return (in order):**
   - `scripts/direct-boot/flash-recovery-inplace.sh --check-bcb` → **all zero**
     (proves the canary/entry left no sticky BCB).
   - `adb shell su -c 'sha256sum /dev/block/by-name/boot'` == `8c0461ee…`.
   - `adb shell su -c 'cat /proc/last_kmsg 2>/dev/null | grep OSS-CANARY'` or
     `… /sys/fs/pstore/console-ramoops*` → the `OSS-CANARY:` marker (proves the
     canary ran in recovery and the reset came from its panic).
8. **Restore stock recovery and get Moon back:**
   `scripts/direct-boot/flash-recovery-inplace.sh --restore-stock`
   then `adb reboot` → Moon. Confirm `sha256(recovery)` == `9ab6cefe…`.

→ Pass: steps 5–7 return to BOOT by themselves with BCB still zero. The recovery
partition is then a brick-proof one-shot slot; the recovery-based stage-2 image
can be tried the same way (it boots, or it panics and returns to BOOT).
→ Fail (loops in recovery / no return): Escape, below. The canary did its job by
telling us the recovery slot is NOT safe on this bootloader; do not proceed.

## ESCAPE (physical; every path ends at the untouched BOOT)
If after a canary reset the phone keeps entering recovery (sticky BCB or the
bootloader re-latches recovery):
1. **Force restart:** hold **Power + Volume Down** ~7–10 s. A manual reset
   replaces the reboot reason; it should boot BOOT. If it boots the system, go
   to step 5 (clean up).
2. If it still enters recovery, **enter Download mode:** with the USB cable
   attached, at the black screen hold **Volume Up + Volume Down**; on the
   warning screen press **Volume Up**.
3. **Clear the BCB and restore stock recovery from Download mode** (misc is PIT
   entry #27, RECOVERY is #22, both flashable):
   ```
   cd ~/Desktop/a26-rescue
   tools/samloader flash -p MISC     misc-clean/misc-bcb-clear.img \
                         -p RECOVERY A266MUBU1AYB8-current/recovery.img
   ```
   (`-p` is repeatable in one session; samloader-rs collects all pairs.) Then
   hold **Power + Volume Down** ~7–10 s to leave Download mode.
4. The phone boots BOOT (`8c0461ee`) — stock recovery restored, BCB cleared.
5. **Clean up** from the booted system: `--check-bcb` → all zero;
   `sha256(recovery)` == `9ab6cefe…`; `rm /data/local/tmp/reboot-recovery`.

### Escape from a stuck recovery selection (updated 2026-10-26)
A plain forced restart does **not** help: the PMU recovery selection survives a
hard reset (that is exactly what Stage 0 showed). The deterministic escape,
BOOT never touched:
1. Enter **Download mode** — VolUp+VolDown with USB at the warning screen. This
   is a bootloader key combo, independent of the PMU reboot-cmd, so it always
   works. The armed watcher flashes **stock recovery** (`9ab6cefe`) + MISC-clear
   to the recovery slot.
2. Restart (Power+VolDown). The PMU still says recovery, so the bootloader boots
   the slot — now **stock recovery**.
3. In stock recovery: Volume keys to **"Reboot system now"**, then Power. Its
   clean reboot rewrites the PMU to NORMAL and the phone boots **BOOT**.
A full power-off also clears it (the alive domain loses power), but step 1-3 is
faster and does not risk the battery.


## Invariants asserted every run
- misc BCB (first 4 KiB) == all zero, **before entry and first after return**.
- BOOT sha256 == `8c0461ee…` throughout (never written).
- RECOVERY ends as either the canary (during the test) or stock `9ab6cefe…`
  (after `--restore-stock`); every write is read-back-verified.
