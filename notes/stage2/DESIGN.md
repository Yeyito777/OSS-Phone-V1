# Stage 2: OSS Phone's own init as PID 1 — research, design, and groundwork

Device: SM-A266M (a26xs), Exynos 1280 / s5e8825, Android 15, firmware
A266MUBU1AYB8, Samsung GKI kernel 5.10.223-android12-9. Written 2026-10-25.
No flashing was done; all device evidence is from read-only adb (serial
R5CY323ES7X) and the research workspace on `whale`.

## 0. TL;DR

The canonical "Linux-first" goal — **our init as the real PID 1 with Android's
init (first and second stage) contained in its own PID/mount namespace** — is
**not achievable on this device's stock kernel**, and not safely testable
tonight. The kernel is built with `# CONFIG_PID_NS is not set` and
`# CONFIG_USER_NS is not set` (verified below), so the container model that
Halium/Droidian/Ubuntu Touch use literally cannot be constructed, and Samsung's
RKP/KDP/DEFEX/PROCA/FIVE stack is designed to kill exactly the kind of
unsigned PID-1-over-vendor arrangement this would require. The research
workspace already proved this empirically: every native-PID-1 supervisor
(v5k–v5n) died; the pre-init wrapper that execs the exact stock init (v6a–v6c)
succeeded and even kept a persistent root process alive.

So stage 2 delivers the **safe, real, incremental step** that is worth taking
now and is the correct foundation for the long-term goal: **`oss-pid1`**, OSS
Phone's own tiny PID-1 pre-init. The kernel execs *our* binary as PID 1; it
owns the first userspace instant, then chain-loads the proven stage-1 boot
(magiskinit → stock init → Moon) and falls back to stock Android init if
anything goes wrong. PID 1 now belongs to OSS Phone, with a boot that is
bit-for-bit today's boot on the happy path and a normal Android boot on the
failure path. Full Linux-first containment is scoped as a separate
custom-kernel effort (§6).

## 1. What PID 1 does today (stage 1), from evidence

- The ramdisk `/init` is **magiskinit** (Magisk v30.7). It runs only when
  `getpid() == 1` (Magisk `native/src/init/init.rs:188`), does Magisk's
  first-stage work, restores the stock init from `.backup/init.xz`, and execs
  it. The real stock Android init then becomes PID 1 (execve preserves the PID)
  and performs Samsung's full bring-up.
- Android init brings up **306 kernel modules** (`/proc/modules`) and **~105
  init services**, including ueventd/coldboot, first-stage mounts, the SELinux
  policy load, the property service, firmware, adbd/USB, charging, and the
  watchdog pet loop.
- The **hardware watchdog** is `s3c2410_wdt` at `10060000.watchdog_cl0`,
  exposed as `/dev/watchdog0`. The bootloader arms it
  (`s3c2410_wdt.tmr_atboot=1` on the kernel cmdline) and a stock init service
  pets it: `service watchdogd /system/bin/watchdogd 10 20` (class `core`,
  `/vendor/etc/init/init.s5e8825.rc:1013`), started early at
  `init.s5e8825.rc:197`. `watchdogd` opens `/dev/watchdog0`, sets a 30 s timer
  and pets every 10 s. **Nothing but stock init/watchdogd pets this device's
  watchdog**, which is why anything that replaces init must either run
  watchdogd's equivalent within a few seconds or keep stock init in charge.
- The direct-boot overlay (`scripts/direct-boot/`) adds init rc that stops
  zygote in Moon mode and starts the Moon supervisor; Moon is on screen ≈34 s
  after the kernel starts.

## 2. Question 1 — can Android init run contained while our init is PID 1?

**Answer: no, not on the stock kernel.** The pieces, with evidence:

