# Step 1 — userspace containment harness on the booted phone (2026-10-26)

Goal: validate the corrected containment design (DESIGN.md §5.6) on the REAL
device + OSS kernel, as a **non-PID-1** root process, without disturbing the
live Moon system and without flashing anything.

## Method (safe by construction)
A tiny static harness (`clone(CLONE_NEWPID|CLONE_NEWNS|SIGCHLD)`; child:
`mount(MS_REC|MS_PRIVATE,/)`, `chroot(stagedroot)`, `execve("/init")`) ran the
**real magiskinit** as PID 1 of a new PID+mount namespace, chrooted into a
**private copy** of the OSS ramdisk staged in `/data/local/tmp` (scratch), with:
- the **normal Magisk layout**: `/init` = real magiskinit, `.backup/init` = a
  harmless **stub** standing in for the stock init (`.magisk`/`.rmlist` real),
- an **empty `/dev`** (no real device nodes), empty `/data` (not the real one),
- mounts made **private** so nothing propagates to the live mount namespace.

The real stock init was **not** run: it would load SELinux policy and write
global sysctls (not namespaced) and so would disturb the live system. The stub
stops exactly at the magiskinit→stock-init handoff, which is the step that had
been failing.

## Result — containment design validated natively
The stub wrote `/HARNESS-RESULT` (so magiskinit reached and exec'd `/init`):
```
STOCKINIT-STUB reached: real magiskinit restored and exec'd /init in the namespace
argv0=/init pid=1 uid=0
uname=Linux 5.10.223-android12-9-30597860-abA266MUBU1AYB8 aarch64
```
Post-run private rootfs showed magiskinit's normal first-stage behaviour:
`/init`(magiskinit) renamed to `/sdcard`; `.backup/init`(stub) restored to
`/init` and exec'd; `/storage`, `/data` created. `pid=1` confirms the namespaced
PID 1 on the real kernel.

Contrast with the broken stage-2 layout (`/init`=oss-init): there magiskinit
mis-backed-up oss-init and left `/init` dangling (V3-BOOTLOOP-ROOTCAUSE.md). With
the normal layout in a private rootfs, it hands off cleanly.

### Note on the backup format
magiskinit left `/init` **0 bytes** when `.backup/init.xz` was an `xz` I made
with the host `xz` (its decoder rejected it). Providing the backup
**uncompressed** as `.backup/init` worked (magiskinit renames it directly). A
real build must reuse Magisk's own `.backup/init.xz` (which magiskinit
decompresses fine) or store the stock init uncompressed.

## Live system: undisturbed
After the run: `moon.session.ready=1`, `/proc/1/comm=init` (Android, unchanged),
**0** harness mounts in the live namespace, mount count normal (165). Scratch
(`/data/local/tmp/osshar`, harness, tarball) removed.

## What this does and does NOT prove
- DOES: on the real OSS kernel, the real magiskinit runs correctly as a
  namespaced PID 1 in a private normal-Magisk rootfs and hands off to `/init` —
  the core of the §5.6 design — with no effect on the live system.
- DOES NOT: run the real stock init / a full contained Android boot (unsafe off
  a real boot). That remains for the one-shot recovery path
  (`ONESHOT-TEST-PLAN.md`), and oss-init's private-rootfs construction in PID 1
  still has to be implemented and is itself the remaining risk.
