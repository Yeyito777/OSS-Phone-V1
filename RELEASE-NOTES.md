# OSS Phone v1.0: release notes

Device: Samsung Galaxy A26 (SM-A266M), firmware A266MUBU1AYB8.
Repositories: OSS-Phone-V1 (this one) and [a26-system](https://github.com/Yeyito777/a26-system)
(the System, Phone and Messages apps and the Wi-Fi backend).

> [!WARNING]
> Research software, not a turnkey ROM: it needs an unlocked bootloader,
> Magisk root and a separately prepared Alpine rootfs. Unlocking can erase
> data and permanently trips Knox.

## What OSS Phone is

- **Direct boot into Moon.** Android's init still brings the hardware up as
  stock does (modules, firmware, vendor HALs, adbd), but the Java framework
  never starts: zygote is stopped before it can spawn system_server, and
  Moon replaces SurfaceFlinger once the panel is up. Moon is a touch-first
  phone shell and window manager written in Rust, running in an Alpine Linux
  userspace (`scripts/direct-boot/README.md`).
- **36 unused Android services are stopped** once Moon is up (camera,
  Bluetooth, GPS, biometrics, telemetry, …); display, touch, audio, Wi-Fi,
  USB, charging, sensors, thermal, keystore/TEE and the cellular stack stay
  (`notes/direct-boot/RESEARCH.md`).
- **The OSS Phone kernel**, built from Samsung's official source release for
  this firmware (`SM-A266M_LA_15_Opensource.zip` from
  opensource.samsung.com) with the stock toolchain and config. It adds
  `CONFIG_PID_NS` and `CONFIG_DEVTMPFS` and drops DEFEX. All 306 vendor
  modules still load: their imports, CRCs and vermagic all match, and 274
  rebuild byte-identical (`scripts/kernel/README.md`).
- **Android stays reachable:** a volume key during the boot animation, or
  `skip-once` / `disabled` in `/data/adb/moon`, boots stock Android. The
  system, vendor and product partitions are never modified.

## Features

### Everyday use
- **Lock screen and passcode:** clock, date, charging line and unseen
  notifications. The six-digit PIN is stored as a salted hash and checked in
  constant time.
- **Home screen and dock:** widgets, and a dock with Phone, Browser,
  Messages and System. Apps open and close with an icon zoom.
- **App switcher:** swipe up from an app, scroll the cards, and throw a card
  away to close its app.
- **Finger-following gestures at 120 Hz:** swipe-up, the switcher, the lock
  screen, and curtains whose edge follows the finger.
- **Page slides** in the System, Phone and Messages apps: iOS-style 380 ms
  push and pop, composed by Moon (`docs/page-transitions.md`).
- **Notifications** (`docs/notifications-ui.md`):
  - Banners over the home screen and apps: tap to open, swipe up to hide.
    Normal ones hide after 5 s; urgent ones stay.
  - A notification centre pulled down from the top edge, grouped by app,
    with swipe-to-dismiss and Clear.
  - Notifications on the lock screen.
- **Control Centre** (from the top right): Wi-Fi, Focus (Do Not Disturb),
  the flashlight, the next alarm, and brightness and volume sliders.
- **Alarms and timers:** they ring full screen, from the lock screen or
  from screen-off, and survive screen-off (moon-notifyd keeps the RTC
  programmed).
- **Power menu:** hold the power key for 0.9 s for *slide to power off*,
  Restart and Cancel. Moon shows *Shutting down…* or *Restarting…*, then the
  device side shuts down in order (`docs/settings.md`, `docs/power.md`).
- **Battery and charging** (`docs/power.md`):
  - Warnings at 20 % and 10 %.
  - At 3 %, a full-screen countdown before a clean power-off; plugging a
    charger in cancels it.
  - Plugging in wakes the lock screen with a charging card.
  - The bolt shows in the status bar.
  - Off-mode charging uses Samsung's own charging screen.

### Settings (the System app)
- **Wi-Fi:** scan, join, and saved networks that reconnect. Supports WPA
  Personal and WPA2-Enterprise (PEAP/MSCHAPv2, e.g. eduroam), with the
  server-certificate name field. Credentials go straight to the root-only
  Wi-Fi service and are never logged.
- **Display:** brightness. **Wallpaper:** six built-in wallpapers, including
  true black. **Haptics:** a switch (below).
- **Date & Time:** 24-hour time, and a time-zone picker (88 zones, rules
  from tzdata 2026b) that every clock follows at once.
- **Battery:** charge, an estimate from the measured drain, health,
  temperature, current, voltage and capacity.
- **Storage:** OSS Phone, its data, Android and other, and free space.
- **About:** the OSS Phone version and build, the device, Alpine release,
  kernel, memory and display. Restart and Power Off rows open the power
  menu.

### Phone and Messages, without a SIM
The phone has no SIM yet, so these run in no-SIM mode
(`docs/telephony-ui.md`):
- The status bar shows `No SIM`, or `SOS` when emergency calls are possible.
- Phone keeps its keypad and call history but cannot place calls.
- Messages says why it is empty.
- The call screens are complete (slide to answer, call waiting, mute,
  keypad, speaker, a tucked-away in-call pill). They are tested only
  against a stand-in daemon.

### Haptics (off by default)
- **Tap haptics** (keyboard keys, Control Centre, switches) are off by
  default, because the A26's coin motor is too faint for crisp taps. Turn
  them on in System → Haptics.
- **The switch covers taps only:** the wrong-passcode buzz plays either way
  (`docs/settings.md`), and ringing and notification vibration come from
  moon-notifyd, not the switch.

## Under the hood

- **GPU compositor** (`notes/gpu/DESIGN.md`):
  - `moon-compositor` owns the panel as DRM master. It shows each frame on
    the display planes, on the Mali-G68 through GLES, or, failing both, on
    the CPU: one frame per vblank at 120 Hz.
  - Automatic fallbacks: a compositor that dies is restarted twice, then
    the session returns to Xorg on the panel in the same boot, with no
    reboot. Without a ready compositor, the boot uses that path from the
    start.
- **Native audio** with no framework (`src/moon-audiod/`): AAudio into
  Android's AudioFlinger and audio HAL, with stand-ins for the permission
  and audio services.
- **moon-notifyd** (`docs/notifications-protocol.md`): notifications, alarms,
  timers, alert sounds, the vibrator and the RTC wake.
- **moon-telephonyd** (`docs/telephony-protocol.md`): SIM, network, signal,
  SMS, calls and mobile data, through Samsung's radio HAL with no framework.
- **DPMS screen-off:** the panel and display pipeline power down, and the
  panel is back 60–110 ms after the request (`notes/direct-boot/RESEARCH.md`).
- **The boot supervisor's fallbacks** (`scripts/direct-boot/README.md`):
  - A failed or slow policy, or Moon not ready within 240 s, starts
    Android's framework.
  - Three failed starts in a row give Android until one succeeds.
  - A kernel panic gives one Android boot.
  - Moon dying gets one local restart.
  - A hung Moon is diagnosed and restarted.
- **In-OS flashing:** `scripts/direct-boot/flash-boot-inplace.sh` writes a
  BOOT image from the running OS, with checks, a backup and a verified
  read-back. Download mode is only needed for an image that does not boot.
- **`scripts/install-all.sh`:** one idempotent installer for everything on
  `/data`, with `--dry-run` and sha256 verification of every file
  (`docs/INSTALL.md`).

## Measured

All measured on the A26; the source document is given for each.

**Boot** (`notes/direct-boot/RESEARCH.md`, three `adb reboot`s): Moon is
ready at **21.1–21.3 s** of kernel uptime, **43–44 s** after `adb reboot`.
That includes Android's shutdown, the bootloader and its
unlocked-bootloader warning. In GPU mode Moon was ready at 22 s of uptime,
and the compositor 108 ms after it started (`notes/gpu/DESIGN.md`).

**Battery** (`docs/battery.md`):
- Method: on battery through `batt_slate_mode`, brightness 128 (Moon's
  default, of 0–665), Wi-Fi connected.
- Runtime is the rated 5000 mAh divided by the average current, so real
  runtimes are somewhat shorter.

| Scenario | Average | Estimated runtime |
| --- | --- | --- |
| Screen off, locked, idle | **37 mA** | ≈ 135 h (5.6 days) |
| Browser in the background (frozen), screen off | 38 mA | ≈ 130 h |
| Audio playing, screen off | 69 mA | ≈ 72 h |
| System app in front | 176 mA | ≈ 28 h |
| Home screen | 213 mA | ≈ 23 h |
| Lock screen on | 218 mA | ≈ 23 h |
| Browser in front (white page, idle) | 259 mA | ≈ 19 h |
| Home screen, maximum brightness | 344 mA | ≈ 14.5 h |

**Screen-off:** before screen-off powered the panel down (DPMS), the same
idle state drew 193–218 mA; it now draws 37 mA (`docs/battery.md`). In GPU
mode it measured 36 mA, against 40 mA on the software path, over 150 s
windows (`notes/gpu/DESIGN.md`).

**Frame times** (`notes/gpu/DESIGN.md`):
- Method: Moon's own per-animation summaries from vblank timestamps, with
  the same injected 120 Hz touches on both display paths.
- Over all gesture runs in GPU mode: about 900 frames with 1 missed vblank.

| Transition | Software path (XRender) | GPU mode |
| --- | --- | --- |
| Swipe up out of an app | 53 frames, 3 missed, 10 paced at 60 Hz | 82–83 frames, 0–1 missed |
| Switcher scroll | 78–84 frames, 1–2 missed | 81–85 frames, 0 missed |
| Switcher back | 72–86 frames, 0–4 missed | 85 frames, 0 missed |
| Notification centre in / out | 88–90 frames, 0 missed | 87–90 frames, 0 missed |

**Display planes vs. GPU drawing** (`notes/gpu/DESIGN.md`):
- Planes took 704 of 1311 gesture flips.
- Scrolling the switcher continuously:

| Path | Current | CPU |
| --- | --- | --- |
| Software path | 443 mA | 15 % |
| GPU-drawn | 337 mA | 10 % |
| Planes | **273 mA** | 7 % |

**Power menu, battery screens and System pages**
(`notes/ui-polish/BASIC-OS-DEVICE-ROUND.md`, the same per-animation
summaries):

| Animation | Frames | Missed vblanks |
| --- | --- | --- |
| Power menu opens / closes | 65–70 / 60–61 | 1 / 0 |
| System page push / pop | 45 / 46 | 0 / 1 |
| Charging card | 55 | 0 |

- A restart from the menu reached the lock screen 50–51 s after the slide.
- On the real key, holds opened the menu at 0.99–1.0 s (the threshold is
  0.9 s); presses of 0.79 s and 0.93 s put the screen to sleep.

## Install

See [docs/INSTALL.md](docs/INSTALL.md):

```sh
scripts/install-all.sh --dry-run
scripts/install-all.sh --a26-system ../a26-system --reboot
```

The direct-boot BOOT image is flashed separately, under the rules in
`scripts/direct-boot/README.md` and `notes/direct-boot/RESCUE.md`.

## Known limits

- **No SIM, calls or SMS tested.** The phone has had no SIM, so the call
  screens and SMS are tested only against a stand-in daemon. A proximity
  sensor, contacts, composing in Messages and SIM PIN entry are not done.
- **VoLTE is parked.** On this Samsung the IMS client is a framework app,
  and the framework does not run in OSS Phone. Without IMS, only CS voice
  (3G, or CSFB from LTE) and SMS over the CS domain can work.
  Native VoLTE needs its own IMS client and is a research item
  (`notes/telephony/DESIGN.md` §4).
- **Auto-brightness is not yet enabled.** It ships off. The light sensor
  under the panel also measures the screen's own light; Moon's compensation
  (v1) works but still needs its fit refined
  (`docs/display-brightness.md`, `docs/auto-brightness-compensation.md`).
- **The Browser is CPU-rendered.** Chromium stays on SwiftShader, and GPU
  work on it is parked (`notes/gpu/BROWSER.md`). It also has no page slides.
- **Wayland and Xwayland are opt-in** and parked: native Wayland apps
  (`WAYLAND_APPS=1`) and Xwayland in place of headless Xorg (`XWAYLAND=1`).
  The default session is moon-compositor with headless Xorg
  (`notes/wayland/MOON-WAYLAND.md`).
- **Stage 2 runs only from the recovery slot.** OSS Phone's own init as the
  real PID 1, with Android contained, passed end to end from the recovery
  slot. Promotion to the BOOT partition is planned, but not built or
  enabled (`notes/stage2/PROMOTION-PLAN.md`).
- **The vibration motor is faint.** Short taps are barely felt, which is why
  tap haptics are off by default.
- **Also:**
  - Touch handles taps and one-finger swipes; there is no multitouch or
    pinch.
  - Audio is the built-in speaker only (no Bluetooth or USB audio).
  - Fonts have no CJK, Arabic, Hebrew or Indic coverage (`docs/fonts.md`).
  - The lock screen is a session UI, not a security boundary: root or an
    authorized ADB host can bypass it.

The live backlog, with where each item resumes, is [TODO.md](TODO.md).

## Credits and licences

- **OSS-Phone-V1 and a26-system:** MIT (`LICENSE` in each repository).
- **Inter** (the typeface of Moon and the System, Phone and Messages apps):
  SIL Open Font License 1.1.
- **Noto fonts** for fallback (Noto Sans, Noto Sans Symbols and Symbols 2,
  Noto Sans Math, Noto Color Emoji): SIL Open Font License 1.1. The licence
  texts ship beside them, and the files are pinned by SHA-256 in
  `scripts/a26-shell/fonts.manifest` (`docs/fonts.md`).
- **The kernel:** Samsung's GPL source release for SM-A266M
  (opensource.samsung.com), rebuilt reproducibly by `scripts/kernel/`.
- **Alpine Linux** userspace (Xorg, Python, `wpa_supplicant` and others),
  under their own licences.
- **Samsung's vendor HALs and Mali driver** on the phone are used as
  installed and not redistributed here.

## Soak results

24 hours on the release candidate (main `d8de466`, the same code as
v1.0: since then only `VERSION`, docs and the soak-report tool changed). Run `20261027-045519` of
`scripts/a26-shell/soak.sh`, analysed with `scripts/a26-shell/soak-report.py`:
**passed**, with no drift, restarts or failed events.

- **No restarts.** Moon, moon-compositor, Xorg, moon-notifyd,
  moon-telephonyd, moon-audiod, moon-permissiond, the Wi-Fi service and
  wpa_supplicant kept one process each for the whole run. Only `udhcpc`
  restarted, once for each Wi-Fi rejoin.
- **Events, all passed:**
  - 71 of 71 screen-off/on cycles.
  - 3 of 3 silent alarms.
  - 4 of 4 Wi-Fi drops and rejoins.
- **Memory (RSS):**
  - Xorg: 40 MB, with a one-time step to 50 MB at 11 h, then flat.
  - Moon: 120 → 125 MB, slowing. That is +3 MB in the first 6 h, then
    about +1 MB per 6 h.
  - moon-compositor: 34 → 35 MB.
  - The other services stayed flat, and so did file descriptors (at most
    one or two more per process).
- **Idle CPU** (all cores): 2.19 % with the screen off, 2.47 % with it on.
- **Screen-off drain on battery**, over 7.1 h unplugged: median 30 mA, mean
  42 mA. That is about 5 days of standby, assuming 5000 mAh.
- **No errors in the logs** during the run: the kernel log, Moon's session
  logs and logcat.
