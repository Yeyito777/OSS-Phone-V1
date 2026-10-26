# Testing a built kernel on the phone

## Results on the phone (2026-10-25, attended)

Flashed in-OS with `scripts/direct-boot/flash-boot-inplace.sh --allow-kernel-change`, each image
checked against the same list on three boots (Moon ready, 306 modules, no
module/symbol/CRC/oops lines, root, display, touch device, audio, Wi-Fi +
HTTPS, charging, DPMS screen off/on, clean reboot):

| Image | Result |
| --- | --- |
| stock-mirror `28f31505…` | pass ×3: `/proc/version` identical to stock, taint 1024, Moon ready 21.3–21.6 s |
| oss-mirror `8c0461ee…` | pass ×5 (3 + 2 after the stage-2 rescue): `oss-phone@oss-phone-v1` banner, taint 1024, PID_NS/DEVTMPFS on, DEFEX off, 1 MiB log (dmesg starts at 0.000000), KFENCE sample 0, Moon ready 20.5–25.1 s |
| oss-userns | not run |

PID namespaces: busybox `unshare -p -f` gives the shell PID 1 and a fresh
proc mount shows only its own 4 processes (busybox's `--mount-proc` fails
with EINVAL on the chroot's shared root; mount proc by hand). devtmpfs mounts
with 430 nodes. **The OSS kernel image is the installed default**; stage 2
on top of it failed (see `notes/direct-boot/RESCUE.md`).

## Official source (2026-10-26)

Rebuilt from Samsung's own `SM-A266M_LA_15_Opensource.zip` (sha256
`6b993454…0453f`): the stock, oss and oss-userns BOOT images are
byte-identical to the mirror-built ones below (`28f31505…`, `8c0461ee…`,
`061b6923…`; the tree differs only in an unused defconfig). The image on the
phone (`8c0461ee…`, read back 2026-10-26) is therefore already the
official-source OSS kernel: nothing to flash, and the results above stand.
For any future kernel change: same ramdisk only, attended, with
`scripts/direct-boot/flash-boot-inplace.sh --allow-kernel-change` (changes to `/init` are refused).

Images (unflashed) live in the Mac checkout's git-ignored `images/kernel/`
(copies of whale's `$KROOT/out/boot/`); verify with `shasum -a 256 -c SHA256SUMS`.

| Image | Kernel | Purpose |
| --- | --- | --- |
| `boot-stock-mirror-A266MUBU1AYB8.img` | unmodified rebuild, banner identical to stock | Test 1: proves a self-built kernel boots with the prebuilt modules |
| `boot-oss-mirror-A266MUBU1AYB8.img` | OSS variant (no DEFEX, 1 MiB log, KFENCE idle, PID_NS, devtmpfs) | Test 2: the OSS Phone kernel |
| `boot-oss-userns-mirror-A266MUBU1AYB8.img` | OSS + USER_NS | Test 3, optional |
| `rollback/boot-moon-direct-A266MUBU1AYB8.img` | stock kernel; today's flashed BOOT (`3bf29162…`) | Rollback |

All three keep today's ramdisk (direct-boot overlay + Magisk v30.7) byte for
byte; only the kernel differs.

## Before flashing

1. The images were built on top of BOOT `3bf291627791e71b…19e9f6`. Check that
   this is still what is flashed:
   `adb shell su -c "'sha256sum /dev/block/by-name/boot'"`. If not, rebuild on
   whale with `BASE_BOOT_IMG=<new image> scripts/kernel/build-all.sh`, or run
   `make-boot-image.sh` for one variant.
2. Have Download-mode rescue ready (`notes/direct-boot/RESCUE.md`), but flash
   the **rollback image above**, not `a26-rescue/A266MUBU1AYB8-current/boot.img`
   (that one is the older Magisk-only BOOT without direct boot).
3. Battery above 50 %, USB connected to the Mac, someone at the phone: if a
   kernel does not boot, only buttons get it back to Download mode.

## Flashing

- In-OS (the device lead's root `dd` path): write the image to
  `/dev/block/by-name/boot`, read the full 64 MiB back and compare sha256
  with the image before rebooting.
- Or samloader: `adb reboot download`, `tools/samloader detect`,
  `tools/samloader flash -p BOOT <image>`, then restart by hand
  (Power + Volume Down 7–10 s); see RESCUE.md for why.

## Test 1: stock rebuild

Pass if all hold:
- Moon is on screen after the usual ≈34 s; adb works.
- `adb shell su -c "'sha256sum /dev/block/by-name/boot'"` equals the stock-mirror image hash.
- `cat /proc/version` identical to stock; `wc -l /proc/modules` = 306;
  `cat /proc/sys/kernel/tainted` = 1024 (same as stock).
- `dmesg | grep -iE "disagrees about version|Unknown symbol|invalid module format|exec format|uh|rkp|kdp"` shows nothing new compared with a stock boot.
- Magisk root (`su -c id`), touch, display, audio, Wi-Fi, battery/charging,
  one suspend/resume, and a clean `reboot`.

## Test 2: OSS variant

Test 1's checks, plus:
- `/proc/version` shows `oss-phone@oss-phone-v1 … Sun Oct 25 00:00:00 UTC 2026`.
- `zcat /proc/config.gz | grep -E "PID_NS|DEVTMPFS|SECURITY_DEFEX|LOG_BUF_SHIFT|KFENCE_SAMPLE"`.
- `cat /sys/module/kfence/parameters/sample_interval` = 0.
- In the Alpine chroot as root: `unshare -p -f --mount-proc sh -c 'echo $$'`
  prints `1`; `mkdir /tmp/d && mount -t devtmpfs devtmpfs /tmp/d && ls /tmp/d`.
- `dmesg | head` starts at `[    0.000000]` well after Moon is up (bigger buffer).

## Test 3 (optional): user namespaces

`unshare -U -r id` prints uid 0 inside. Afterwards consider
`sysctl -w user.max_user_namespaces=0` in boot policy until stage 2 needs it.

## Rollback

- It boots: flash `rollback/boot-moon-direct-A266MUBU1AYB8.img` the same way
  (read back and compare `3bf29162…`).
- It does not boot (black screen, boot loop, reset before the boot animation):
  RESCUE.md Level 3 (USB in; hold Volume Up + Volume Down + Power until
  black; release only Power, keep both volume keys until the teal warning;
  Volume Up), then `tools/samloader flash -p BOOT
  images/kernel/rollback/boot-moon-direct-A266MUBU1AYB8.img` and a manual
  restart. The direct-boot fallbacks are userspace and cannot catch a kernel
  that fails to boot; a kernel panic after boot does get one Android boot.
  BOOT flashes do not touch /data, Magisk or vendor_boot.
