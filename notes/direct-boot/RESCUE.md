# Direct boot: rescue procedure

Everything needed is on the Mac in `~/Desktop/a26-rescue/` (not in git):

**Installed since 2026-10-25: the OSS kernel with direct boot,
`8c0461eef2ca…3b15f73`** (`known-good/boot-oss-kernel-direct.img`). Flash that
in a rescue unless there is a reason to go back further.

```text
known-good/boot-oss-kernel-direct.img    OSS kernel + direct boot, the installed default
                                         8c0461eef2ca07b3d35199149907b40ff5db07fc10b4b92567ef1e3443b15f73
known-good/boot-stock-kernel-direct.img  stock kernel + direct boot (stage 1, 2026-10-24)
                                         3bf291627791e71ba00b4fcdde8b6bd92faa62d6fe2f14da9b906fd86619e9f6
known-good/SHA256SUMS
tools/samloader                          topjohnwu/samloader-rs 2.0.0, macOS universal
                                         (same release as the research host's; Heimdall port)
A266MUBU1AYB8-current/boot.img           BOOT read from the phone 2026-10-24, the Magisk image
                                         365a8b3c0d59f94a58a89c4f35f2a5d774f643452a8f07a6ac60d4cfc12e08a0
A266MUBU1AYB8-current/{vendor_boot,recovery,vbmeta,vbmeta_system,dtbo}.img + SHA256SUMS
stock-A266MUBU1AYB8/boot.img             unrooted stock BOOT (e35e0cd7…08c7), last resort
inplace-backups/                         BOOT as read back before each in-OS flash
stage2-failure/                          last_kmsg of the stage-2 boot that panicked PID 1
```

Never flash `boot-oss-stage2-A266MUBU1AYB8.img` (`4d27b69b…`): Android's
SwitchRoot failed on the `/oss-run` mount, oss-init exited and PID 1
panicked, a boot loop that needed Download mode (2026-10-25).
`flash-boot-inplace.sh` refuses it.

The research host holds the same images and its own samloader.

## Level 0: nothing to do

Moon failing at boot falls back to Android by itself (no readiness within
240 s, or the supervisor exiting, start the framework). Resets, hangs and
fallbacks before Moon is up are counted and a ready Moon resets the count;
after three in a row the phone boots Android without trying Moon until
`autostart.sh enable` clears the count. A kernel panic gets one Android boot.
All of this was exercised on the phone on 2026-10-25 (RESEARCH.md).

## Level 1: adb works (it does from early boot even in Moon mode)

```sh
scripts/a26-shell/autostart.sh skip-once   # next boot Android only
scripts/a26-shell/autostart.sh disable     # Android until re-enabled
adb shell su -c "'echo DIRECT_BOOT=0 >> /data/adb/moon/config'"   # back to the legacy takeover
```

Or, on the phone, press Volume Up while the boot animation shows.

## Flashing BOOT: in-OS by default (no Download mode)

Any BOOT image is written from the running OS (Android or Moon) over adb,
with no button presses:

```sh
scripts/direct-boot/flash-boot-inplace.sh --dry-run IMAGE   # checks + backup only
scripts/direct-boot/flash-boot-inplace.sh IMAGE
adb reboot
```

It refuses an image that is not exactly the partition's size (64 MiB), not a
boot v4 image with an AVB footer, or whose kernel or Magisk `/init` differ
from the installed image (`--allow-kernel-change` / `--allow-init-change`
override; never use them unattended). Images it knows (the `KNOWN_GOOD` table
at its top: the two `known-good/` images) may change the kernel without an
override, so rolling back never needs one; known non-booting images
(`KNOWN_BAD`: stage 2) are refused. Before writing it reads the installed
image back into `/data/adb/moon/boot-backups/` (newest three) and
`~/Desktop/a26-rescue/inplace-backups/` on the Mac. It writes with root `dd`
(`conv=fsync`), syncs, drops caches and re-reads the partition with O_DIRECT;
the sha256 must match, otherwise it writes the backup back and verifies that.

Verified 2026-10-25 by writing the installed direct-boot image
(`3bf29162…e9f6`) over itself: 131072 sectors written to sda14, read-back
hash identical, and the phone booted into Moon afterwards.

An image that boots but misbehaves is undone the same way, by flashing the
backup. Download mode (Level 3) is only for an image that no longer boots far
enough for adb; ramdisk-only images from `build-boot-image.sh` keep the
kernel and magiskinit, and adb comes up early in every boot mode.

## Level 2: put a known-good BOOT image back

From any working adb (Android or Moon), flash the known-good image in place:

```sh
cd ~/Desktop/a26-rescue
shasum -a 256 -c known-good/SHA256SUMS
~/Desktop/OSS-Phone-V1/scripts/direct-boot/flash-boot-inplace.sh \
    known-good/boot-oss-kernel-direct.img     # or boot-stock-kernel-direct.img
adb reboot
```

Through Download mode instead, which costs a physical restart:

```sh
cd ~/Desktop/a26-rescue
shasum -a 256 -c known-good/SHA256SUMS
adb reboot download             # or Level 3 when adb is gone
tools/samloader detect          # wait until it finds the phone
tools/samloader flash -p BOOT known-good/boot-oss-kernel-direct.img
```

`A266MUBU1AYB8-current/boot.img` is older still: the Magisk BOOT from before
direct boot (Android first, Moon after a post-boot takeover).

On this phone samloader's closing "Rebooting device..." does **not** reboot:
the phone stays in Download mode until it is restarted by hand (hold
**Power + Volume Down** about 7–10 s). Until then it also refuses a second
session ("Unexpected handshake response", with both the libusb and vcom
backends), so every flash costs one physical restart, and flashing again
first needs Download mode re-entered (Level 3 steps 1–4). samloader 2.0.0's
libusb backend works on macOS (verified 2026-10-24); `--usb-backend vcom`
uses `/dev/cu.usbmodem*` instead.

## Level 3: no adb (boot loop or black screen) — the only case for Download mode

What worked on 2026-10-25, from a boot loop:

1. Connect the USB cable to the Mac first.
2. Hold **Volume Up + Volume Down + Power** together until the screen goes
   black.
3. Release **only Power**; keep both volume keys held until the teal
   warning screen appears.
4. Press **Volume Up** (Download mode).
5. Run the Level 2 `samloader detect` and `flash` commands with
   `known-good/boot-oss-kernel-direct.img`, then restart by hand (hold
   **Power + Volume Down** about 7–10 s); Moon is up about 45 s later.

The older sequence (Power + Volume Down until black, then quickly switch to
both volume keys) depends on hitting a short window and was too hard to time.

Alternative entry: stock recovery (Power + Volume Up with USB connected) →
"Reboot to bootloader" enters Download mode.

Data, Magisk and /data/adb are untouched by BOOT flashes.
