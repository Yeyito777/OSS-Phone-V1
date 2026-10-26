# Panic-canary recovery runbook (attended, morning)

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
2. Push and run the BCB-free entry tool:
   `adb push images/oss-stage2/canary/reboot-recovery /data/local/tmp/ && adb shell su -c 'chmod 0755 /data/local/tmp/reboot-recovery'`,
   verify `adb shell sha256sum /data/local/tmp/reboot-recovery` == `89c036e0…`, then
   `adb shell su -c /data/local/tmp/reboot-recovery`
   (this writes no partition; it just calls `reboot(2)`).
3. Phone enters **stock recovery** (Android Recovery screen).
4. **Force-restart while in recovery:** hold **Power + Volume Down** ~7–10 s.
5. Expected: the phone boots the **normal system** from BOOT, NOT recovery.
   It may come up as **plain Android rather than Moon**: `moon-direct-policy.sh`
   sends a boot whose reason contains `recovery` to Android once (line 48).
   Pass = BOOT booted (adb authorized, `/proc/1/comm=init`, BOOT sha), not Moon.
6. Back in the system: `--check-bcb` → still all zero; BOOT and RECOVERY hashes
   unchanged.
→ If step 5 lands in the system, the PMU-reason entry is one-shot even across a
mid-recovery reset, with no BCB involved. If it re-enters recovery, **stop** —
the canary is unsafe; go to Escape.

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

### How likely is a plain forced restart (step 1) to reach BOOT?
Good, and this is the normal case. Recovery here is selected by the one-shot PMU
reason, which the recovery boot consumes; a manual Power+VolDown reset writes a
*manual* reason, so the bootloader boots BOOT. The only way step 1 fails is if a
sticky **BCB** `boot-recovery` is present — which the canary path never writes
(it enters via `reboot(2)`, not init), and which we assert is zero before entry.
The Download-mode MISC flash (step 3) is the deterministic backstop if it is
ever set. MISC and RECOVERY are both in the PIT, so this is a one-session fix,
not a wait-for-the-battery-to-die situation like the July 2026 incident.

## Invariants asserted every run
- misc BCB (first 4 KiB) == all zero, **before entry and first after return**.
- BOOT sha256 == `8c0461ee…` throughout (never written).
- RECOVERY ends as either the canary (during the test) or stock `9ab6cefe…`
  (after `--restore-stock`); every write is read-back-verified.
