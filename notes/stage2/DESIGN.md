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

The **iron rule** holds at every layer: any failure in our path leaves the
phone booting exactly as today (setup failure → exec the stock stack as real
PID 1; container crash / early death → fresh uncontained boot; container hang →
kill + uncontained boot; intentional reboot → performed for real). The four
paths were validated under real Linux PID+mount namespaces on whale (§5.5). The
image is built and **not flashed**; the on-device unknowns (SELinux enforcement
on our PID 1, full Android boot as a namespaced init, relay survival across
switch_root) are what the attended morning test resolves.

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
/overlay.d/oss-contain.rc                 (NEW: liveness + reboot relay)
kernel, everything else → unchanged (verified byte-for-byte at build time)
```
Source `scripts/native-init/oss-init.c`; overlay
`scripts/native-init/overlay.d/oss-contain.rc`; builder
`scripts/native-init/build-stage2-image.sh`.

### 5.2 Boot flow
1. Bootloader → OSS kernel → kernel execs ramdisk `/init` = **oss-init as the
   real PID 1**.
2. oss-init writes a `/dev/kmsg` marker, opens held fds to `/init.magisk` and
   `/init.stock` (so the fallback survives Android's `FreeRamdisk`), and mounts
   a tmpfs at `/oss-run` (shared superblock with the container, for the relay).
3. `clone(CLONE_NEWPID | CLONE_NEWNS | SIGCHLD)` creates the container. The
   child is **PID 1 of a new PID + mount namespace**; it makes its mounts
   private and `execve("/init.magisk", argv, environ)` — the exact stock stack.
   Android does the full bring-up (modules, ueventd, SELinux, property service,
   firmware, adbd/USB, charging, watchdogd) and starts Moon, all as today, all
   inside the namespace.
4. The parent (`oss-init`, real PID 1) supervises: reaps, watches the relay for
   liveness, and handles container exit.

### 5.3 The iron rule, at every layer
- **Setup fails** (`clone`/`mount` error): exec the stock stack as the real
  PID 1 → identical to today's boot, no containment.
- **Container dies without a reboot request** (crash, or early bail — relay
  `powerctl` empty): exec the stock stack as the real PID 1 → a fresh
  **uncontained** boot. The dead container's mount namespace is auto-cleaned by
  the kernel; loaded modules remain (insmod EEXIST is tolerated), so this is a
  warm re-init. No reboot loop.
- **Container hangs before liveness** (no `/oss-run/alive` within 90 s): kill the
  container, then exec the stock stack as the real PID 1.
- **Container asks for a real reboot/shutdown** (`sys.powerctl` → relay
  `/oss-run/powerctl`): perform it for real. `reboot(2)` from a child PID
  namespace only tears that namespace down, so PID 1 must do the real reboot;
  the relay carries the target (`bootloader`, `download`, `recovery`, poweroff).
- Fallback execs use the **held fds** via `execveat(AT_EMPTY_PATH)`, so they work
  even after Android's `FreeRamdisk` unlinks the shared initramfs files.

### 5.4 Required behaviors — all preserved
- **Watchdog keepalive:** unchanged. watchdogd runs inside the container and
  pets `/dev/watchdog0` exactly as today (DEVTMPFS/ueventd create the node in
  the container's `/dev`). oss-init never pets it, so a genuine hang still
  self-resets instead of being masked.
- **Module order / firmware / USB-adb / charging:** unchanged — the contained
  Android does all of it.
- **Safe fallback to the exact stock init:** built into every failure layer.

### 5.5 Validation (on whale, real Linux PID+mount namespaces)
A native build of `oss-init.c` (paths redirected to test stand-ins, 2 s
deadline) was run under `unshare -Urmpf`:
- **healthy** (container writes `alive`, stays up) → `container reported
  liveness; supervising`, no fallback. ✓
- **crash** (container exits early, no `powerctl`) → `container exited without a
  reboot request; booting uncontained` → fallback re-exec reaches a stable
  boot. ✓
- **hang** (no `alive` by the deadline) → `container never signalled liveness;
  killing and booting uncontained`. ✓
- **reboot** (container writes `powerctl=reboot,download` then exits) → reboot
  branch taken with the correct target, not fallback. ✓ (reboot syscall stubbed
  in the test so it never rebooted the host).

Build-time checks (in `build-stage2-image.sh`) confirm the kernel is
byte-identical to `8c0461ee`, `/init` is our supervisor, `/init.magisk` is the
base magiskinit byte-for-byte, the Moon overlay files are unchanged,
`/init.stock` is the decompressed backup, and the relay rc is present.

### 5.6 `oss-pid1` — the safe stepping stone (kept)
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
   inside the ns). Expected to work (Android init only needs `getpid()==1`,
   which holds), but unproven here.
3. **Relay tmpfs surviving switch_root** so liveness/reboot work. If it does
   not, oss-init degrades safely (treats exits as crashes → uncontained boot).

## 7. Risk ranking (honest)

1. **Container hangs after liveness but before adbd/watchdogd** (narrow window):
   no watchdog pet, no adb → hardware watchdog resets → loop → physical
   Download-mode rescue. Low probability (adbd and watchdogd are both early),
   high effort to recover. **This is why the test is attended.**
2. **SELinux denies our PID 1 the fallback exec** after a late container death:
   would leave Moon down with possibly-live adb → in-OS reflash, else physical
   rescue. Medium uncertainty; mitigated by the permissive-SELinux test variant.
3. **oss-init crashes before clone**: PID-1 panic. Low probability (static,
   ~190 lines, namespace logic validated), mitigated by held-fd fallback and by
   `oss-pid1` as a bisection artifact.
4. **Everything else** ≈ today's risk: on any containment failure the iron rule
   yields today's boot.

No A/B slot and no hands during unattended runs ⇒ **do not flash unattended.**
Test attended, OSS kernel (passthrough) first, then stage 2 (§ TEST-PLAN.md).

## 8. Artifacts

- `scripts/native-init/oss-init.c` — the containing PID-1 supervisor.
- `scripts/native-init/overlay.d/oss-contain.rc` — liveness + reboot relay.
- `scripts/native-init/build-stage2-image.sh` — reproducible builder (OSS base).
- `scripts/native-init/oss-pid1.c`, `build-boot-image.sh` — the stepping stone.
- Built, **unflashed** image (git-ignored):
  `images/oss-stage2/boot-oss-stage2-A266MUBU1AYB8.img`
  sha256 `4d27b69bed59fd075f00963d19b29d630307cf1a247d3193832cd381bcac62f3`
  (base `8c0461ee…`; also on whale
  `~/Workspace/research/linux-galaxy-a26/images/oss-stage2/`).
- `notes/stage2/TEST-PLAN.md` — attended morning test, layered on the kernel
  agent's tests.
