# On-device run, 2026-10-26 23:18–23:37 (canary, recovery-slot Moon, stage 2)

Attended; BOOT `8c0461ee` never written; misc BCB all zero before and after every
stage; every RECOVERY write read-back verified; stock recovery `9ab6cefe`
restored at the end. Logs: `~/Desktop/a26-rescue/{canary,slotmoon,stage2v4}-run-20261026/`
(`/proc/sec_log` keeps the bootloader log and the early kernel log that the 1 MiB
ring buffer loses within ~50 s).

## How recovery is selected (now measured)
- `reboot(2)` "recovery" → the kernel sets panic-inform `0x80c = 0x12345674` and
  reboot-cmd `0x810` mode 1.
- LK: `sbl_check_reboot_mode: PINFORM = 12345674 ... setting REBOOT_MODE to 4` →
  `platform_init: Recovery Set!`; LK then **clears inform3 (0x80c) to 0** itself.
- During the recovery boot 0x810 reads **`0x9`** (bootloader flag bit 3 + **mode 1**);
  LK leaves that mode latched, which is why a hard reset during recovery
  re-entered recovery (Stage 0).
- After `oss-rebootmode-clear.ko`: 0x810 `0x8` (mode 0), panic-inform `0x12345670`;
  next LK: `PINFORM = 12345670 ... setting REBOOT_MODE to 0` → BOOT.

## Stage 1 — canary (`71d68ced`, require_mode=1): PASS (hands-free return)
LK #19 booted the slot (`Recovery Set!`); the canary boot ended in a **CL0
watchdog reset** (`rst_stat 0x1000000 / CL0_WDTRESET`, LK's 60 s watchdog);
LK #20 read `PINFORM = 12345670` (the module's write) → `REBOOT_MODE 0` → BOOT.
Back in BOOT 115 s after the fire, no hands. Stock recovery never ran (no
`/cache/recovery` writes), so the canary did not take its no-panic branch. The
canary's own kernel log is not recoverable (no sec_debug that early); the panic
evidently did not restart the SoC by itself (no restart handler loaded that
early), the hardware watchdog did. **Implication: an early failure in a slot
boot returns to BOOT within ~60 s via LK's watchdog.**

## Stage 2 — recovery-slot-moon (`cc406f02`): PASS
Moon on screen from the recovery slot 35 s after the fire; marker
`/debug_ramdisk/oss-recovery-slot` present. sec_log:
```
oss-rebootmode-clear: before: reboot-cmd[0x810]=0x9 (mode 1) magic-inform[0x808]=0x4e4e0000 panic-inform[0x80c]=0x25b7ce71
oss-rebootmode-clear: after:  reboot-cmd[0x810]=0x8 (mode 0) magic-inform[0x808]=0x4e4e0000 panic-inform[0x80c]=0x12345670
oss-rebootmode-clear: OK: NORMAL boot selected (cleared a reboot mode)
oss-pid1: PMU reboot state set to NORMAL; handing PID 1 to magiskinit as /init
```
(The boot reason seen by Android was `reboot`, so the policy exception was not
even needed this time; it is installed on /data and harmless.)

## Stage 3 — recovery-slot-stage2-v4 (`f0b7263a`): containment WORKS; Moon does NOT
- **oss-init is the real PID 1 and Android runs contained.** sec_log: the module
  `OK:`, `oss-init: OSS-Phone init is the real PID 1 (v4…)`, `PMU reboot state set
  to NORMAL`; Android's init logs as **host PID 198** (7 099 lines) while being PID 1
  in its namespace; `OSSPHONE_CONTAIN_LIVE` at 6.9 s from `init: 198`. ueventd,
  watchdogd, adbd, Magisk (`su` → `u:r:magisk:s0`) all run inside the container;
  adb works (it runs inside, so its `/proc/1` is Android's init).
- **Defect 1 — SurfaceFlinger crash loop → no Moon.** SF aborts every ~5 s:
  `no suitable EGLConfig found … vendor: Android, version: 1.4 Android META-EGL`.
  Kernel: `mali 10300000.mali: Failed to get pid pointer for surfaceflinger/N` /
  `Common context initialization failed error = -3`. Cause in Samsung's Mali
  source (`drivers/gpu/arm/bv_r32p1/context/mali_kbase_context.c:142-170`):
  `kctx->tgid = current->tgid` (global) then `find_get_pid(kctx->tgid)`, which
  looks the number up in the **caller's** PID namespace → ESRCH in a container.
  Moon's supervisor then logged `SurfaceFlinger/display bootstrap timed out`
  (103 s) and fell back to the Android framework (inside the container).
  Fix (kernel agent): `pid_struct = get_task_pid(current, PIDTYPE_TGID);` (or
  `find_pid_ns(kctx->tgid, &init_pid_ns)`), rebuild `mali_kbase.ko`, and ship
  it **only in the recovery-slot image's vendor ramdisk** (vendor_boot untouched).
- **Defect 2 — oss-init's liveness reader is blocked by SELinux.** After the
  container loads the enforcing policy, every read of the `/dev/kmsg` node oss-init
  created on rootfs is denied: `avc: denied { read } … scontext=u:r:kernel:s0
  tcontext=u:object_r:rootfs:s0 tclass=chr_file` (SELinux revalidates open fds
  after a policy load; `kernel` has only `write` on `rootfs chr_file`). Its own
  log lines after that point are lost too (re-opening /dev/kmsg is denied).
  It therefore never saw `OSSPHONE_CONTAIN_LIVE`.
- **Defect 3 — the hang path's `kill` did not stop the container.** At the 60 s
  deadline oss-init neither rebooted nor stopped the container (which kept
  running for minutes), so its `kill(SIGKILL)` evidently failed (SELinux:
  `kernel` → `init` process) and it then blocked in `waitpid(child, 0)`. Still
  safe: when the user's `adb reboot` ended the container, `waitpid` returned and
  the fallback rebooted to BOOT (`RPON`, `PINFORM 12345670 → REBOOT_MODE 0`).

## Fixes for oss-init (next build)
1. Liveness: treat the first `EACCES` from the kmsg reader as "the container
   loaded SELinux policy" (= reached second stage) — a true signal that needs
   no new permission. Keep a write fd to /dev/kmsg opened at start (writes on an
   open fd need only `write`, which `kernel` has).
2. Hang path: if `kill` fails, do not `waitpid` blocking; go straight to the
   controlled reboot. After the clear every reset lands on BOOT (measured in all
   three stages), so a reboot is the safe universal fallback once the clear has
   succeeded.
3. Ship the Mali fix in the slot image (kernel agent), then re-run Stage 3.
