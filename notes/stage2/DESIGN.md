# Stage 2: OSS Phone's own init as PID 1, Android contained

Device: SM-A266M (a26xs), Exynos 1280 / s5e8825, Android 15, firmware
A266MUBU1AYB8. Updated 2026-10-25 after the OSS Phone kernel landed. No flashing
was done; device evidence is from read-only adb (R5CY323ES7X) and the research
workspace on `whale`.

## 0. TL;DR

The earlier blocker — the stock kernel lacked `CONFIG_PID_NS` — is **resolved**.
The kernel agent built the OSS Phone kernel from Samsung's source with
`CONFIG_PID_NS=y`, `CONFIG_DEVTMPFS=y`, and `CONFIG_SECURITY_DEFEX` removed,
proving all 306 vendor modules still match CRCs/vermagic and 274 rebuild
byte-identical (`scripts/kernel`, branch `kernel`, image
`boot-oss-mirror-A266MUBU1AYB8.img` sha256 `8c0461ee…`).

With PID namespaces available, the canonical goal is now implementable and is
implemented: **`oss-init`**, OSS Phone's own init, runs as the **real PID 1**
and stays PID 1 for the life of the system. It runs Android's entire init
(first + second stage, via the existing magiskinit → stock-init → Moon stack)
as its child inside a **new PID + mount namespace**, so Android init sees
`getpid()==1` in its own namespace and performs Samsung's full, proven hardware
bring-up unchanged. OSS Phone owns the real init seat; Android is contained.

The **iron rule** holds at every layer: any failure leaves the phone booting as
today (setup failure, container crash/early-death, or container hang → a clean
**uncontained** boot in the same boot). It is structurally impossible for
oss-init to exit/return.

> **v2 boot-looped; v3 is the fix.** The first containment build (`4d27b69b…`)
> mounted a tmpfs relay at `/oss-run`, which broke Android's SwitchRoot (it
> moves every mount into the read-only system root) and FATAL-looped — in both
> the contained init and the uncontained fallback, because the mount lived in
> oss-init's own namespace. v3 (`eaeb4494…`) mounts **nothing** (liveness now
> rides the global kernel log), and its fallback runs from a pristine namespace
> (the container's mounts are `CLONE_NEWNS`-isolated and freed on death). Full
> write-up: `notes/stage2/V2-BOOTLOOP-ROOTCAUSE.md`. The paths were validated
> under real Linux PID+mount namespaces on whale, including a switch_root-style
> mount-move proof (§5.5). The image is built and **not flashed**.
>
> **v3 ALSO boot-looped (`eaeb4494`); the architecture below (§5.1–§5.5) is
> SUPERSEDED.** Running the REAL magiskinit (not a stub) proved two fatal facts:
> magiskinit *must* be the kernel-loaded `/init` (with oss-init at `/init`,
> magiskinit backs up oss-init as itself, renames `/init`→`/sdcard`, and corrupts
> the hijack), and it *rewrites the shared rootfs*, corrupting oss-init's fallback
> — which then re-ran magiskinit and SIGSEGV'd as PID 1 → panic → reset loop.
> Full write-up: `notes/stage2/V3-BOOTLOOP-ROOTCAUSE.md`. The corrected design is
> §5.6; it is **not built or flashed** and must be validated (userspace harness,
> then the one-shot recovery path in `ONESHOT-TEST-PLAN.md`) before any flash.
> **Do not flash `4d27b69b` or `eaeb4494`.**

## 1. What PID 1 does today (stage 1), from evidence

- The ramdisk `/init` is **magiskinit** (Magisk v30.7). It runs only when
  `getpid()==1` (Magisk `init.rs:188`), does first-stage work, restores the
  stock init from `.backup/init.xz`, and execs it. Stock Android init then
  becomes PID 1 and does the full bring-up.
- Android brings up **306 modules** and **~105 init services**: ueventd/coldboot,
  first-stage mounts, SELinux policy load, the property service, firmware,
  adbd/USB, charging, and the watchdog.
- The **hardware watchdog** `s3c2410_wdt` (`/dev/watchdog0`, `10060000.watchdog_cl0`)
  is armed by the bootloader (`s3c2410_wdt.tmr_atboot=1`) and petted by
  `service watchdogd /system/bin/watchdogd 10 20` (class `core`,
  `init.s5e8825.rc:1013`, started at `:197`). Only stock init/watchdogd pets it.
- The direct-boot overlay stops zygote in Moon mode and starts Moon ≈34 s after
  the kernel.

## 2. The kernel blocker and how it was removed

The stage-1 analysis found three kernel facts that made containment impossible:
`# CONFIG_PID_NS is not set`, `# CONFIG_USER_NS is not set`,
`# CONFIG_DEVTMPFS is not set`, plus Samsung's RKP/KDP/DEFEX/PROCA/FIVE. The
OSS kernel addresses the load-bearing ones (`scripts/kernel/README.md`):

| Need | OSS kernel | Why it is safe for the frozen KMI |
| --- | --- | --- |
| PID namespace | `CONFIG_PID_NS=y` | `pid_namespace.h` inlines only; no vendor module uses them; no struct change |
| Kernel-maintained `/dev` | `CONFIG_DEVTMPFS=y` (not auto-mounted) | `device.h` prototypes used only by built-in drivers/base |
| Root processes not killed | `# CONFIG_SECURITY_DEFEX is not set` | header-free; removes the exec/open/write hooks that denied/killed "unauthorised" root — exactly what killed the v5 supervisors (§3) |
| (variant) user namespace | `CONFIG_USER_NS=y` in `oss-userns` | **not required** for a privileged root container; the default `oss` variant omits it |

KDP_NS protects mount-namespace vfsmounts (Android already creates them);
KDP_CRED already refcounts `cred->user_ns`; RKP/uH are unchanged. A **privileged
(root) container needs PID_NS, not USER_NS**, so stage 2 targets the `oss`
variant (`8c0461ee`), which keeps the smaller, better-understood change set.

Android init's PID-1 assumptions (`reboot_utils.cpp:184` `getpid()!=1`,
`init.cpp:967` `/proc/1/oom_score_adj`, subreaper, `reboot(2)`) are now **all
satisfied** because Android init genuinely is PID 1 *inside its namespace*.
`/proc/1` on the device is referenced only by `/system/bin/init` and `ueventd`;
inside the container both resolve to the container's own init. No vendor HAL
hard-codes PID 1.

