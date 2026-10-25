# A26 Shell design notes

A26 Shell is a minimal Rust phone shell/window manager targeting native Xorg on
the Samsung Galaxy A26 (SM-A266M).

## Current interface

The October 2026 UI polish (iOS-style lock screen, home screen, keyboard,
transitions, and System app) is documented with before/after captures in
[`../ui-polish/README.md`](../ui-polish/README.md). The list below describes
the earlier interface.

- numeric lock screen;
- minimal app-grid launcher with generated System artwork;
- exact one-physical-pixel app-icon frame;
- no on-screen lock button—the physical power key locks and blanks the panel;
- no gesture bar on the launcher;
- separately managed `a26-system` application with device/Linux information,
  live CPU/GPU utilization, and used/remaining filesystem space;
- bottom-edge gesture bar inside System, where swiping up closes the app;
- focus-independent physical volume keys, a global volume overlay, real
  AudioTrack-backed app audio, and root-only ADB/Unix-socket controls.
- global Moon-owned on-screen keyboard in a dedicated X11 window, with XTEST
  delivery to the focused managed app, an iPhone-compatible English (US)
  four-row layout, and a key-free bottom close zone.

## Captured output

These images were captured from the actual X11 shell window with the included
`a26-shellshot` utility rather than through Android SurfaceFlinger.

### Launcher

![Launcher](launcher-redesign.png)

### System app

![System app](system-view.png)

Pixel-level layout checks are recorded in
[`ui-redesign-verification.txt`](ui-redesign-verification.txt).

## Verified development behavior

- Xorg window size: 1080x2340
- XInput 2.2 raw touch begin/update/end decoding
- lock-screen keypad hit testing
- System app launch and bottom-edge close gesture
- normal X11 app window mapped fullscreen and terminated on close/power lock
- power-off lock, backlight zero, and lock-only wake
- safe WM restart while the panel is blanked
- root-only control socket and configuration permissions
- static aarch64-musl build
- keyboard layout/hit-test, state-transition, password-redaction, and lock
  gating unit tests
- autonomous Magisk late-start boot from a controlled full reboot: Android
  completed vendor initialization, the phone-local supervisor committed its own
  matching Xorg safety token, Moon reached its lock screen without a host start
  command, native Wi-Fi re-associated, and the persistent failure counter reset
  to zero
- autonomous audio boot from a controlled full reboot: the system-UID bridge
  was authorized before framework suspension, its PID/FIFO survived the handoff,
  Browser PCM reached a 48 kHz stereo AudioFlinger speaker track, and GPIO volume
  changes updated both Moon state and the bridge gain while Browser/System owned
  X focus
- Android fallback policy for a preceding kernel panic, three failed starts,
  explicit disable/skip-once markers, a post-boot volume-key override, and an
  8% low-battery threshold

The lock screen is intentionally only a session UI. The reference development
device has an unlocked bootloader, root, and authorized ADB, all of which can
bypass the shell.
