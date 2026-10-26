# Stage-2 v3 boot loop — root cause (reproduced with the real binaries)

Image `eaeb4494…` instant-reset-looped (never reached the boot animation, no
USB); physical Download-mode rescue again; phone back on `8c0461ee`. Log:
`~/Desktop/a26-rescue/stage2-v3-failure/last_kmsg.txt` (bit-flipped).

## Device log, decoded
```
oss-init: OSS-Phone init is the real PID 1; containing Android init     (l.7695)
oss-init: container exited, booting uncontained                          (l.7988)
oss-init: no init to exec; rebooting to a fresh boot
magiskinit: lstat '/init' / '.backup' / openat '/overlay.d' / '/data/magiskinit' …  No such file or directory  (l.8030)
Kernel panic - not syncing: Attempted to kill init!  exitcode=0x…0b (SIGSEGV), trace: do_group_exit ← __do_sys_waitid ← el0_svc
```

## Reproduced off-device with the REAL magiskinit (static aarch64) under qemu + namespaces
I extracted the real ramdisk from the OSS base image and ran the real
`magiskinit` as PID 1 in a `unshare -Urmnpf` + chroot, with `qemu-aarch64-static
-strace`. Two layouts:

**Our stage-2 layout (`/init` = oss-init, magiskinit at `/init.magisk`), exactly as flashed:**
```
getpid()=1 → mount /proc,/sys
openat("/.backup/.magisk") = ENOENT
openat("/init") = 3 ; openat("/data/magiskinit", O_CREAT) = 4   ← copies CURRENT /init …
   … but CURRENT /init is OSS-INIT, so /data/magiskinit becomes a copy of oss-init
symlink("/system/system/bin/init","/storage/self/primary")
renameat("/init","/sdcard")          ← renames OSS-INIT out of the way
mount("/sdcard","/sdcard",MS_BIND)
unlinkat("/init") = ENOENT
faccessat("/.backup/init.xz") = ENOENT ; stat("/.backup/init") = ENOENT   ← can't restore stock init
symlink("/system/bin/init","/init")  ← dangling (nothing mounted at /system)
execve("/init") = ENOENT → exit_group(1)
```
magiskinit **mutated the shared rootfs** (added `/data`, `/sdcard`, `/storage`;
renamed `/init`).

**Correct Magisk layout (`/init` = magiskinit, `.backup/init.xz` present), for contrast:**
```
decompress /.backup/init.xz → /.backup/init  (the real stock init)
renameat("/.backup/init","/init")
execve("/init")                              ← execs the REAL stock init → normal boot
```

## Root cause (three linked faults)

1. **magiskinit assumes it IS the kernel-loaded `/init`.** Its first-stage
   `hijack_init_with_switch_root()` + `restore_ramdisk_init()` back up *whatever
   is at `/init`* to `/data/magiskinit` (believing it to be magiskinit), rename
   `/init`→`/sdcard`, and restore the stock init from `.backup` to `/init`. With
   oss-init at `/init` and magiskinit exec'd from `/init.magisk`, magiskinit
   backs up **oss-init** as "magiskinit", renames oss-init away, and the whole
   SwitchRoot-hijack chain (which would make second-stage `/system/bin/init` be
   magiskinit) is corrupted. magiskinit cannot be run anywhere but `/init`.

2. **The container shares oss-init's rootfs superblock.** `CLONE_NEWNS` copies
   the mount *tree* but the initramfs tmpfs is one superblock, so magiskinit's
   renames/unlinks/`FreeRamdisk` inside the container **corrupt oss-init's own
   rootfs** — the exact files the fallback relies on.

3. **The fallback re-ran magiskinit and died as PID 1.** `boot_uncontained`
   tried `fd_magisk` first; re-running magiskinit on the now-corrupted rootfs
   made it **SIGSEGV as the real PID 1** → `Attempted to kill init` → kernel
   panic → instant reset. (The "no init to exec; rebooting" line is the prior
   loop iteration; the reboot had no registered restart handler that early, so
   PID 1 simply died.)

## Why the v2→v3 reasoning was wrong
v3 fixed the *mount-leak-into-SwitchRoot* bug (removed `/oss-run`) and proved a
pristine-namespace fallback in a **stub** test. But the stub stood in for
magiskinit; the real magiskinit (a) requires being `/init`, and (b) rewrites the
shared rootfs. Neither shows up with a stub. **Lesson: validate with the real
`magiskinit`/stock-init binaries, which this round did.**

## What a correct design must do (validated direction)
- The container must have the **normal Magisk layout** — `/init` = magiskinit,
  `.backup/init.xz` intact — so magiskinit runs exactly as in the working
  stage-1 boot (proven: it then restores and execs the real stock init).
- The container must have a **private rootfs** (its own superblock), so its
  rewrites/`FreeRamdisk` cannot corrupt oss-init's rootfs (overlayfs upper, or a
  tmpfs copy, set up in the child before exec).
- The fallback must exec the **stock init** (`/init.stock`, static, via a held
  fd), **never magiskinit** — stock init is the normal boot path and does not
  depend on the mangled ramdisk.
- Even so, a fallback that `exec`s any init can, in principle, let that init die
  as PID 1 → panic. The **panic path cannot be made truly impossible while
  exec'ing a fallback**; therefore testing must use a mechanism that returns to
  the working boot on its own after a panic — the one-shot recovery path
  (`ONESHOT-TEST-PLAN.md`). Nothing goes on the BOOT partition until that
  mechanism is proven.