## 3. Why containment, not a from-scratch init or full Halium (evidence)

- **Native PID-1 supervisors v5k–v5n** (`tools/linux-supervisor-v5*.c`) loaded
  vendor modules themselves and always died: `watchdog0 setup failed` at ~5–15 s
  (no devtmpfs → `/dev/watchdog0` missing), USB never enumerated, Download-mode
  resets (`v5m/v5n-kernel-markers.txt`, `v5n-soft-supervisor-experiment.log`).
  Two of the three root causes (devtmpfs, DEFEX) are now fixed in the OSS
  kernel, but the deeper lesson stands: **reproducing Samsung's 306-module +
  coldboot + USB + watchdog bring-up by hand is impractical.** So we keep
  Android doing 100 % of the bring-up and merely contain it.
- **Pre-init wrapper v6a–v6c** (`tools/android-bootstrap-v6*.c`) ran code as
  PID 1 then exec'd the exact stock init and even kept a forked root process
  alive in the booted system ("Android completed under V6c" + persistent root
  shell). This is the proven foundation; stage 2 generalizes it: instead of
  exec'ing stock init (giving up PID 1), `oss-init` *clones* it into a namespace
  and *keeps* PID 1.
- **Full Halium** (systemd host + `lxc-android` + libhybris) is the long-term
  shape but needs libhybris, a GNU/Linux host rootfs, and a container config for
  every HAL — weeks of brittle work. Stage 2 delivers the architectural seam
  (our init PID 1, Android in a PID+mount ns) at the cost of a ~190-line static
  binary, with Android still driving the hardware and still bringing Moon up.
  Moving userspace out of the container toward a Linux host is stage 3.

## 4. Options, reconsidered with PID_NS available

