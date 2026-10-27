# oss-init v4 (private-rootfs containment): design and off-device validation

Built 2026-10-26, unflashed. Source `scripts/native-init/oss-init.c`; image
builder `scripts/native-init/build-slot-image.sh containment` (recovery slot only); test
bench `scripts/native-init/test/run-tests.sh`.

## Images (deterministic, whitelisted in flash-recovery-inplace.sh)
| Image | sha256 | What it is |
| --- | --- | --- |
| `recovery-slot-stage2-v4-A266MUBU1AYB8.img` | `f0b7263a…036022a1` | Today's BOOT (`8c0461ee`) + vendor_boot in the recovery slot; ramdisk `/init` = oss-init v4, `/init.magisk` = magiskinit, `/init.stock` = static stock init, `overlay.d/oss-contain.rc`, recovery-slot marker |
| `recovery-slot-moon-A266MUBU1AYB8.img` | `cc406f02…3a30b86e` | The same with `/init` = oss-pid1 instead of oss-init (clear, then magiskinit as `/init`): today's boot from the slot, plus the marker so Moon comes up |

Both are in `images/oss-stage2/recovery-slot/` (Mac) and on whale. Built twice,
byte-identical.

## What v4 does (requirements → implementation)
- **Private ramdisk copy, normal Magisk layout.** The cloned child (new PID +
  mount namespace) makes its mounts private, mounts a tmpfs on `/mnt` *in its
  own namespace*, copies the root filesystem into it (skipping mountpoints,
  keeping `/dev`, `/proc`, `/sys` empty), renames `init.magisk` → `/init`,
  drops `init.stock`, then `MS_MOVE`s the tmpfs onto `/` and chroots, exactly
  like init's own SwitchRoot, and execs `/init` (magiskinit).
- **Fallback execs only the stock init, via a held fd.** `/init.stock` is opened
  at start (`O_CLOEXEC`, so the container never inherits it). At fallback
  CLOEXEC is cleared and `execveat(fd, "", AT_EMPTY_PATH)` runs it; the path is
  a second try. magiskinit is never the fallback.
- **No mounts in oss-init's namespace.** oss-init calls `mount()` only inside
  the child. It reads `/dev/kmsg` (a `mknod` if missing, not a mount) for two
  markers: `OSSPHONE_CONTAIN_LIVE` (liveness, from `oss-contain.rc`) and
  `sec-reboot … register restart handler successfully` (reboot-safe).
