# Direct boot: rescue procedure

Everything needed is on the Mac in `~/Desktop/a26-rescue/` (not in git):

```text
tools/samloader                          topjohnwu/samloader-rs 2.0.0, macOS universal
                                         (same release as the research host's; Heimdall port)
A266MUBU1AYB8-current/boot.img           BOOT read from the phone 2026-10-24, the Magisk image
                                         365a8b3c0d59f94a58a89c4f35f2a5d774f643452a8f07a6ac60d4cfc12e08a0
A266MUBU1AYB8-current/{vendor_boot,recovery,vbmeta,vbmeta_system,dtbo}.img + SHA256SUMS
stock-A266MUBU1AYB8/boot.img             unrooted stock BOOT (e35e0cd7…08c7), last resort
inplace-backups/                         BOOT as read back before each in-OS flash
```

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
override; never use them unattended). Before writing it reads the installed
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

## Level 2: put the old BOOT image back

From any working adb (Android or Moon), flash the known-good image in place:

```sh
cd ~/Desktop/a26-rescue
shasum -a 256 -c A266MUBU1AYB8-current/SHA256SUMS
~/Desktop/OSS-Phone-V1/scripts/direct-boot/flash-boot-inplace.sh \
    A266MUBU1AYB8-current/boot.img      # the pre-direct-boot Magisk image
adb reboot
```

Its kernel and magiskinit are identical to the direct-boot image's (dry run
verified), so no override is needed. Through Download mode instead, which
costs a physical restart:

```sh
cd ~/Desktop/a26-rescue
shasum -a 256 -c A266MUBU1AYB8-current/SHA256SUMS
adb reboot download
tools/samloader detect          # wait until it finds the phone
tools/samloader flash -p BOOT A266MUBU1AYB8-current/boot.img
```

On this phone samloader's closing "Rebooting device..." does **not** reboot:
the phone stays in Download mode until it is restarted by hand (hold
**Power + Volume Down** about 7–10 s). Until then it also refuses a second
session ("Unexpected handshake response", with both the libusb and vcom
backends), so every flash costs one physical restart, and flashing again
first needs Download mode re-entered (Level 3 steps 1–3). samloader 2.0.0's
libusb backend works on macOS (verified 2026-10-24); `--usb-backend vcom`
uses `/dev/cu.usbmodem*` instead.

## Level 3: no adb (boot loop or black screen) — the only case for Download mode

1. Force restart: hold **Power + Volume Down** about 7–10 s.
2. As soon as the screen goes black, release them and hold **Volume Up +
   Volume Down** with the USB cable connected to the Mac.
3. On the warning screen press **Volume Up** (Download mode).
4. Run the Level 2 `samloader detect` and `flash` commands.

Alternative entry: stock recovery (Power + Volume Up with USB connected) →
"Reboot to bootloader" enters Download mode.

The restored Magisk BOOT brings back today's behaviour (Android boot, Moon
after the post-boot takeover). Data, Magisk and /data/adb are untouched by
BOOT flashes.