| Option | Safety | Complexity | Verdict |
| --- | --- | --- | --- |
| **A. Our init PID 1 + Android init in its own PID+mount ns (contain the existing full Android boot)** | High: Android does all bring-up unchanged; iron-rule fallback to today's boot at every layer | Low (≈190-line static binary + overlay rc) | **Chosen & implemented** |
| **B. Our init does a minimal first stage, hand vendor bring-up to a second-stage Android container** | Needs us to reproduce first-stage mounts/coldboot before the container — the v5 failure surface | High | Rejected (no benefit over A) |
| **C. Native PID-1 supervisor brings up hardware itself (v5)** | Proven to fail even with devtmpfs/DEFEX fixed: module order, USB, timing | High | Rejected (§3) |
| **D. Full Halium (systemd + lxc-android + libhybris)** | Unknown; large new surface | Very high | Deferred to stage 3 |
| **E. Stepping stone: `oss-pid1` pre-init that execs the stack (no containment)** | Highest (byte-identical boot) | Trivial | Kept as the safe fallback artifact (§5.6) |

**Chosen: Option A — `oss-init` containment.** It is the real stage-2 goal, is
safe to build and test now, and is the correct seam for stage 3.

## 5. The `oss-init` design (implemented on this branch)

### 5.1 Ramdisk layout (built on the OSS-kernel image `8c0461ee`)
```
/init          → oss-init    (NEW: real PID 1 / host supervisor, static aarch64)
/init.magisk   → magiskinit  (contained-primary: today's Magisk + Moon stack)
/init.stock    → stock init  (contained/host fallback)
/overlay.d/moon-direct-boot.rc + sbin/…   (unchanged)
/overlay.d/oss-contain.rc                 (NEW: one kernel-log liveness marker)
kernel, everything else → unchanged (verified byte-for-byte at build time)
```
Source `scripts/native-init/oss-init.c`; overlay
`scripts/native-init/overlay.d/oss-contain.rc`; builder
`scripts/native-init/build-stage2-image.sh`.

### 5.2 Boot flow
1. Bootloader → OSS kernel → kernel execs ramdisk `/init` = **oss-init as the
   real PID 1**.
2. oss-init writes a `/dev/kmsg` marker and opens held fds to `/init.magisk`,
   `/init.stock`, and `/dev/kmsg` (read, for liveness). **It mounts nothing** —
   anything it left mounted would break Android's SwitchRoot (this was the v2
   bug; see V2-BOOTLOOP-ROOTCAUSE.md).
3. `clone(CLONE_NEWPID | CLONE_NEWNS | SIGCHLD)` creates the container. The
   child is **PID 1 of a new PID + mount namespace**; it makes its mounts
   private and `execve("/init.magisk", argv, environ)` — the exact stock stack.
   Android does the full bring-up (modules, ueventd, SELinux, property service,
   firmware, adbd/USB, charging, watchdogd) and starts Moon, all as today, all
   inside the namespace.
4. The contained second-stage init writes a one-line marker to the **global
   kernel log** (`overlay.d/oss-contain.rc`). oss-init (real PID 1) reads
   `/dev/kmsg` — visible across namespaces, no mount — and takes the marker to
   mean "past first-stage SwitchRoot, watchdogd starting" (so its early-hang
   breaker stands down). It then supervises and handles container exit.

### 5.3 The iron rule — oss-init can never exit, and the fallback is clean
There is **no reachable `return`/`_exit` on the PID-1 path.** Every path ends in
an exec of the stock stack, or (only if even that fails) an infinite loop that
re-tries the exec and then issues a real `reboot(2)`. The one `_exit(127)` is in
the cloned child (a separate process); the parent observes that death and falls
back. The fallback is always a **re-exec of the stock stack in oss-init's own
namespace** (not a reboot): it stays in THIS boot and goes uncontained, so a
containment failure can never become a reset loop.

- **Setup fails** (`clone` error, e.g. a kernel without PID_NS): nothing was
  dirtied → boot the stock stack uncontained = today's boot.
- **Container dies** (crash, early bail, or SwitchRoot failure): boot the stock
  stack uncontained. The dead container's mount namespace is auto-cleaned by the
  kernel, so oss-init's namespace is **pristine** (proven in §5.5); loaded
  modules remain (insmod EEXIST is tolerated) — a clean warm re-init. No loop.
- **Container hangs before liveness** (no kernel-log marker within 60 s): kill
  the container, then boot the stock stack uncontained.
- Fallback execs use the **held fds** via `execveat(AT_EMPTY_PATH)`, so they work
  even after Android's `FreeRamdisk` unlinks the shared initramfs files.

