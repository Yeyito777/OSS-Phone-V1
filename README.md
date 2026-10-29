# OSS-Phone-V1

An open-source phone operating system for the Samsung Galaxy A26. The OSS
Phone kernel (rebuilt from Samsung's official source, `scripts/kernel/`) and
Android's native hardware services stay underneath, but Android's Java
framework never starts: the display, input, audio, and power policy are handed
to a Linux userspace, an Alpine rootfs running **Moon**, a touch-first phone
shell and window manager written in Rust (`a26-shell`). Release notes:
[RELEASE-NOTES.md](RELEASE-NOTES.md).

<p>
  <img src="notes/ui-polish/after-lock.png" width="24%" alt="Moon lock screen">
  <img src="notes/ui-polish/after-home.png" width="24%" alt="Moon home screen">
  <img src="notes/ui-polish/after-system.png" width="24%" alt="System app">
  <img src="notes/ui-polish/after-keyboard.png" width="24%" alt="Moon keyboard">
</p>

Moon draws through its GPU compositor (`moon-compositor`) on a headless Xorg
session. It provides a lock screen with clock and passcode keypad, a home
screen with widgets, an app grid and a dock that the user rearranges as on iPhone (hold an
icon: it jiggles, drags and comes off; an App Library brings it back,
[docs/home-screen.md](docs/home-screen.md)), icon-zoom app transitions, an app switcher with
swipe-to-close, notifications (banners, a notification centre, the lock
screen), Control Centre, alarms and timers, a power menu, low-battery and
charging screens, an iPhone-style system keyboard, external X11 app lifecycle
management, physical power/volume key policy, and a root-only Unix-socket
control interface over ADB. The System, Phone, Messages and Exocortex apps come from
[a26-system](https://github.com/Yeyito777/a26-system); Exocortex, the
user's AI agent, runs on the phone itself ([docs/EXOCORTEX.md](docs/EXOCORTEX.md)):
hold the power key to talk to it ([docs/voice.md](docs/voice.md)).

> [!WARNING]
> This is research software, not a turnkey custom ROM. The reference setup uses
> an unlocked bootloader, Magisk, the OSS Phone kernel built from Samsung's
> source, a separately prepared Alpine rootfs, and a DRM/KMS display takeover.
> Flashing or unlocking Samsung devices can erase data and permanently trip
> Knox.

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
- GPU display path by default: `moon-compositor` owns the panel and shows
  each frame on the display planes, the Mali GPU or, failing both, the CPU,
  one frame per vblank at 120 Hz; the XRender path remains the automatic
  fallback (`notes/gpu/DESIGN.md`)

## Repository layout

```text
src/a26-shell/          Rust window-manager crate (Moon, moon-notifyd, tools)
src/moon-audiod/        Native AAudio sink and "permission" stand-in (no framework)
src/moon-gpu/           moon-compositor: KMS planes, GLES and CPU composition (NDK)
src/moon-telephonyd/    Telephony service over Samsung's radio HAL
src/a26-audio-bridge/   Former Android AudioTrack bridge source
scripts/install-all.sh  One idempotent installer for everything on /data
scripts/direct-boot/    Boot-ramdisk overlay: power-on straight into Moon
scripts/device/         Boot supervisor, power, audio, GPU and telephony session scripts
scripts/a26-shell/      build, install, lifecycle, IPC, and test helpers
scripts/gpu/            moon-compositor build, install, measurement and test helpers
scripts/kernel/         Reproducible OSS Phone kernel build from Samsung's source
scripts/native-init/    Stage 2 (OSS Phone's own PID 1), recovery slot only
docs/                   Protocols, settings, power, battery and install guides
notes/                  Design notes and on-device results
```

## Build

Moon needs a current Rust toolchain with the `aarch64-unknown-linux-musl`
target, and ADB. On macOS, install `brew install rustup zig`, `rustup target
add aarch64-unknown-linux-musl`, and `cargo install --locked cargo-zigbuild`;
the build then links through zig, and because the shell is Linux-only its
tests are cross-built and run inside the attached phone's chroot
(`device-test.sh`).

```sh
scripts/a26-shell/build.sh
```

The native parts (audio, the GPU compositor, telephony) need the Android NDK,
and the System app the musl cross toolchain; without them locally,
`install-all.sh` builds them on a build host (docs/INSTALL.md). The former
audio bridge (`build-audio-bridge.sh`) also needs a JDK; nothing installs it
any more.

Generated binaries and source archives are written under `images/` and are
intentionally ignored by Git.

## Device installation

Everything is installed by one idempotent script; see
[docs/INSTALL.md](docs/INSTALL.md) for the prerequisites (the reference
Alpine rootfs at `/data/local/a26-linux`, root through `/data/local/tmp/su`,
Magisk) and what it installs:

```sh
scripts/install-all.sh --dry-run
scripts/install-all.sh --a26-system ../a26-system [--reboot]
```

If exactly one authorized ADB device is attached, it is selected
automatically. With multiple devices, set `A26_SERIAL` explicitly.

The six-digit UI PIN comes from the `secrets` store (`A26_SHELL_PIN`), never
from source or a command line; it is only needed when the phone has no PIN
config yet, or with `--reset-pin`.

The System, Phone and Messages apps and the Wi-Fi backend are maintained in
their own repository, [a26-system](https://github.com/Yeyito777/a26-system),
which `install-all.sh` installs from a checkout (`--a26-system DIR`).

To iterate on the shell itself, `scripts/a26-shell/deploy.sh [--reboot]`
rebuilds and replaces only the Moon binaries, keeping the phone's PIN
config, and Moon runs the new build at once (`a26-shellctl restart`: no
reboot).

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
host computer. By default the display is composited on the GPU: the session
starts `moon-compositor` as the panel's owner, Xorg runs headless and Moon
draws through the compositor. If the compositor is missing or not ready, the
session uses Xorg on the panel (the XRender path) instead; if it fails at
runtime it is restarted, and after repeated failures the session falls back
to the XRender path in place. `scripts/gpu/install.sh --disable` opts out,
`--enable` opts back in (both take effect at the next boot). Android remains active
instead when the preceding boot was a kernel panic, required hardware is
missing, startup repeatedly fails, or the battery is at or below 5 % without
a charger. At runtime the supervisor owns the low-battery policy
(docs/power.md): warnings at 20 % and 10 %, then at 3 % a 15 s countdown and a
clean power-off unless a charger arrives; Moon does not fall back to Android
on low battery.

To remain in Android for one boot, press either volume key while the boot
animation shows (until it has been visible for 3 s, with the direct-boot
image; in the 8 s window after Android finishes booting, without it), or
prepare it from an authorized host:

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
scripts/a26-shell/ctl.sh layout reset
scripts/a26-shell/ctl.sh volume up
scripts/a26-shell/screenshot.sh
```

The control socket is root-owned and mode `0600`. The lock screen is a session
UI, not a cryptographic security boundary: an unlocked bootloader, root, or an
authorized ADB host can bypass it by design.

## Current limitations

- The app set is built in (System, Phone, Messages, Browser); a general
  manifest/discovery protocol is not done yet.
- No SIM has been tested: calls and SMS are tested only against a stand-in
  daemon, and VoLTE is not supported (docs/telephony-ui.md,
  notes/telephony/DESIGN.md).
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
