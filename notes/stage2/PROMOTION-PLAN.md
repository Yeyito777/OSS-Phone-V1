# Promotion plan: putting oss-init into BOOT, safely

Stage 2 works end-to-end from the recovery slot (`ONDEVICE-RESULTS-20261027-V5.md`):
oss-init is the real PID 1, Android runs contained, Moon comes up. This plan is
the path from "proven in the one-shot recovery slot" to "oss-init is the PID 1 of
the real BOOT partition", on a phone with **no A/B slot** and a hard rule: nothing
that changes the boot chain goes to BOOT until a failed boot provably recovers on
its own.

Status: v6 (classification + boot-count consumer) and the clear hook are built
unflashed. The counter STORE (a kernel module) and the gated flash path are
specified here for the kernel agent and for an explicit, attended promotion; they
are **not** built or enabled yet. The recovery-slot soak (below) runs first.

---

## 1. oss-init v6 — reboot vs power-off vs failure (built)

The v5 run mislabelled a contained `adb reboot` as a failure. Root cause, from
the kernel: a reboot/power-off requested inside a PID namespace goes through
`reboot_pid_ns()` + `zap_pid_ns_processes()` (kernel/pid_namespace.c), which sets
`pid_ns->reboot = SIGHUP` for RESTART/RESTART2 and `SIGINT` for POWER_OFF/HALT and
tears the namespace down. oss-init sees it on the container PID 1's wait status.

v6 classifies the container's exit:
- **WIFSIGNALED/WIFEXITED == SIGHUP (1)** → contained **reboot** → `real_reboot(0)`.
  (Measured on the bench: PID 1 calling `reboot(RESTART)` surfaces as status
  `0x1`; POWER_OFF as `0x2`. Both signalled and exited encodings are accepted.)
- **== SIGINT (2)** → contained **power-off** → `real_reboot(1)` (actually powers off).
- **SIGKILL / any other signal / other exit code** → a real failure → the
  existing `fall_back`: `reboot_safe` (sec_reboot seen) → controlled reboot to
  BOOT; else exec the stock init.

Bench cases `contained-reboot`, `contained-poweroff`, `late-reboot` (a genuine
exit 42 failure) all pass.

**Known residual (documented, fixed in the channel below).** If a *non-PID-1*
process in the container calls `reboot(2)` (e.g. a Samsung `vendor_init`
subcontext — the likely cause of the v5 device observation), the kernel SIGKILLs
the container's PID 1 and the reboot-vs-power-off distinction is **lost** (the
status is only SIGKILL). v6 then treats it as a failure and reboots to BOOT
(safe, but a contained *power-off* would reboot instead of powering off). The
complete fix is a reason channel:

> **Reason channel (design, not yet built).** Before the clone, oss-init creates
> a small shared tmpfs (e.g. `/run/oss`) and bind-mounts it into the container's
> private rootfs so both namespaces see it. A one-line Android init hook shipped
> in the image's `overlay.d` (`on property:sys.powerctl=*` → `write /run/oss/powerctl ${sys.powerctl}`)
> records the intent ("reboot[,reason]" / "shutdown[,reason]") the instant it is
> set, before any process calls `reboot(2)`. On container exit oss-init reads
> `/run/oss/powerctl`: authoritative reboot-vs-power-off AND the reason string
> (recovery / bootloader / normal), independent of which process issued the
> syscall. This also lets a contained `reboot bootloader`/`recovery` be honoured
> deliberately (or refused) instead of always landing on BOOT. Deferred because
> it touches the container rootfs setup and ships an rc file — worth doing
> carefully, not needed for the first BOOT promotion (which only needs
> reboot→BOOT, already safe).

---

## 2. BOOT-promotion safety: a boot-attempt counter (built: oss-init v7)

The one property the recovery slot gave us for free — a failed boot auto-returns
to the known-good BOOT — does not exist on the single BOOT partition. v7
reconstructs an A/B-like "fall back to the last-good boot after N failures":

1. On every boot oss-init supervises, it **bumps a persistent counter**.
2. If the new count is **>= 3**, oss-init **skips containment** and boots
   **magiskinit as `/init`** — today's proven boot (Magisk + Moon), the path
   oss-pid1 proved on the device — with the stock init via the held fd only if
   that exec fails. The phone always reaches a working system.
3. A **healthy contained session clears** the counter
   (`scripts/device/moon-boot-confirm.sh`, after a 60 s soak window), so the
   count is *consecutive* failures. A skipped boot has no
   `/debug_ramdisk/oss-contained` marker and does **not** clear: a skipped
   phone stays on the proven boot until someone clears the count on purpose
   (`oss-init --bootcount clear`).
