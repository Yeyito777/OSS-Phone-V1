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

## 2. BOOT-promotion safety: a boot-attempt counter

The one property the recovery slot gave us for free — a failed boot auto-returns
to the known-good BOOT — does not exist on the single BOOT partition. We
reconstruct an A/B-like "fall back to the last-good boot after N failures" in
software:

1. **Very early** (before containment, right after the PMU clear) oss-init reads
   and increments a persistent counter: `bootcount_bump()`.
2. If the counter is **>= N** (default 3), oss-init **skips containment entirely
   and execs the stock init** — today's proven boot (`8c0461ee`), uncontained.
   The phone always reaches a working system; promotion can never hard-brick it.
3. A **healthy session clears** the counter (`moon-boot-confirm.sh`), so the
   count reflects *consecutive* failures, not lifetime boots.
4. The existing in-boot fallbacks still apply within each attempt (early failure
   → stock init in the same boot; late failure → controlled reboot to BOOT;
   module-load failure → stock init).

Built now: the oss-init consumer (`bootcount_bump()`, the skip, bench cases
`bootcount-skip`/`bootcount-ok`) and the clear hook (`scripts/device/moon-boot-confirm.sh`).
The consumer is a **no-op until the store module is present** (open of
`/oss-bootcount.ko` fails → returns -1 → contain as usual), so it is inert in
every recovery-slot image and only engages on a promoted BOOT image.

### Counter store — where it lives (evaluated)

Requirements: writable by a tiny static binary **very early**, before any real
filesystem or `/dev` is set up; **survives a warm reset and a kernel panic** (a
boot loop must escalate); ideally **clears on a cold power-off** (a deliberate
power-cycle = fresh start); never collides with bootloader/Samsung usage.

| Store | Early write? | Survives warm reset / panic | Cold-boot behaviour | Verdict |
| --- | --- | --- | --- | --- |
| **(a) PMU alive-domain scratch register**, via a kernel module (as oss-rebootmode-clear already does) | **Yes** — a module needs no fs/dev; it runs at oss-init's first action | **Yes** — proven: the reboot-mode registers in this domain survived reset AND panic (`RECOVERY-PERSISTENCE-ROOTCAUSE.md`) | **Clears** (alive domain loses power) — matches "power-cycle = clean slate" | **Recommended** |
| **(b) Reserved area of the misc partition** (raw flash, away from the BCB) | Needs a block-device node before the by-name symlinks exist — oss-init would `mknod` the UFS node by major:minor, fragile; or defer the increment until after a mount (too late) | Yes (flash) | Persists — a cold power-cycle does NOT reset it; a bootlooping phone the user power-cycles keeps escalating (could be desired or not) | Fallback if no free PMU register; needs a proven-unused offset |
| **(c) `/data` (or `/metadata`) file** | **No** — `/data` is FBE/metadata-encrypted and only mountable after Android's vold/keystore come up; `/metadata` is ext4 and needs a real mount + ext4 writer | n/a | n/a | **Rejected for the early increment.** Usable only for the *clear* side, which already runs in userspace |

**Recommendation: (a), a PMU scratch register**, because it is the only store
oss-init can touch at its very first action with no filesystem and no device
node — the same reason oss-rebootmode-clear is a module — and its warm-reset-
survives / cold-boot-clears semantics are exactly an A/B retry counter. (b) is
the fallback if the kernel agent finds no truly free register; it needs a
documented, Samsung-unused misc offset and an early `mknod` of the misc block
device.

### `oss-bootcount.ko` — interface for the kernel agent

A sibling of oss-rebootmode-clear; gets its regmap from the DT syscon; touches
**one** alive-domain scratch register that nothing else uses (candidate: an
unused `SYSIP_DAT*` / spare `*INFORM*` word — the agent to pick and prove free,
as was done for 0x810/0x808/0x80c). Behaviour on `finit_module`:

