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
stage2-failure/                          last_kmsg of the stage-2 v2 boot loop
stage2-v3-failure/                       last_kmsg of the stage-2 v3 boot loop
```

## Hard rule: never flash a different ramdisk `/init`

No image whose ramdisk `/init` differs from the installed image's is flashed,
with no override. Such an image may only be tried from a one-shot boot slot,
and this non-A/B phone has none. A new PID 1 is the one change whose failure
no userspace fallback can catch: both stage-2 images (`/init` = oss-init)
reset before adb came up and needed a physical Download-mode rescue on
2026-10-25:

- `4d27b69b…` (v2): Android's SwitchRoot failed on the `/oss-run` mount,
  oss-init exited and PID 1 panicked.
- `eaeb4494…` (v3): no adb within 4 minutes, reset loop
  (`stage2-v3-failure/last_kmsg.txt`).

`flash-boot-inplace.sh` refuses any changed `/init` and both images by hash
(`KNOWN_BAD`); its old `--allow-init-change` override is gone. Kernel-only
changes on the same ramdisk (`--allow-kernel-change`) remain possible, attended.

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
from the installed image. `--allow-kernel-change` overrides the kernel check
(never unattended); a changed `/init` has no override at all (hard rule
above). Images it knows (the `KNOWN_GOOD` table
at its top: the two `known-good/` images) may change the kernel without an
override, so rolling back never needs one; known non-booting images
(`KNOWN_BAD`: both stage-2 builds) are refused. Before writing it reads the installed
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

What worked twice on 2026-10-25, from a reset loop:

1. Plug the USB cable into a **data port on the Mac itself**, not a hub.
2. Hold **Volume Up + Volume Down + Power** together, and keep holding
   through the phone's resets.
3. The instant the screen goes black, lift **only Power**; keep both volume
   keys held until the **teal** warning screen appears.
4. Press **Volume Up** (Download mode).
5. Flash `known-good/boot-oss-kernel-direct.img` (below, or let the watcher
   do it), then restart by hand (hold **Power + Volume Down** about 7–10 s);
   Moon is up about 45 s later.

What did **not** work for the user: holding only the volume keys, and the
Power + Volume Down variants (Power + Volume Down until black, then switching
to both volume keys).

**Mac-side watcher.** During attended tests the device lead's parent runs a
watcher on the Mac that waits for Samsung's Download-mode USB device
(product `0x685d`) and runs `tools/samloader flash -p BOOT
known-good/boot-oss-kernel-direct.img` by itself. With it armed, steps 1–4
are all the user does before the manual restart. Without it, run Level 2's
`samloader detect` and `flash` commands.

Alternative entry: stock recovery (Power + Volume Up with USB connected) →
"Reboot to bootloader" enters Download mode.

Data, Magisk and /data/adb are untouched by BOOT flashes.
