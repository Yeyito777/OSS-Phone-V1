# OSS-Phone-V1

An open-source phone operating system for the Samsung Galaxy A26. Android's
kernel and native hardware services stay underneath, but the display, input,
audio, and power policy are handed to a Linux userspace: an Alpine rootfs
running native Xorg and **Moon**, a touch-first phone shell and window manager
written in Rust (`a26-shell`).

<p>
  <img src="notes/ui-polish/after-lock.png" width="24%" alt="Moon lock screen">
  <img src="notes/ui-polish/after-home.png" width="24%" alt="Moon home screen">
  <img src="notes/ui-polish/after-system.png" width="24%" alt="System app">
  <img src="notes/ui-polish/after-keyboard.png" width="24%" alt="Moon keyboard">
</p>

Moon runs directly on the phone's native Xorg session. It currently
provides a lock screen with clock and passcode keypad, a home screen with
widgets and a dock, icon-zoom app transitions, an iPhone-style system keyboard,
external X11 app
lifecycle management, one-finger app-closing gestures, physical power/volume
key policy, and a root-only Unix-socket control interface over ADB.

> [!WARNING]
> This is research software, not a turnkey custom ROM. The reference setup uses
> an unlocked bootloader, Magisk, Samsung's downstream kernel, a separately
> prepared Alpine rootfs, and a native Xorg DRM/KMS takeover. Flashing or
> unlocking Samsung devices can erase data and permanently trip Knox.

## Highlights

- Rust 2024, toolkit-free UI using pure-Rust `x11rb` and a small anti-aliased
  software renderer with the Inter typeface (SIL OFL 1.1)
- XInput 2.2 raw-touch handling for the A26 touchscreen
- Salted PIN configuration with constant-time verification
- Power-button lock and panel-backlight control
- Separate-process app launch, fullscreen placement, close, and power-lock lifecycle
- App-provided artwork with an exact one-pixel launcher frame
- Static `aarch64-unknown-linux-musl` deployment binaries
- Root-only JSON IPC and deterministic ADB test controls
- X11-native screenshot capture independent of Android SurfaceFlinger

## Repository layout

```text
src/a26-shell/          Rust window-manager crate
src/moon-audiod/        Native AAudio sink and "permission" stand-in (no framework)
src/a26-audio-bridge/   Former Android AudioTrack bridge source
scripts/direct-boot/    Boot-ramdisk overlay: power-on straight into Moon
scripts/a26-shell/      build, install, lifecycle, IPC, and test helpers
notes/a26-shell/        curated screenshots and design verification
```

## Build

Host requirements include a current Rust toolchain, the
`aarch64-unknown-linux-musl` target, `aarch64-linux-musl-gcc`, ADB, `curl`,
`unzip`, and a JDK providing `javac`, `java`, and `jar`. The first audio-bridge
build downloads pinned Android platform and R8 inputs; subsequent builds use
the ignored local cache under `build/a26-audio-bridge/`.

```sh
scripts/a26-shell/build.sh
```

On macOS, install `brew install rustup zig`, `rustup target add
aarch64-unknown-linux-musl`, and `cargo install --locked cargo-zigbuild`. The
build then links through zig, and because the shell is Linux-only its tests are
cross-built and run inside the attached phone's chroot (`device-test.sh`).

Generated binaries and source archives are written under
`images/a26-shell-0.1.0/` and `images/a26-audio-bridge/` and are intentionally
ignored by Git.

## Device installation

The helper scripts expect the reference Alpine rootfs at
`/data/local/a26-linux`, authorized root access through
`/data/local/tmp/su`, and a working Xorg display `:0`.

If exactly one authorized ADB device is attached, it is selected
automatically. With multiple devices, set `A26_SERIAL` explicitly.

Provision the six-digit UI PIN without placing it in source or command-line
arguments:

```sh
read -rsp 'A26 Shell PIN: ' A26_SHELL_PIN; echo
export A26_SHELL_PIN
scripts/a26-shell/install.sh
unset A26_SHELL_PIN
```

The launcher’s System app is intentionally maintained and installed from its
own repository:

```text
https://github.com/Yeyito777/a26-system
```

Install `a26-system` first. It supplies both
`/opt/a26-system/bin/a26-system` and the launcher icon under
`/opt/a26-system/share/`.

To iterate on the shell itself, `scripts/a26-shell/deploy.sh [--reboot]`
rebuilds and replaces only the Moon binaries, keeping the phone's PIN config
and audio bridge.

Start just the shell when Xorg is already running:

```sh
scripts/a26-shell/start.sh
```

The `desktop-start.sh` and `desktop-stop.sh` wrappers use the included safe
native-Xorg handoff scripts. The prepared Alpine rootfs, stock-firmware-specific
Xorg configuration, Magisk installation, and Wi-Fi backend remain device setup
prerequisites rather than artifacts published in this repository.

## Default Moon boot session

Moon can be installed as the phone's autonomous default session while retaining
Samsung Android as the hardware bootstrap and recovery environment:

```sh
scripts/a26-shell/install-autostart.sh
scripts/a26-shell/autostart.sh status
```

With the direct-boot BOOT image (`scripts/direct-boot/`), Android's init still
brings the hardware up but the Java framework never starts: Moon is on screen
about 21 s after the kernel starts (Wi-Fi, network time and trimming unused
Android services follow in the background), and pressing a volume key while the boot
animation shows starts normal Android instead. Without that image, the Magisk
`service.d` entry waits for Android to finish booting, suspends the framework,
then performs the same proven DRM handoff. Either way the supervisor verifies
Xorg locally, starts native audio and Moon, and monitors the session without a
host computer. Android remains active
instead when the preceding boot was a kernel panic, required hardware is
missing, startup repeatedly fails, or the battery is too low. At runtime Moon
returns to Android charging mode at 8% rather than reaching PMIC undervoltage
shutdown.

To remain in Android for one boot, press either volume key during the eight
second post-boot override window, or prepare it from an authorized host:

```sh
scripts/a26-shell/autostart.sh skip-once
```

Persistent control is available with `autostart.sh enable|disable`. Disabling
while Moon is active also causes the device-local supervisor to restore Android
within its next 30-second health interval.

## Development control

```sh
scripts/a26-shell/ctl.sh state
scripts/a26-shell/ctl.sh lock
scripts/a26-shell/ctl.sh screen off
scripts/a26-shell/ctl.sh screen on
scripts/a26-shell/ctl.sh launch system
scripts/a26-shell/ctl.sh swipe-up
scripts/a26-shell/ctl.sh volume up
scripts/a26-shell/screenshot.sh
```

The control socket is root-owned and mode `0600`. The lock screen is a session
UI, not a cryptographic security boundary: an unlocked bootloader, root, or an
authorized ADB host can bypass it by design.

## Current limitations

- The initial external-app contract is intentionally small and System-specific;
  a general manifest/discovery protocol is the next launcher milestone.
- Touch taps and one-finger swipes work; multitouch/pinch policy is not yet
  implemented.
- Audio targets the built-in speaker through Android's native AudioFlinger/HAL
  services via AAudio (no Java framework); Bluetooth and USB audio routing are
  not yet exposed.
- The native Xorg/DRM takeover environment must be prepared separately.
- While Moon runs, USB exposes only adb: MTP/PTP file transfer needs Android's
  MediaProvider, which is stopped with the framework.

## License

MIT — see [LICENSE](LICENSE).