- Read the register as `[ MAGIC:16 | COUNT:16 ]`. If MAGIC != ours, treat COUNT
  as 0 (first use / after a cold boot that cleared it).
- Default load: `count = min(count+1, 0xffff)`, write back with our MAGIC, and
  `pr_info("oss-bootcount: count=%u max=%u\n", count, OSS_BOOTCOUNT_MAX)`.
  oss-init parses that line (already implemented).
- `clear=1` param (or a writable sysfs param `count`): set COUNT=0, keep MAGIC,
  log `oss-bootcount: cleared`. This is what `moon-boot-confirm.sh` calls.
- `dry_run=1`: read and log only.
- Expose `count` and `max` as `0444`/`0644` module params for sysfs read/clear.
- KMI-clean, core imports only, like oss-rebootmode-clear.

When this module exists and is pinned into a BOOT-promotion image (and only
then), the skip engages. Pick N with the kernel agent; **N = 3** is the default
in oss-init (`BOOTCOUNT_MAX`) and the module should agree.

---

## 3. Soak test (recovery slot first, before any BOOT write)

Run the current v6 recovery-slot image (`f61349b3`) — and, once the counter
module exists, a v6+counter slot image — through a full shakeout from the slot,
where a failure still auto-recovers. Promotion is gated on all of this passing.

For each item, collect `/proc/sec_log` (oss-init + module lines), `logcat -b all`,
`dmesg`, and the relevant `dumpsys`:

1. **Repeated boots** — flash + `reboot-recovery` ≥ 10 times; every boot reaches
   Moon; oss-init declares the container live each time; no fallback line.
2. **Screen on/off** — several lock/unlock and timeout cycles; display wakes;
   SurfaceFlinger stays up (0 crashes); no Mali context errors.
3. **Audio** — playback + the audio bridge (`moon-audiod`) alive through a
   screen-off/on cycle.
4. **Wi-Fi** — associate, DHCP, a reachability check; survives suspend/resume.
5. **Telephony** — `moon-telephonyd` up; RIL connected; SIM state sane (airplane
   on/off if no SIM).
6. **Notifications + alarms** — post a notification; set an alarm a few minutes
   out and confirm it fires (exercises wakelocks/RTC from inside containment).
7. **Long uptime** — leave it up ≥ 2 h idle; no oss-init deadline/reboot, no OOM
   of the container, no SurfaceFlinger restart.
8. **Android handback inside containment** — `adb reboot` (contained reboot →
   lands on BOOT, v6 logs "requested reboot"); contained **power-off**
   (`svc power shutdown` / `reboot -p`) actually powers the phone off (v6 logs
   "requested power-off"); a forced container crash recovers via the proven
   fallback.
9. **Counter behaviour** (once the module exists, still from the slot): after a
   healthy boot `moon-boot-confirm.sh` zeroes the count; inject N early failures
   and confirm oss-init then skips containment and boots stock; confirm a cold
   power-off (store (a)) resets the count.

Record results in `notes/stage2/SOAK-<date>.md`. Any item failing stops promotion.

---

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
   - the ramdisk `/init` references `oss-rebootmode-clear.ko` AND `oss-bootcount`
     (grep the extracted `/init`, as `build-recovery-slot.sh` already greps),
     i.e. it clears the PMU first and increments the counter;
   - `/oss-bootcount.ko` is present in the ramdisk and its sha256 matches the
     pinned module;
   - the counter-store module's on-device presence is confirmed (the kernel
     carries it / it loads), and `moon-boot-confirm.sh` is installed at
     `/data/adb/moon/` — so the "clear on health" half of the net exists before
     the "count on boot" half is trusted;
   - `BOOTCOUNT_MAX` in the image agrees with the module.

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
- Kernel agent: `oss-bootcount.ko` (store (a), a proven-free register) + its
  on-device read/increment/clear test, like oss-rebootmode-clear's.
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