4. The in-boot fallbacks still apply (early failure → stock init; late failure →
   controlled reboot to BOOT; PMU-clear failure → stock init).

### Store: the misc partition (kernel agent, record format v1)
No PMU register could be proven unused (bootloader, EL3 and firmware may touch
them), so the counter lives on flash: `docs/bootcount-misc-record.md`. Two 4 KiB
A/B slots at misc `0x40000`/`0x41000` (confirmed all-zero on the live phone;
the only non-zero misc blocks are `0x8000` and `0x7f000`), a 32-byte CRC'd
record, newest valid `seq` wins. **Persists across cold power-off** (safer: a
user power-cycling a boot loop still escalates). Restoring the whole-partition
`misc-bcb-clear.img` zeroes it (a reset; on a promoted image it escalates, see
below).

Hard write rules, enforced in `bootcount-misc.h` (the only writer): only full
4 KiB blocks at exactly `0x40000` or `0x41000`; never the BCB or any block that
is neither zero nor ours; `O_DSYNC` + `fsync`, then read back (O_DIRECT on the
block device) and compare. Our own torn/damaged record (magic, zeros after byte
32, bad CRC) is overwritable; anything else non-zero is foreign → write nothing.

### Timing (why the bump is not at the first action)
At oss-init's first action **there is no misc device**: UFS is a vendor module
(`ufs-exynos-core.ko`, 29 symbol dependencies plus regulator/PHY providers),
loaded by the container's stock first stage. v5 log: oss-init 0.72 s, UFS module
1.67 s, partition scan 1.94 s, switch root 4.69 s. So:
- oss-init clones the container as before, plus a **helper** in its own private
  mount namespace (oss-init's own namespace stays mount-free). The helper mounts
  sysfs, polls for `PARTNAME=misc` (exactly one entry, exactly 1 MiB), and
  **freezes the container** with a pidfd `SIGSTOP`.
- It `mknod`s the node (from sysfs `MAJOR`/`MINOR`, never hard-coded) in a
  private tmpfs, bumps, and reports through a pipe.
- oss-init thaws the container (`SIGCONT`) or kills it and takes the skip path.

The freeze lands during module loading, before any dm device or mount: the same
state as an early container failure. A container that is already live when the
report arrives is never killed. If misc never appears within 15 s, both modes
contain as usual: without UFS no boot can mount anything.
**Residual:** a failure in the ~1.2 s before misc appears is not counted. That
window holds oss-init's own clear + clone (proven on device three times) and
stock first-stage module loading (same kernel and modules as today's boot).
Container failures there already fall back to the stock init in the same boot
(no reboot, so no loop).

### Normal vs promoted
| Store state at the decision | slot build (`OSS_PROMOTED=0`) | promoted BOOT build (`-DOSS_PROMOTED=1`) |
| --- | --- | --- |
| valid record, new count < 3 | contain | contain |
| valid record, new count >= 3 | skip | skip |
| absent / only damaged | create count 1, contain | **skip** (no write) |
| foreign slot | no write, contain | **skip** |
| I/O or verify error, wrong size, ambiguous | contain | **skip** |
| misc not found in 15 s | contain | contain |
A promoted image therefore needs a valid record before its first boot:
`oss-init --bootcount init` is part of the promotion procedure (§4).

### Verified (off-device)
`test/run-bootcount-tests.sh`, 131 checks, all pass:
- **file:** oss-init as PID 1 with a file-backed misc and a fake sysfs. Cases:
  fresh, second, limit, capped, foreign, torn, damaged, late misc, limit after
  live, no sysfs, wrong size, ambiguous, and promoted absent/valid/foreign/
  damaged/no-sysfs.
- **cli:** including seq wrap-around and foreign refusal.
- **device:** a real block device (GPT partition named `misc`, `losetup -P`).
  The CLI and the production helper path ran as root in a private namespace:
  real sysfs `PARTNAME=misc`, real `mknod`, O_DIRECT, freeze, skip.

Every case checks that no byte outside `0x40000`–`0x41fff` changed; the images
carry a non-zero BCB, `0x8000` and `0x7f000`. The shipped aarch64 binary's CLI
(qemu) writes records the zlib reference accepts.

## 3. Soak test (recovery slot first, before any BOOT write)

Runbook: `notes/stage2/SOAK-RUNBOOK.md` (v7 slot image `0381b5ab`). Repeated boots,
screen on/off, audio, Wi-Fi, telephony, notifications and alarms, long uptime,
contained reboot / power-off / crash handback, and the counter (bump, clear,
escalation at 3, foreign-slot interlock) — all from the recovery slot, where a
failure still auto-recovers. Promotion is gated on every item passing.

## 4. Exact `flash-boot-inplace.sh` changes to allow an oss-init BOOT image

Today the script refuses, with no override, any BOOT image whose ramdisk `/init`
differs from the installed one (that rule caught the v2/v3 bricks). Promotion
adds a **separate, heavily gated** path that does not weaken the default refusal.
Precise changes:

1. **New flag `--promote-oss-init`** (distinct from `--allow-kernel-change`; the
   removed `--allow-init-change` stays removed). Without it, the `/init differs`
   refusal is byte-for-byte unchanged.

2. **A dedicated allowlist `declare -A OSS_INIT_BOOT=( [sha256]='desc' )`** of
   BOOT images whose `/init` is oss-init. The new path runs ONLY for an image
   whose sha256 is in it; anything else still hits the hard refusal. (Mirrors the
   recovery whitelist model.)

3. **Image interlock, all required** (host-side + phone-side), or refuse:
   - sha256 ∈ `OSS_INIT_BOOT`;
   - the ramdisk `/init` references `oss-rebootmode-clear.ko` and carries the
     promoted counter (its strings include `promoted boot without a trustworthy
     boot count`), i.e. it clears the PMU first and was built `-DOSS_PROMOTED=1`;
   - the clear half exists before the count half is trusted:
     `/data/adb/moon/oss-init` is the same binary as the image's `/init`
     (sha256), and `moon-boot-confirm.sh` + the `moon-post-ready.sh` hook are installed;
   - **the misc record is valid with count 0** (`oss-init --bootcount init`, then
     `show` → `status=ok … count=0`), because a promoted image escalates on an
     absent record; and slots A/B are not foreign.

4. **A verified stock-BOOT rollback must pre-exist**: the script already backs up
   the current BOOT to phone + host and read-back-verifies; additionally require
   that a known-good stock/Moon BOOT (`8c0461ee` or `3bf29162`) is present in
   `$A26_RESCUE` and on the phone before writing, so a one-command rollback is
   always in hand.

5. **Explicit human confirmation**: with `--promote-oss-init`, print the safety
   summary (counter N, store, rollback locations, the Download-mode escape) and
   require the operator to type a fixed phrase (e.g. `promote oss-init to BOOT`)
   — not just `y`. `--dry-run` runs the whole interlock and prints the plan
   without writing.

6. **Unchanged**: exact size / header v4 / AVB footer checks, the read-back +
   verify + auto-rollback-on-mismatch, and `KNOWN_BAD` (the v2/v3 bricks stay
   refused).

The first promotion is attended, with USB and the Download-mode escape armed,
even though the counter is designed to make hands unnecessary.

---

## 5. Open items before the first BOOT write
- ~~Counter store~~: done in v7 (misc record v1, `bootcount-misc.h`).
- A BOOT-promotion image build (oss-init `/init` + both modules pinned) — a new
  builder akin to `build-slot-image.sh` but targeting BOOT, still unflashed.
- The reason channel (§1) if a contained power-off must power off in all cases.
- Soak (§3) green.
- The gated flash path (§4) implemented + dry-run-exercised.

---

## 6. Cleanup: the recovery-slot policy exception — merge it

`moon-direct-policy.sh` carries a recovery-reason exception (continue to Moon
when `/debug_ramdisk/oss-recovery-slot` exists). State today:
- The branch `stage2-native-init` version (`03f81842`, with the exception) is
  committed (e012e57); `main` still has `0f199df4` (without it); the phone's
  `/data/adb/moon/moon-direct-policy.sh` is `03f81842` (stock backed up as
  `…sh.bak-0f199df4`).

**Proposal: merge it into main** (it arrives there when `stage2-native-init`
merges — no separate action). Reasons:
- It is **marker-gated and inert in production**: the marker is added only by
  `build-recovery-slot.sh`, so it exists only in our recovery-slot test images.
  A normal recovery boot runs stock recovery (which never runs this policy); a
  BOOT image never carries the marker. A real device sees no behaviour change.
- It keeps source and device in agreement. If it stayed a `/data`-only patch,
  the next `install-autostart.sh` from main would overwrite it with `0f199df4`
  and silently break recovery-slot testing until re-pushed.
- It is small and self-documenting.

So: do **not** leave it as a device-only override; let it land in main via this
branch. The one-time `.bak-0f199df4` on the phone can be deleted once main
carries the exception.