- **Every error path ends in an exec or a reboot.** Early failure (before
  `sec_reboot` registered) → exec stock init: kernel state is clean, and a
  reboot would keep the RECOVERY reason and re-enter the slot. Late failure →
  controlled `reboot(2)`; with `sec_reboot` loaded that writes the normal reason
  and lands on BOOT (proven on this phone by the stock round trip:
  recovery-reason entry + recovery's own `reboot("")` → system). A reboot
  requested by the contained Android (its init dies by SIGHUP/SIGINT,
  `pid_namespace.c:331-338`) → oss-init performs the real reboot/power-off. No
  `return`/`exit` exists on the PID-1 path; exec failure loops into reboot.

## Validation (whale; real namespaces; real merged ramdisk)
oss-init (x86 build of the identical source, `-DTEST_STDERR`) ran as PID 1 of a
fresh user+pid+mount+net namespace, chrooted on a bind mount (rootfs is a mount
on the phone) into the real merged recovery-slot ramdisk (vendor fragments +
BOOT ramdisk). aarch64 binaries ran via binfmt qemu.

| Case | Container | oss-init | Result |
| --- | --- | --- | --- |
| layout + crash | stub sees `/init`, no `init.magisk`/`init.stock`, `.backup/init.xz` and empty `/mnt` present, PID 1; makes 2 tmpfs mounts + `MS_MOVE` (SwitchRoot-style), crashes | early → exec stock init via fd | stock-init stub runs as PID 1 (`exe=/init.stock`); oss-init's namespace holds only root + the proc the stub mounted: **no leak**; original tree intact |
| hang | never live | deadline → SIGKILL → exec stock init | same clean fallback |
| late failure | exits after the `sec_reboot` marker | controlled `reboot(2)` | real reboot syscall (test namespace killed by SIGHUP, status 129) |
| contained reboot | `reboot(RESTART)` in its PID ns | init SIGHUP'd → real reboot | 129 |
| live | liveness marker seen, then idles | logs `container is live … supervising as the real PID 1`; no deadline kill, no fallback | supervised until the bench's own SIGKILL (137) |
| **real binaries** | real **magiskinit** from the copy: proc/sys, `/data` tmpfs, `/init`→`/sdcard` hijack, restored `.backup/init.xz` → `/init`, exec'd the real **stock init** → `FirstStageMain` mounted `/dev`, devpts, proc, sysfs, then died on test-environment limits (no selinuxfs, `gid=1000` unmapped) | early → `execveat(3, "", AT_EMPTY_PATH) = 0` → real stock init `FirstStageMain` as PID 1 | strace shows **zero `mount()` calls by oss-init** between its start and the fallback |
| **image's exact aarch64 /init** (qemu) | — | qemu-user returns EINVAL for `clone(NEWNS\|NEWPID)` → "clone failed" path | `execveat(held fd)` of the real stock init **succeeded** |

Bug found and fixed by the real-binary run: `execveat` on an `O_CLOEXEC` fd
returns ENOENT when the binary is handed to an interpreter as `/dev/fd/N`
(binfmt). Native ELF on the phone would not hit it, but the fallback must never
depend on that, so CLOEXEC is cleared just before the exec.

## Not provable off-device (resolved only by booting it from the slot)
- The real stock init's full first and second stage *inside* the namespace on
  the real device (selinuxfs, device nodes, dm, SwitchRoot, property service,
  ueventd, watchdogd) — the test environment cannot create device nodes.
- The `sec_reboot` marker timing and the controlled reboot landing on BOOT from
  the slot (sound by source + the stock round trip; confirmed only on device).
- Containment from the aarch64 binary itself (qemu-user cannot do the
  namespace clone; the x86 build of the same source can and did).

## Test order on the phone (after the canary passes)
1. `recovery-slot-moon` (`cc406f02`): the normal boot from the recovery slot,
   Moon via the marker. Proves the slot boots a full system.
2. `recovery-slot-stage2-v4`: oss-init containment. adb runs **inside** the
   container, so its `/proc/1` is Android's init and the real PID 1 is not
   visible from there. Pass = the global kernel log (`dmesg | grep oss-init`)
   shows `the real PID 1 (v4…)` then `container is live … supervising as the
   real PID 1`, with no fallback line, and Moon is up. Any failure → stock init
   or a reboot to BOOT, by design (the log says which).
Each run: `--check-bcb`, `flash-recovery-inplace.sh <image>`,
`reboot-recovery`, observe, then `--restore-stock`.

## Update 2026-10-26: the PMU clear comes first (images above rebuilt)
Stage 0 showed the recovery selection survives a reset or panic
(RECOVERY-PERSISTENCE-ROOTCAUSE.md). oss-init, oss-pid1 and the canary now load
the kernel agent's `oss-rebootmode-clear.ko` (now v2, sha256 `7130cf3c…`: masked
mode clear, verified read-back, `require_mode`; the first module `a18606a8` is
withdrawn) as their very
first action (`scripts/native-init/rebootmode-clear.h`). If the load fails they
never panic or reboot on purpose: oss-init skips containment and execs the stock
init with no reboot fallback; oss-pid1 execs the stock init; the canary execs
the stock recovery init. `build-recovery-slot.sh` refuses any payload whose
`/init` is not one of these pre-inits.

Added tests (`test/run-tests.sh`; a user namespace cannot load modules, so the
containment cases fake a successful clear and the *-noclear cases call the real
`finit_module` on the real `.ko`, which returns EPERM):
| Case | Result |
| --- | --- |
| containment matrix (live, layout+crash, hang, late reboot, contained reboot) | unchanged, each now logs `PMU reboot state set to NORMAL` first |
| oss-init, clear fails (EPERM) | no clone; stock init via the held fd; no leaked mounts |
| oss-pid1, clear fails | stock init exec'd; magiskinit never runs |
| canary, clear fails | does not panic; execs the recovery init |
| canary, clear OK | exits PID 1 (the deliberate panic) |
| oss-pid1, clear OK, real magiskinit | trace: `rename("/init.magisk","/init")` → exec `/init` (real magiskinit) → it hijacks itself (`/init`→`/sdcard`) → restores `.backup/init` → execs the real stock init: the normal Magisk sequence |
| each built image's exact aarch64 `/init` (qemu; finit_module = ENOSYS there) | canary → `execve("/system/bin/init")`; moon → `execve("/init.stock")`; stage 2 → `execveat(held fd)`: all no-clear fallbacks, none panics |
The successful clear itself (the module writing the PMU) is only provable on
the device: the kernel agent's insmod test, then the canary.

