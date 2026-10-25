# Direct boot: rescue procedure

Everything needed is on the Mac in `~/Desktop/a26-rescue/` (not in git):

```text
tools/samloader                          topjohnwu/samloader-rs 2.0.0, macOS universal
                                         (same release as the research host's; Heimdall port)
A266MUBU1AYB8-current/boot.img           BOOT read from the phone 2026-10-24, the Magisk image
                                         365a8b3c0d59f94a58a89c4f35f2a5d774f643452a8f07a6ac60d4cfc12e08a0
A266MUBU1AYB8-current/{vendor_boot,recovery,vbmeta,vbmeta_system,dtbo}.img + SHA256SUMS
stock-A266MUBU1AYB8/boot.img             unrooted stock BOOT (e35e0cd7…08c7), last resort
```

The research host holds the same images and its own samloader.

## Level 0: nothing to do

Moon failing at boot falls back to Android by itself. Resets or hangs before
Moon is up are counted; after three, the phone boots Android until Moon
succeeds again. A kernel panic gets one Android boot.

## Level 1: adb works (it does from early boot even in Moon mode)

```sh
scripts/a26-shell/autostart.sh skip-once   # next boot Android only
scripts/a26-shell/autostart.sh disable     # Android until re-enabled
adb shell su -c "'echo DIRECT_BOOT=0 >> /data/adb/moon/config'"   # back to the legacy takeover
```

Or, on the phone, press Volume Up while the boot animation shows.

## Level 2: put the old BOOT image back

From any working adb (Android or Moon):

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

## Level 3: no adb (boot loop or black screen)

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
