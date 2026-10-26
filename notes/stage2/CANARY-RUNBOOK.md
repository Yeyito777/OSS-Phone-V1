# Recovery-slot test runbook: canary v2, then Moon and stage 2 from the slot

> ⏸ **ON HOLD until the new `oss-rebootmode-clear.ko` exists, passes on the
> device, and the images are rebuilt with its sha256 pin.** The new module
> (requested 2026-10-26): clears only the mode bits 2:0 of reboot-cmd 0x810
> (the bootloader owns the other bits; a normal boot reads 0x48), reads every
> register back and fails the load unless they select NORMAL, and with
> `require_mode=1` fails unless this load actually found and cleared a
> recovery-style mode. The canary loads it with `require_mode=1` and treats
> "already loaded" as failure, so it panics only on concrete evidence that it
> cleared the real selector; in every other case it does not panic and boots
> stock recovery, and its log is the measurement. oss-init and oss-pid1 load
> with default parameters. The current images (pinned to the old module
> `a18606a8`) must not be used for the canary.

## Why it is built this way (history in one paragraph)
`adb reboot recovery` writes a sticky `boot-recovery` BCB, so recovery is
entered with the BCB-free `reboot-recovery` tool (raw `reboot(2)` "recovery").
That still leaves a **recovery selection in PMU alive-domain registers**
(exynos-reboot reboot-cmd `0x810`, sec_reboot magic-inform `0x808` and
panic-inform `0x80c`), which survive a hard reset **and a kernel panic**:
Stage 0 on 2026-10-26 re-entered recovery after a forced restart
(`RECOVERY-PERSISTENCE-ROOTCAUSE.md`). So every recovery-slot `/init` now loads
`oss-rebootmode-clear.ko` **as its first action**, setting those registers to a
NORMAL boot; any later failure then lands on BOOT. If the module does not
load, nothing panics or reboots on purpose (that could loop); the image falls
back to a stock init instead. Stage 0 is retired: its question is answered.

## Artifacts (built unflashed, deterministic, whitelisted in `flash-recovery-inplace.sh`)
| File | sha256 | What |
| --- | --- | --- |
| `canary/recovery-canary-A266MUBU1AYB8.img` | `0e4e70b2…f0c06bbe` | Stock recovery (kernel, dtb, dtbo, full ramdisk) with `/init` = canary v2 and `/oss-rebootmode-clear.ko` |
| `canary/reboot-recovery` | `89c036e0…a823c40e` | BCB-free recovery entry |
| `recovery-slot/recovery-slot-moon-A266MUBU1AYB8.img` | `583ad993…3e25e9c` | Today's boot from the slot: `/init` = oss-pid1 (clear → magiskinit as `/init` → Moon) + recovery-slot marker |
| `recovery-slot/recovery-slot-stage2-v4-A266MUBU1AYB8.img` | `84768b63…f4108c0` | Stage 2: `/init` = oss-init v4 (clear → Android in a PID+mount namespace) |
| `oss-rebootmode-clear.ko` (inside each image) | `a18606a8…cba4054` | kernel agent's module, `src/oss-rebootmode-clear` |
| `a26-rescue/misc-clean/misc-bcb-clear.img` | `42ba364c…4ab1ddac` | escape: MISC with an empty BCB |
| `a26-rescue/A266MUBU1AYB8-current/recovery.img` | `9ab6cefe…a91a7e8` | stock recovery (restore target) |

## Preconditions (every stage)
- Attended, USB to the Mac, battery ≥ 50 %, the Download-mode escape watcher armed.
- `scripts/direct-boot/flash-recovery-inplace.sh --check-bcb` → all zero.
- BOOT sha256 `8c0461ee…`, RECOVERY `9ab6cefe…` (stock) before a stage starts.
- `reboot-recovery` on the phone with sha256 `89c036e0…`
  (`adb push`, `chmod 0755`, `sha256sum`).

## Stage 1 — canary v2 (expected: no hands at all)
1. `--check-bcb` → all zero.
2. `scripts/direct-boot/flash-recovery-inplace.sh images/oss-stage2/canary/recovery-canary-A266MUBU1AYB8.img`
   (backs up stock recovery to phone + Mac, writes, reads back, verifies).
3. `adb shell su -c /data/local/tmp/reboot-recovery`.
4. Expected, hands-free: warning screen → recovery kernel → the canary loads
   the module and exits PID 1 → kernel panic → reset → **BOOT**. The first boot
   back is **plain Android** (the policy gives one Android boot after a panic).
