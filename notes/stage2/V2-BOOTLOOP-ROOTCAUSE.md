# Stage-2 v2 boot loop — root cause and fix

Image `boot-oss-stage2-A266MUBU1AYB8.img` sha256 `4d27b69b…` (the first
containment build) boot-looped on the device: every boot reset right after the
"bootloader unlocked" screen, with no USB, forcing a physical Download-mode
rescue. The phone was restored to the OSS-kernel passthrough image
(`8c0461ee…`). Evidence: `~/Desktop/a26-rescue/stage2-failure/last_kmsg.txt`
(several boots, partly bit-flipped as Samsung's last_kmsg always is).

## What happened, from the log

The clear-enough lines of one boot:
```
init: [libfs_mgr] ... Created logical partition product/vendor_dlkm ...
init: __mount(source=/dev/block/dm-0,target=/system,type=erofs) = 0: Success
init: Switching boot to '/system'
init: Unable to move mount at 'oss-run': No such file or directory
init: InitFatalReboot: signal 6
```
and, in a later boot:
```
reboot reason: 0x... - Kernel PANIC
Attempted to kill init! exitcode=0x00000010
RESET CAUSE - kernel panic
```

So the contained Android init got **all the way through first-stage** — dm
logical partitions created, `/system` mounted as erofs, vbmeta checked — and
died only at `SwitchRoot`. (That is actually good news: Android init runs fine
as a namespaced PID 1 through first-stage mounts.)

## Root cause: the relay mount broke SwitchRoot, in BOTH paths

v2's `oss-init` mounted a tmpfs relay at `/oss-run` **in its own mount
namespace**, before `clone()`, to carry liveness/reboot signals to/from the
container. Android's `SwitchRoot` (`system/core/init/switch_root.cpp`) moves
*every* mount under `/` into the new root:
```cpp
mkdir(new_root + mount_path);                    // /system/oss-run
mount(mount_path, new_root + mount_path, MS_MOVE) // fails: /system is read-only
    -> PLOG(FATAL) "Unable to move mount at 'oss-run'"
```
`/system` is a read-only erofs, so `mkdir(/system/oss-run)` fails, the `MS_MOVE`
target does not exist (ENOENT), and init `LOG(FATAL)`s → `abort()` → SIGABRT →
`InitFatalReboot`.

Two consequences, and this is why it was a *loop with no USB*:
1. The **contained** init died at SwitchRoot (before adbd, so no USB).
2. `oss-init` then ran its fallback — re-exec the stock stack uncontained — but
   the `/oss-run` mount lived in **oss-init's own namespace**, which the
   re-exec'd init inherited. So the uncontained fallback hit the **identical**
   SwitchRoot failure, now as the real PID 1 → `InitFatalReboot` → real reboot
   → next boot → same thing. The `Attempted to kill init` / kernel panic is
   that FATAL path tearing down PID 1.

The "iron rule" (fall back to today's boot) was present in code but **void in
practice**: the fallback inherited the same poison and could not boot either.

## The two bugs, precisely

1. **Relay mount placement.** Any mount `oss-init` leaves in its namespace is
   inherited by both the container and the fallback, and Android's SwitchRoot
   cannot move it into the read-only system root → FATAL.
2. **Fallback not from a clean state.** A fallback that inherits oss-init's
   dirtied namespace is not a safe boot. The fallback must run from exactly the
   namespace the kernel handed PID 1.

## The fix (v3)

- **`oss-init` mounts nothing.** The `/oss-run` relay is removed entirely.
  Liveness is now observed through the **global kernel log** (`/dev/kmsg`),
  which is visible across namespaces and needs no mount: the contained
  second-stage init writes a one-line marker (`overlay.d/oss-contain.rc`), and
  `oss-init` reads `/dev/kmsg` for it. Nothing survives into Android's
  SwitchRoot, so SwitchRoot is unaffected — in the container *and* in the
  fallback.
- **The fallback runs from a pristine namespace.** Because the container is
  cloned with `CLONE_NEWNS`, everything it mounts (its `/dev`, `/system`
  SwitchRoot, …) lives in *its* namespace and is torn down by the kernel when
  it dies. `oss-init`'s namespace stays exactly as the kernel handed it over,
  so re-exec'ing the stock stack is a clean, uncontained boot — **proven**: a
  test container that mounts two tmpfs and does an `MS_MOVE` (switch_root-style)
  then dies leaves `oss-init`'s namespace with **0** of those mounts (§ test
  matrix in DESIGN.md §5.5).
- **Structurally impossible for `oss-init` to exit/return.** There is no
  reachable `return`/`_exit` on the PID-1 path. Every path ends in an exec of
  the stock stack or, if even that fails, an infinite loop that re-tries the
  exec and then issues a real `reboot(2)`. The fallback is a re-exec (stays in
  THIS boot, goes uncontained), which — unlike a plain reboot — can never become
  a reset loop: a containment failure yields one working uncontained boot with
  adb, not another reset.

## Why v3 cannot repeat the v2 loop
- No mount is left for SwitchRoot to choke on (contained or fallback).
- The fallback is clean (pristine namespace, proven).
- Containment failure → uncontained boot in the same boot (adb up) → the device
  lead can reflash in-OS; it is not a no-USB reset loop.

Residual on-device unknowns remain (SELinux on PID 1; a re-exec after a *fully
booted* container later dies; whether a very-late hang before watchdogd can
still loop) — see DESIGN.md §6–§7 and TEST-PLAN.md. These are why the next test
is attended and starts from the passthrough image.