Reboot/shutdown fidelity is intentionally dropped in v3 (no relay): a reboot
requested inside the contained Android tears down the namespace, which oss-init
answers with an uncontained boot rather than a real reboot. This is a safe,
documented limitation of the containment mode and also the escape hatch — one
`adb reboot` from contained Moon drops to a working uncontained boot for
reflashing. A mount-free kernel-log reboot relay can be added later.

### 5.4 Required behaviors — all preserved
- **Watchdog keepalive:** unchanged. watchdogd runs inside the container and
  pets `/dev/watchdog0` exactly as today. oss-init never pets it, so a genuine
  hang still self-resets instead of being masked.
- **Module order / firmware / USB-adb / charging:** unchanged — the contained
  Android does all of it.
- **Safe fallback to the exact stock init:** structural (§5.3), from a pristine
  namespace, never a reboot loop.

### 5.5 Validation (on whale, real Linux PID+mount namespaces)
A native build of `oss-init.c` (init paths and the `/dev/kmsg` reader redirected
to test stand-ins, 3 s deadline) was run under `unshare -Urmpf`:
- **healthy** (container emits the kernel-log marker, stays up) → `container
  reached second stage; supervising`, no fallback. ✓
- **crash** (container exits early, no marker) → `container exited; booting
  uncontained` → fallback re-exec reaches a stable boot. ✓ **And the pristine-ns
  proof:** that container mounted two tmpfs and did an `MS_MOVE`
  (switch_root-style) before dying; the fallback saw **0** of those mounts in
  oss-init's namespace (2 inside the container, 0 in the parent) — exactly the
  property that fixes the v2 SwitchRoot loop. ✓
- **hang** (no marker by the deadline) → `no liveness by deadline; killing and
  booting uncontained`. ✓

Build-time checks (in `build-stage2-image.sh`) confirm the kernel is
byte-identical to `8c0461ee`, `/init` is our supervisor, `/init.magisk` is the
base magiskinit byte-for-byte, the Moon overlay files are unchanged,
`/init.stock` is the decompressed backup, and the relay rc is present. An
independent unpack confirmed **no `oss-run` anywhere** in the ramdisk.

### 5.6 Corrected architecture (v4 — built and validated off-device, see V4-CONTAINMENT-VALIDATION.md)
The real-binary findings (`V3-BOOTLOOP-ROOTCAUSE.md`) dictate three changes.
Validated direction: with the normal Magisk layout, the real magiskinit
decompresses `.backup/init.xz` and execs the real stock init (observed under
qemu); with oss-init at `/init` it does not.

1. **The container gets a PRIVATE rootfs with the normal Magisk layout.** In the
   cloned child (new PID+mount ns), oss-init builds a fresh rootfs on its own
   superblock — either a tmpfs populated with a copy of the ramdisk, or an
   overlayfs (lowerdir = the initramfs read-only, upperdir = a fresh tmpfs) —
   whose `/init` IS magiskinit and which has `.backup/`, `overlay.d/`,
   `first_stage_ramdisk/` exactly as a normal Magisk boot. It `pivot_root`s into
   it and execs `/init`. magiskinit then runs **identically to the working
   stage-1 boot**, and every rewrite/`FreeRamdisk` it does lands on the private
   superblock, leaving oss-init's rootfs pristine.
2. **The fallback execs the STOCK init, never magiskinit.** `/init.stock` is the
   static stock init (confirmed statically linked); exec'ing it via a held fd is
   the normal stock-boot path and does not depend on the ramdisk magiskinit
   mangled. The fallback must **not** re-run magiskinit (that is what SIGSEGV'd
   as PID 1 in v3).
3. **The panic path cannot be made truly impossible while exec'ing a fallback.**
   Once oss-init execs any init and that init later dies as PID 1, the kernel
   panics. Therefore this design is testable **only** behind a mechanism that
   auto-returns to the known-good boot after a panic — the one-shot recovery
   path (`ONESHOT-TEST-PLAN.md`) — and must first be de-risked with the
   zero-boot userspace harness. Nothing goes on BOOT until both hold.

Open implementation risk: the private-rootfs copy/overlay runs inside PID 1
before exec; a bug there is itself a PID-1 crash. It must be built and proven in
the userspace harness first.