### Rebuild with module v2 (2026-10-26)
Images rebuilt against `7130cf3c` (on-device re-test passed): canary `71d68ced`
(loads with `require_mode=1`, EEXIST counts as failure), recovery-slot-moon
`cc406f02`, stage-2 v4 `f0b7263a`; all built twice, byte-identical. Bench: all
ten cases pass. Exact-binary checks: each image carries the v2 `.ko`, only the
canary's `/init` contains `require_mode=1`, and each `/init` (qemu, no module
load possible) took its fallback without panicking: canary → `/system/bin/init`,
moon → `/init.stock`, stage 2 → `execveat(held fd)`.

### v5 (2026-10-27): Mali PID-namespace fix + SELinux-safe oss-init
The on-device Stage-3 run (`ONDEVICE-RESULTS-20261026.md`) proved containment
works but found three defects. v5 (`recovery-slot-stage2-v5`, sha256
`4c0ea361…`) fixes them:
- **Mali:** the slot's payload ramdisk now carries the kernel agent's patched
  `mali_kbase.ko` (`f36fd339`, `get_task_pid(current, PIDTYPE_TGID)` instead of
  `find_get_pid(current->tgid)`) at `lib/modules/mali_kbase.ko`. The payload is
  concatenated last, so it overrides the vendor copy (`b5704023`) — the same
  last-wins path that makes oss-init the `/init`. Verified: the effective (last
  fragment) module is `f36fd339`, the vendor copy is left byte-untouched, and
  `modules.load` is unchanged (bare `mali_kbase.ko`, same deps).
- **Liveness under SELinux:** `watch_kmsg` treats the first `EACCES`/`EPERM` read
  of `/dev/kmsg` as proof the container loaded its enforcing policy — which
  happens in second-stage init, after first stage loaded `sec_reboot` — so it
  sets both `live` and `reboot_safe`, closes the read fd and stops (no audit-log
  flood). oss-init logs through a write fd opened at startup (writes on an open
  fd need only `write`, which the `kernel` domain has), so its later lines
  survive the policy load.
- **Hang path:** if the deadline `kill` is refused, oss-init reboots immediately
  (the PMU clear already succeeded, so a reset lands on BOOT) instead of blocking
  in `waitpid`.
Bench: all ten cases pass (the kill-refused and EACCES-live branches only fire
under SELinux, so they are covered by the device run, not the userns bench).
Exact-binary checks confirm the effective Mali, the v2 rebootmode `.ko`, the v5
`/init`, and the qemu fallback (`execveat` of the held fd, no panic).

### v6 (2026-10-27): reboot/power-off/failure classification + boot-count consumer
Off-device (`recovery-slot-stage2-v6`, sha256 `f61349b3…`). Changes:
- **Contained reboot vs power-off vs failure.** The v5 device run mislabelled a
  contained `adb reboot` as a failure. `reboot_pid_ns`/`zap_pid_ns_processes`
  (kernel) set the container PID 1's wait status to `SIGHUP` for reboot and
  `SIGINT` for power-off; v6 reads that (both signalled and exited encodings,
  measured `0x1`/`0x2` on the bench) and reboots or powers off accordingly, and
  treats SIGKILL/other as a real failure (existing fallback). Residual: a
  reboot issued by a non-PID-1 container process arrives only as SIGKILL and
  loses the distinction — PROMOTION-PLAN.md §1 specs a shared `sys.powerctl`
  reason channel.
- **Boot-attempt counter consumer** (`bootcount_bump()`): reads/increments a
  persistent counter at oss-init's first action and, at `BOOTCOUNT_MAX`
  consecutive failures, skips containment and execs the stock init. A no-op
  until the `oss-bootcount` kernel module is present, so inert in the slot;
  engages only on a promoted BOOT image. Store + module interface + the gated
  BOOT flash path: PROMOTION-PLAN.md §2/§4. Clear hook:
  `scripts/device/moon-boot-confirm.sh`.
Bench: 14 cases pass, including `contained-reboot`, `contained-poweroff`,
`late-reboot` (a real exit-42 failure), `bootcount-skip`, `bootcount-ok`.
Exact-binary checks: effective mali `f36fd339`, rebootmode `.ko` `7130cf3c`,
`/init` carries the v6 reboot/power-off/bootcount strings and the
`oss-bootcount` reference, qemu `/init` takes the `execveat` fallback, no panic.
