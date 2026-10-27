# Installing OSS-Phone-V1 on a phone

One script provisions everything this project installs on `/data`:

```sh
scripts/install-all.sh --dry-run            # what would change; writes nothing
scripts/install-all.sh --a26-system ../a26-system [--reboot]
```

It is idempotent: every file is replaced atomically, and a second run (or
`--dry-run` right after an install) reports everything unchanged.

## What must already be there

The installer checks these and stops if one is missing; it never creates or
flashes them.

| Prerequisite | Notes |
| --- | --- |
| Samsung Galaxy A26 (SM-A266M), bootloader unlocked, Magisk root | Root through `/data/local/tmp/su`. Android's rescue mode stays intact: system, vendor and product partitions are never modified |
| The reference Alpine rootfs at `/data/local/a26-linux` | With `busybox.static`, Xorg (`/usr/libexec/Xorg`), Python 3 and the Xorg configuration in `etc/X11/xorg.conf.d`. Not published in this repository |
| The direct-boot boot image (optional) | Magisk boot image with `scripts/direct-boot/overlay.d`: Moon starts instead of the Java framework. Flashed separately with `scripts/direct-boot/flash-boot-inplace.sh` under its rules (README there); without it Moon takes over after Android boots |
| Mac tools | `adb`, `brew install rustup zig`, `rustup target add aarch64-unknown-linux-musl`, `cargo install --locked cargo-zigbuild` |
| A build host for the native parts | The Android NDK (audio, GPU compositor, telephony helper) and the musl cross toolchain (System app). Used when the Mac has neither: `A26_BUILD_HOST` (default `whale`), with the NDK at `~/Workspace/tools-cache/android-ndk-r27c` |
| `secrets` store with `A26_SHELL_PIN` | Six digits. Read only through `secrets run`, never on a command line; only needed when the phone has no PIN config yet, or with `--reset-pin` |
| A checkout of [a26-system](https://github.com/Yeyito777/a26-system) | The System app and the native Wi-Fi backend: `--a26-system DIR` (default `../a26-system`), or `--skip-system` |

## What it installs, in order

1. **Preflight**: root, rootfs, Magisk, whether the direct-boot ramdisk is
   installed, whether a PIN config exists.
2. **System app and Wi-Fi backend** (`a26-system/scripts/install.sh`):
   `/opt/a26-system` (the app, `a26-wifi-service`, `a26-wifi-ctl`, the
   Wi-Fi start/stop scripts and Alpine Wi-Fi packages).
3. **Moon**: `/opt/a26-shell/bin` (Moon, `a26-shellctl`, moon-notifyd,
   `moon-notifyctl`, `moon-power`, tools). The PIN config
   (`/etc/a26-shell/config.json`, root-only, salted hash) is created only if
   missing or with `--reset-pin`.
4. **Native audio**: `/opt/a26-audio` (moon-audiod, Moon's permission and
   audio-service stand-ins).
5. **Autostart and power**: the boot supervisor, direct-boot policy and
   session scripts in `/data/adb/moon`, the Magisk `service.d` hook, the
   chroot and Xorg handoff scripts, the GPU display path (moon-compositor,
   Xorg's dummy driver configuration), `moon-power.sh` (docs/power.md) and
   `/data/adb/moon/config` (created with defaults, or old defaults migrated).
6. **Telephony**: `/opt/moon-telephony` and its start/stop scripts.
7. **Verification**: every installed file's sha256 against its build, and
   the config keys Moon needs.

Changes take effect at the next boot (`--reboot` reboots cleanly through
`moon-power.sh`).

## Not installed (kept as they are)

- Wi-Fi networks: joined from the System app (credentials stay in the
  root-only Wi-Fi service).
- The time zone: `scripts/a26-shell/set-timezone.sh`.
- Moon's settings (`/var/lib/a26-shell`): wallpaper, Auto-Lock, haptics,
  brightness, the light-sensor calibration.
- Alarms, timers and notifications (`/var/lib/moon-notify`).

## Turning Moon off

`touch /data/adb/moon/disabled` (as root) makes every later boot plain
Android; remove it to return to Moon. `/data/adb/moon/skip-once` skips the
next boot only. Holding a volume key during the first seconds of a boot does
the same for that boot.