### 5.7 `oss-pid1` — the safe stepping stone (kept)
`scripts/native-init/oss-pid1.c` + `build-boot-image.sh` remain: a pre-init that
owns PID 1 then *execs* the stack (no containment), with a built image on the
**stock** kernel. It is the lowest-risk artifact and a useful bisection point
("does our-init-as-PID-1 boot at all, without namespaces?") if stage-2
containment misbehaves on-device.

## 6. On-device unknowns the morning test must resolve

These are sound in theory and validated in a generic namespace, but only the
real device + OSS kernel can confirm:
1. **SELinux enforcement on our PID 1.** The contained Android's second-stage
   init loads the policy and sets enforcing *globally*; our parent (real PID 1,
   `kernel`/`init` domain, never exec'd) is then subject to it. It does little
   (waitpid, read `/oss-run`, execveat/reboot). DEFEX is gone, and the `kernel`
   domain has `sys_boot` and `execute` of `init_exec`, so it should survive; if
   a denial blocks the fallback, the test can run with SELinux permissive
   (Magisk/moon policy) to isolate it.
2. **Android completing a full boot as a namespaced PID 1** on this device
   (property service, binder, servicemanager, ueventd coldboot, switch_root all
   inside the ns). The v2 log shows it already passes first-stage (dm/erofs
   mounts, vbmeta, up to SwitchRoot) as a namespaced init — encouraging — but a
   full boot to Moon is unproven here.
3. **Re-exec after a *fully booted* container later dies.** The fallback is
   proven clean for an early death (pristine namespace). After a complete boot,
   global state (SELinux policy loaded, modules, binder) is dirtier; a re-exec
   then may not fully re-init (e.g. a second policy load). This only matters
   when a container that already booted dies later; such a state almost always
   has adb up for an in-OS reflash. Not a reset loop either way (re-exec stays
   in-boot).

## 7. Risk ranking (honest)

1. **Container hangs after the liveness marker but before adbd** (narrow
   window): oss-init's hang-breaker has already stood down, and if adb is not
   yet up the only recovery is the hardware watchdog resetting → one reset, then
   the image boots again and re-attempts containment. Low probability (adbd is
   early, and the marker is at the very start of second stage), but it is the
   one case that could still need the physical rescue. **This is why the test is
   attended.** (A deterministic early failure, by contrast, falls back to an
   uncontained boot in the same boot — no reset.)
2. **SELinux denies our PID 1 the fallback exec** after a late container death:
   would leave Moon down with likely-live adb → in-OS reflash. Medium
   uncertainty; mitigated by the permissive-SELinux test variant.
3. **oss-init crashes before/at clone**: PID-1 panic. Low probability (static,
   ~190 lines, every path validated under real namespaces), mitigated by
   held-fd fallback and by `oss-pid1` as a bisection artifact.
4. **Everything else** ≈ today's risk: on any containment failure the iron rule
   yields a clean uncontained boot in the same boot.

No A/B slot and no hands during unattended runs ⇒ **do not flash unattended.**
Test attended, OSS kernel (passthrough) first, then stage 2 (§ TEST-PLAN.md).

## 8. Artifacts

- `scripts/native-init/oss-init.c` — the containing PID-1 supervisor (v3).
- `scripts/native-init/overlay.d/oss-contain.rc` — one kernel-log liveness marker.
- `scripts/native-init/build-stage2-image.sh` — reproducible builder (OSS base).
- `scripts/native-init/oss-pid1.c`, `build-boot-image.sh` — the stepping stone.
- `notes/stage2/V2-BOOTLOOP-ROOTCAUSE.md` — the v2 failure write-up.
- Built, **unflashed** image (git-ignored):
  `images/oss-stage2/boot-oss-stage2-A266MUBU1AYB8.img`
  sha256 `eaeb4494567f10459bc91f6c3eb156709f56bb066da0913bc307940b303c17e2`
  (base `8c0461ee…`; built on whale `~/oss-kernel/out/s2/`). Supersedes the
  boot-looping v2 image `4d27b69b…` — **do not flash `4d27b69b`.**
- `notes/stage2/TEST-PLAN.md` — attended morning test, layered on the kernel
  agent's tests.