### 2.1 The kernel has no PID or user namespaces
From the live device (`zcat /proc/config.gz`):
```
CONFIG_NAMESPACES=y
CONFIG_UTS_NS=y
# CONFIG_USER_NS is not set
# CONFIG_PID_NS is not set
CONFIG_NET_NS=y
# CONFIG_DEVTMPFS is not set
```
Containing Android init "in its own PID namespace so it still sees
`getpid()==1`" is impossible: `CLONE_NEWPID` will fail with `EINVAL`. There is
no user namespace either, so an unprivileged/rootless container is out. This
single fact removes the LXC-style model entirely.

### 2.2 Android init assumes it is the real PID 1 in load-bearing ways
Reading AOSP `android15-release` `system/core/init`:
- `reboot_utils.cpp:184` — the crash signal handlers branch on
  `getpid() != 1`; a non-PID-1 init `_exit()`s on any fatal signal instead of
  handling reboot.
- `init.cpp:967` — init writes its own oom score to `/proc/1/oom_score_adj`.
- init is the subreaper/zombie-reaper for the whole process tree and issues the
  `reboot(2)` syscall directly (`reboot_utils.cpp:103`). All of this assumes
  real PID 1. Running stock init as a plain child (PID≠1) without a PID
  namespace is unsupported and wedges reaping, reboot and RescueParty timing.
- `/proc/1/` is referenced on the device only by `/system/bin/init` and
  `/system/bin/ueventd` (scan of `/system/bin`, `/vendor/bin`,
  `/system/lib64`, `/vendor/lib64`): **no vendor HAL or daemon hard-codes
  PID 1**, so the vendor stack itself does not block a reparent — but init
  does.

### 2.3 Device nodes are not auto-created
`# CONFIG_DEVTMPFS is not set`: there is no `devtmpfs` to mount on `/dev`.
First-stage init `mknod`s a handful of nodes by hand and ueventd's coldboot
creates the rest from `/sys` uevents. A replacement PID 1 would have to
reproduce all of this before any driver's char device (including
`/dev/watchdog0`) is usable — which is exactly what bit the v5 supervisors.

### 2.4 SELinux policy is loaded exactly once, by whoever owns second stage
`androidboot.selinux=enforcing` (bootconfig) and `CONFIG_SECURITY_SELINUX_DEVELOP=y`.
The monolithic policy is loaded once in init's `selinux_setup` second stage;
after that the policy is sealed. Whoever loads it defines every domain. If an
unsigned init owns this step it must ship and maintain Samsung's full policy;
if it lets stock init own it (our choice), our pre-init code only exists
*before* the load and must live within the inherited `kernel`/`init` domain.
The live policy's `kernel` domain is broad but not total (sampled with
`magiskpolicy --print-rules`: `allow kernel self capability { … sys_module
sys_boot sys_admin mknod … }`, `allow kernel kmsg_device chr_file …`), and
Magisk injects a permissive `magisk` domain with `dyntransition`. This is
enough for a trivial pre-init but not for a from-scratch PID 1.

### 2.5 Samsung's hardening is built to stop unsigned PID-1-over-vendor
From `/proc/config.gz` and the cmdline:
```
kvm-arm.mode=protected        (uH hypervisor active)
CONFIG_UH=y  CONFIG_RKP=y  CONFIG_KDP=y  CONFIG_KDP_CRED=y  CONFIG_KDP_NS=y
CONFIG_SECURITY_DEFEX=y  CONFIG_PROCA=y  CONFIG_FIVE=y  CONFIG_KNOX_NCM=y
CONFIG_INTEGRITY=y
```
RKP (Realtime Kernel Protection) and KDP protect kernel credential and
**namespace** structures (`KDP_NS`); DEFEX restricts privilege-escalation
paths; PROCA/FIVE authenticate processes/files against Samsung certificates.
Standing up a new namespaced container and a new privileged init is the precise
workload these are designed to detect and reset. The empirical record agrees:
the native supervisors never survived (§3).