5. Checks, in this order:
   - `--check-bcb` → all zero.
   - BOOT `8c0461ee…`.
   - `adb shell su -c 'cat /proc/last_kmsg' | grep -aE 'oss-rebootmode-clear|OSS-CANARY|Attempted to kill init'`
     The `before:` line is the measurement (normal boot: 0x48 = mode 0 + flag
     bits 6 and 3, owned by the bootloader). With `require_mode=1`:
     | `before:` reboot-cmd | Meaning | Module | Canary | Expected |
     | --- | --- | --- | --- | --- |
     | `0x1` | the kernel's recovery write is intact | clears mode bits → `after:` `0`; verified | **panics** | BOOT, hands-free |
     | `0x49` | bootloader flags + mode 1 | clears mode bits → `after:` `0x48`; verified | **panics** | BOOT, hands-free |
     | `0x48` | no mode in 0x810: the bootloader consumed it, or the selector is elsewhere | no mode found → load **fails** | **no panic**; stock recovery | leave via "Reboot system now"; then the selector must be found elsewhere before any panic test |
     | other / read-back mismatch | unknown layout | load **fails** | **no panic**; stock recovery | report the value |
     When it panics, the log also shows `magic-inform` bits 23:16 → `0x4e` and `panic-inform` `0x12345674` →
     `0x12345670`, then
     ```
     OSS-CANARY: PMU reboot state cleared; exiting PID 1 now to force a panic/reset (expected: return to BOOT)
     Kernel panic - not syncing: Attempted to kill init!
     ```
6. `flash-recovery-inplace.sh --restore-stock` (→ `9ab6cefe`), then `adb reboot` → Moon.

Outcomes:
- **Back in BOOT by itself** → PASS: the recovery slot is a hands-free
  one-shot. Go on to Stage 2.
- **Stock recovery UI shows** → the module did not load; the canary did not
  panic (by design) and booted stock recovery. Leave with Volume keys →
  **"Reboot system now"** → Power. Read why: `/proc/last_kmsg` has
  `OSS-CANARY: oss-rebootmode-clear did not load …; errno=N`. Stop.
- **Repeated warning screens / black screen, no adb within ~3 min** → the
  clear did not help (the bootloader uses something else). ESCAPE below. Stop.

## Stage 2 — recovery-slot-moon (Moon from the recovery slot)
0. Once: install the policy with the recovery-slot exception,
   `scripts/device/moon-direct-policy.sh` → `/data/adb/moon/moon-direct-policy.sh`
   (as `install-autostart.sh` does). Without it a recovery-reason boot is plain Android.
1. `--check-bcb`; flash `recovery-slot-moon-A266MUBU1AYB8.img`; `reboot-recovery`.
2. Expected: Moon on screen (from the recovery slot). Verify:
   `dmesg | grep -E 'oss-rebootmode-clear|oss-pid1'` shows `before:` (reboot-cmd 0x1)
   and `after:` lines, then `oss-pid1: PMU reboot state set to NORMAL; handing PID 1 to magiskinit`;
   `ls /debug_ramdisk/oss-recovery-slot` exists; `getprop moon.session.ready` = 1.
3. `--restore-stock`, `adb reboot` → normal BOOT.
A failure here (no Moon, panic) returns to BOOT by itself; an exec fallback
shows `oss-pid1: … exec stock init` in the log.

## Stage 3 — recovery-slot-stage2-v4 (oss-init containment)
1. `--check-bcb`; flash `recovery-slot-stage2-v4-A266MUBU1AYB8.img`; `reboot-recovery`.
2. adb runs **inside** the container, so `/proc/1` there is Android's init.
   PASS = `dmesg | grep -E 'oss-rebootmode-clear|oss-init'` shows, in order:
   `before:`/`after:`, `oss-init: OSS-Phone init is the real PID 1 (v4…)`,
   `oss-init: PMU reboot state set to NORMAL`, then
   `oss-init: container is live (Android second stage reached); oss-init supervising as the real PID 1`,
   with no `failed`/`fallback` line, and Moon is up.
3. `--restore-stock`, `adb reboot`.
By design: early failure → stock init in the same boot; late failure → a
reboot to BOOT; either is logged.

## ESCAPE (physical; BOOT is never written)
If the phone keeps coming back to a recovery-slot image:
1. **Download mode:** with USB attached, at the black screen or the warning
   screen, hold **Volume Up + Volume Down**; on the warning press **Volume Up**.
   This is a bootloader key combo, independent of the PMU registers.
2. The armed watcher runs, in one session:
   `tools/samloader flash -p MISC misc-clean/misc-bcb-clear.img -p RECOVERY A266MUBU1AYB8-current/recovery.img`
   (MISC = PIT #27, RECOVERY = PIT #22).
3. Leave Download mode: hold **Power + Volume Down** ~7–10 s.
4. If the PMU still selects recovery, **stock recovery** now comes up: Volume
   keys → **"Reboot system now"** → Power. Its clean reboot rewrites the PMU to
   NORMAL and the phone boots **BOOT**. (At the recovery menu Power selects:
   only use it on "Reboot system now".)
5. Clean up from the booted system: `--check-bcb` all zero; RECOVERY `9ab6cefe`.
A full power-off also clears the PMU state (the alive domain loses power).

## Invariants asserted every run
- misc BCB (first 4 KiB) all zero, **before entry and first after return**.
- BOOT sha256 `8c0461ee…` throughout (never written).
- RECOVERY is either the image under test or stock `9ab6cefe…`; every write is
  read-back-verified; only whitelisted (clear-first) images are accepted.