### 2.6 How Halium / Droidian / Ubuntu Touch / Sailfish actually do it
Their model *is* "GNU/Linux init (systemd) as real PID 1, Android contained":
systemd on the host, and the Android vendor userspace running inside an
**LXC container** (`lxc-android`) whose own Android init is PID 1 *inside the
container's PID namespace*; libhybris lets glibc processes load the bionic HAL
`.so`s. But that model's hard prerequisites are exactly what this device
lacks:
- **PID and user namespaces** must be enabled in the kernel. Droidian/Halium
  ship or require kernels with `CONFIG_PID_NS`/`CONFIG_USER_NS`; this stock
  kernel has both **off**.
- They run on devices **without** Samsung's RKP/KDP/DEFEX/PROCA/FIVE, or on
  custom Halium kernels where the offending protections are patched out.
- They typically relax SELinux (permissive host) — fighting
  `androidboot.selinux=enforcing` plus Samsung's enforcement.

So Halium's approach is real and is the right long-term target, but it requires
**a custom kernel** here. It cannot be done on A266MUBU1AYB8's shipped kernel.

## 3. Evidence from the research workspace (whale)

- **Native PID-1 supervisors v5k–v5n** (`tools/linux-supervisor-v5*.c`): our
  init was PID 1 and loaded vendor modules itself. They always failed:
  - `notes/v5m-kernel-markers.txt` / `v5n-kernel-markers.txt`: after loading
    `s3c2410_wdt.ko`, `a26-v5m: watchdog0 setup failed: No such file or
    directory` at ~5–15 s — `/dev/watchdog0` did not exist yet (no devtmpfs,
    coldboot not reproduced). v5m then rebooted to Download mode; v5n chose to
    "continue with software supervision, hardware watchdog unavailable" and
    **hung with no USB** (`v5n-soft-supervisor-experiment.log`: "No V5n ACM or
    rescue within 10 minutes"). v5l likewise never brought USB up
    (`v5l-physical-usb-watcher.log`: 700 s, nothing).
  - Root cause matches §2.3/§2.5: reproducing Samsung's module order + coldboot
    + USB gadget + watchdog timing by hand is impractical, and the hardening
    stack is hostile to it.
- **Pre-init wrapper v6a–v6c** (`tools/android-bootstrap-v6*.c`): our code runs
  as PID 1, forks a helper, then `execve`s the exact stock init.
  - `notes/v6a-…`: "SUCCESS: exact stock Android init completed under V6a
    wrapper"; Android reached `boot_completed=1`.
  - `notes/v6c-resumed-watcher.log`: "Android completed under V6c" with
    `u:r:init:s0 … 1 0 … init` as PID 1, and a **persistent standalone BusyBox
    root shell** forked by the wrapper still answering on tcp:2626 inside the
    fully booted system. This is the proof that a forked-before-exec helper
    survives into the running OS.

The v6 line is the proven, safe foundation. Stage 2 generalizes it into a
first-class OSS Phone init.

## 4. Question 2 — alternatives compared, and the choice

| Option | Safety | Complexity | Boot speed | Verdict |
|---|---|---|---|---|
| **A. Our init PID 1 + Android init in PID/mount ns (Halium LXC model)** | Impossible on stock kernel (no PID_NS/USER_NS); custom kernel vs RKP/KDP/DEFEX = high brick risk | Very high (kernel + libhybris + container) | Unknown; likely slower bring-up | **Rejected for now** (§2, §6) |
| **B. Our init does a minimal first stage, hands vendor bring-up to Android init in a container** | Same namespace blocker as A; also needs devtmpfs/coldboot reproduction | Very high | — | **Rejected** (same root cause) |
| **C. Native PID-1 supervisor that brings up hardware itself (v5)** | Proven to fail — watchdog/USB/module-order deaths, Download-mode resets | High | — | **Rejected** (§3) |
| **D. Extend the proven v6 pre-init wrapper into OSS Phone's own PID-1 pre-init, chain-loading the stage-1 stack with a stock fallback** | High: happy path is byte-identical to today; failure path is a normal Android boot | Low (≈40-line static binary + build) | Adds <1 ms before handoff; Moon still ≈34 s | **Chosen** |

**Chosen architecture: Option D — `oss-pid1`.** It is the only option that is
safe to build and test now, it makes PID 1 genuinely OSS Phone's, and it is the
natural staging point from which later pre-init Linux work (and, with a custom
kernel, the §6 containment) can grow without ever giving up the safe fallback.

## 5. The `oss-pid1` design (implemented on this branch)

### 5.1 Ramdisk layout (built from the current Moon direct-boot image)
```
/init          → oss-pid1   (NEW: our static aarch64 PID-1 pre-init)
/init.magisk   → magiskinit (the base image's former /init, byte-identical)
/init.stock    → stock Android init (decompressed from Magisk .backup/init.xz)
/overlay.d/…   → Moon direct-boot rc + sbin scripts (unchanged)
kernel, everything else → unchanged (verified byte-for-byte at build time)
```
Source: `scripts/native-init/oss-pid1.c`; builder:
`scripts/native-init/build-boot-image.sh` (base
`boot-moon-direct-A266MUBU1AYB8.img`, sha256 `3bf29162…e9f6`).

### 5.2 Boot flow
1. Bootloader → kernel → kernel execs ramdisk `/init`, which is now **oss-pid1,
   running as the real PID 1**.
2. oss-pid1 writes one marker to `/dev/kmsg`
   (`oss-pid1: OSS-Phone pre-init running as PID 1; chaining to stage-1 boot`)
   — proof of ownership, recoverable from dmesg/pstore — and does nothing else.
3. **Primary handoff:** `execve("/init.magisk", argv, environ)` with argv
   passed through unchanged (argv[0]=="/init", no "selinux_setup"), so
   magiskinit sees exactly what the kernel would have handed it and runs its
   normal first-stage path. From here the boot is **bit-for-bit today's boot**:
   Magisk restores/execs stock init → 306 modules, ueventd, SELinux, property
   service, firmware, adbd/USB, charging, watchdogd → overlay.d stops zygote →
   Moon supervisor → Moon on screen ≈34 s.
4. The execve preserves PID 1, so stock Android init ends up as PID 1 exactly
   as it does today; every `getpid()==1` assumption (§2.2) holds. oss-pid1 is
   gone (its process image was replaced), and the ramdisk is freed at
   switch_root as usual — oss-pid1 leaves no residue in the running system
   other than its kmsg line.

### 5.3 Failure modes and fallbacks (safety is the design)
- **magiskinit exec fails** (missing/corrupt `/init.magisk`): oss-pid1 logs and
  `execve("/init.stock", …)` → a **normal, unrooted stock Android boot**. No
  Moon, but a working, adb-reachable phone — recoverable.
- **both execs fail** (both files absent — impossible in a correctly built
  ramdisk, and verified present at build time): oss-pid1 does **not** return
  (returning from PID 1 panics the kernel). It spins on `pause()`; the
  bootloader-armed s3c2410 watchdog then resets the device into a fresh boot.
- **oss-pid1 itself crashes before exec:** this is the only new brick-class
  risk vs. today. Mitigations: the program is ~40 lines, static, no heap, only
  `open`/`write`/`close`/`execve`/`pause`; its exec-chain + fallback were
  tested under qemu-aarch64 (primary-present → primary; primary-absent →
  fallback); and modifying the ramdisk `/init` is the *same class* of change
  Magisk already makes and the device already boots (so AVB/FIVE/PROCA do not
  block it — top-level vbmeta is verification-disabled on this unlocked
  device). Residual risk is low but non-zero (§7).
- Everything downstream of the handoff keeps **today's** fallbacks: the Moon
  volume-key window, the supervisor's Android restore, the policy's
  skip/disable/legacy paths, the kernel-panic one-Android-boot rule, and the
  `scripts/direct-boot` rescue ladder in `notes/direct-boot/RESCUE.md`.

### 5.4 Required behaviors — all preserved
- **Watchdog keepalive:** unchanged. stock init/watchdogd still owns
  `/dev/watchdog0`; oss-pid1 adds <1 ms before the identical handoff, far
  inside the 30 s timer.
- **Module loading order / firmware:** unchanged — stock init does all of it.
- **USB/adb from early boot:** unchanged — adbd comes up exactly as today (and
  the overlay forces `sys.usb.config=adb` in Moon mode as before).
- **Charging:** unchanged — vendor charging path and `boot_completed` battery
  actions run as today.
- **Safe fallback to the exact stock init:** built in (§5.3), both to the
  current Magisk+Moon stack and, below it, to plain stock Android.

## 6. The long-term Linux-first path (deferred, needs a custom kernel)

To reach the true goal (our init real PID 1, Android vendor stack as a
contained HAL), a future effort must, in order:
1. Build a custom kernel for s5e8825 with `CONFIG_PID_NS`, `CONFIG_USER_NS`,
   `CONFIG_DEVTMPFS` (+mount), and the container cgroup/namespace options
   enabled, and with DEFEX/PROCA/FIVE/KDP_NS relaxed enough to allow an
   unsigned init and new namespaces — while keeping the device bootable
   (RKP/uH may require `kvm-arm.mode` changes or signed-kernel handling; this
   is the hard, brick-prone part). Samsung kernel source is in
   `samsung-opensource/`.
2. Adopt the Halium layout: systemd (or our Rust init) as PID 1 on a GNU/Linux
   rootfs; Android `/system`+`/vendor` in an `lxc-android` container with its
   own Android init as container-PID-1 bringing up the vendor HALs; libhybris
   for HAL access; Moon on the host.
3. Only attempt on hardware with a tested rescue path and, ideally, after a
   recovery-based A/B-style safety net — never as an unattended flash.

`oss-pid1` is the correct first rung: it already owns PID 1, so step 2's init
can later be grown in place behind the same safe-fallback guarantee.

## 7. Risk ranking (honest)

1. **Deferring the "real" containment goal** — *expected, not a defect.* The
   stock kernel makes it impossible; attempting it tonight would mean an
   unsigned custom kernel vs. RKP/KDP with no A/B and no hands = unacceptable
   brick risk. **Do not attempt tonight.**
2. **oss-pid1 crashes before exec** — low probability (tiny, static, qemu-
   tested), high consequence (PID-1 panic → likely Download-mode rescue needing
   physical hands). This is the one new risk the morning test must gate.
3. **Everything else** — negligible delta from today: the happy path is
   byte-identical, and the ramdisk-/init change is the same class Magisk
   already makes and the device already boots.

Given no A/B slot and no hands during unattended runs, **the image must not be
flashed unattended.** It is ready for an attended morning test (§ TEST-PLAN.md)
with the device lead present for the rescue path.

## 8. Artifacts

- `scripts/native-init/oss-pid1.c` — the PID-1 pre-init.
- `scripts/native-init/build-boot-image.sh` — reproducible builder.
- `scripts/native-init/README.md` — summary + controls.
- Built, **unflashed** image (git-ignored):
  `images/oss-pid1/boot-oss-pid1-A266MUBU1AYB8.img`
  sha256 `9a63cf28b7dc0fef3c3cd2261c135f1a78c39f283ed0d7466dacef652e64dea4`
  (also on whale `~/Workspace/research/linux-galaxy-a26/images/oss-pid1/` after
  a research-host build). Base `boot-moon-direct-A266MUBU1AYB8.img` sha256
  `3bf29162…e9f6`.
- `notes/stage2/TEST-PLAN.md` — the attended morning test.
