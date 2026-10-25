# a26-shell

`a26-shell` is the first native phone shell/window manager for the Galaxy A26
research target. Its core UI is toolkit-free: X11 protocol work uses pure-Rust
`x11rb`, and drawing uses X11 rectangles and an embedded bitmap font. Apps are
separate managed processes maintained in their own repositories.

Current features:

- fullscreen numeric lock screen;
- salted, hashed six-digit PIN configuration (no plaintext PIN in source);
- touch-first launcher containing separate **System** and **Browser** apps;
- app-provided System artwork with a one-pixel launcher frame;
- external `a26-system` process launch and fullscreen lifecycle management;
- external ARM64 vimbrowser launch and fullscreen lifecycle management;
- hidden X11 cursor on touch-only shell and app surfaces;
- a system-owned top safe-area bar that keeps apps below the camera cutout and
  shows Wi-Fi/battery on the left with Moon identity on the right;
- bottom-edge swipe-up app backgrounding with process and window preservation;
- system-owned global on-screen keyboard for managed apps, with text, URL,
  search, password, and number purposes;
- focus-independent physical volume-key handling, a global volume overlay, and
  real application-audio gain control;
- physical power-key lock/screen blanking with panel-backlight control;
- ordinary X11 `MapRequest`/`ConfigureRequest` handling;
- root-only Unix-socket IPC for state inspection and deterministic input;
- host-side build/install/test scripts over ADB.

The standalone System app lives at
<https://github.com/Yeyito777/a26-system>. It installs its executable and icon
under `/opt/a26-system`; the shell does not contain the System renderer or
telemetry implementation.

The standalone Browser app is vimbrowser. Its A26 installation provides
`/opt/vimbrowser-a26/bin/vimbrowser-a26` and
`/opt/vimbrowser-a26/share/browser-app.bgrx`; the shell only owns the launcher
tile and process lifecycle.

Moon keeps a lifecycle-aware registry for each app (`stopped`, `launching`,
`foreground`, or `background`). Swiping up unmaps the app's windows and returns
to the launcher without terminating its process. Reopening the app remaps the
same windows and preserves its PID and in-memory state. Before exec, Moon places
each app leader in `/dev/freezer/moon/<app>`; all later Chromium/helper children
therefore inherit the same isolated process-tree cgroup without a post-spawn
race. After Xorg receives the app's unmap requests, Moon freezes its group;
reopening first thaws the complete process tree and then remaps the same window.
This preserves in-memory state and the original PID while reducing ordinary
background CPU use to zero. Moon still terminates all registered children during
an intentional shell shutdown so an upgrade cannot leave unsupervised processes
behind.

Trusted root clients can temporarily keep a background process tree runnable
with `lease acquire <system|browser> <media|transfer> <seconds>`, and end it with
`lease release <system|browser> <media|transfer>`. Media leases are limited to
30 seconds and transfer leases to 120 seconds; clients renew while real work is
active. Expiry or release automatically refreezes a background app. Vimbrowser
playback also reaches this policy through the existing private PCM bridge: while
its stream contains audible samples, the bridge updates a content-free heartbeat
and Moon renews the same five-second Browser media lease. Pausing or ending audio
stops renewal and refreezes the hidden Browser after expiry. Lease state exposes
only the fixed purpose and remaining duration through `state`; no URL, title,
media metadata, or samples enter lifecycle state.

Every five seconds Moon reads `MemAvailable`. Below the larger of 384 MiB or ten
percent of physical RAM, it evicts one least-recently-used background app that
has no active lease. The complete freezer cgroup is killed, not merely its leader,
so Chromium helpers cannot survive unsupervised. Foreground and leased apps are
never candidates. The root-only `memory-pressure simulate` control runs the same
single-victim policy for deterministic device testing without allocating memory.

Moon's event loop blocks in `poll(2)` over X11, its root-only control socket,
power/volume input devices, and app pidfds. Timer wakeups are scheduled only for
keyboard repeat, gesture recovery, launch animation, status/heartbeat checks,
lease expiry, and UI deadlines. If an older kernel cannot create a pidfd, only
that process uses a bounded 250 ms status fallback. There is no idle 8 ms spin.

Screen-off is a device-local transaction. Moon first enters the lock view,
clears input/keyboard state, unmaps the active app, flushes Xorg, and verifies
that no app window remains active. Only then does it set panel brightness to zero
and send Samsung's touchscreen display-off event. Wake presents a complete lock
frame while dark before restoring touch and brightness. Any preparation or
hardware error rolls back to an awake locked state and is exposed as a fixed
error code in `state.suspend`, never as a half-completed transition.

By default that transaction is the whole screen-off: Moon, Xorg and apps stay
resident and the power key wakes the panel instantly. Deep suspend below is
opt-in with `"deep_suspend": true` in `config.json`, because its wake path is a
warm reboot.

With deep suspend enabled, after that transaction, and only when no
media/transfer lease is active, Moon hands control to a detached Android-namespace supervisor. It terminates the old
app session, disables Xorg's DSI output, and proves both DECON and DSIM runtime
suspended before selecting kernel `deep` mem sleep. This ordering is mandatory:
a direct `mem` request with Xorg's CRTC active was captured as a Samsung watchdog
reset in `pmucal_local_disable`/`blkpwr_dpu`.

The approved user wake source is the PMIC power key; RTC is enabled for Moon
alarms and the bounded proof command. Until the central alarm service in TODO 11
exists, the handoff clears any stale Android AlarmManager RTC alarm. Volume and
USB/USB-PD wake are disabled only for the deep interval and restored exactly
after resume, while charger/fuel-gauge safety sources remain untouched. Their
incidental wakeups are classified by PMIC/RTC IRQ deltas and immediately return
to deep sleep; only a physical power-key press or an explicitly armed Moon RTC
alarm proceeds to full wake. This firmware cannot re-enable the Xorg-disabled
DSI CRTC in place, and Exynos
DWC3 remains detached after configfs plus core/glue-driver resets. The supervisor
therefore persists the successful count/duration and performs a controlled warm
reboot. Magisk autonomous startup reconstructs Samsung display/USB state and
presents a fresh locked Moon session. The root-only `suspend test 2..30` command
uses a bounded one-shot RTC wake to exercise this same production path. Counts,
duration, inhibition, and fixed errors are reported in `state.suspend`. This is a
safe but intentionally slow wake boundary; in-place resume remains future work.

The lock screen is a UI/session lock, not a cryptographic security boundary.
The unlocked bootloader, Magisk root and authorized ADB can all bypass it by
design. IPC access is restricted to the root-owned chroot runtime.

Runtime paths on the phone:

```text
/opt/a26-shell/bin/a26-shell
/opt/a26-shell/bin/a26-shellctl
/opt/a26-shell/bin/a26-shellshot
/etc/a26-shell/config.json
/run/a26-shell/control.sock
/run/moon-audio/{pcm,volume,bridge.pid}
/run/moon-suspend/state
/data/adb/moon/{moon-suspend-cycle.sh,suspend.log}
/root/a26-shell.log
```

Useful host controls include `state`, `lock`, `screen on`, `screen off`,
`launch system`, `launch browser`, `keyboard show url`, `keyboard hide`,
`swipe-up`, `volume up`, and pointer/tap injection. They are exposed through
`scripts/a26-shell/ctl.sh` and the root-only Unix socket.
`scripts/a26-shell/screenshot.sh` captures the composed X11 root screen over ADB
without relying on Android SurfaceFlinger.

## Native-session audio

Samsung's AudioFlinger and audio HAL remain alive when Moon suspends the Android
Java framework, but a new AudioTrack cannot be authorized after PackageManager,
PermissionManager, and AudioService stop. The autonomous supervisor therefore
starts a small system-UID Java bridge before the DRM handoff. Its already-created
48 kHz stereo AudioTrack remains valid after `system_server` exits.

Linux applications write signed 16-bit PCM to the root/system-only
`/run/moon-audio/pcm` FIFO. Moon writes its 0–100 gain to the adjacent volume
file. The physical GPIO volume keys are read directly from `/dev/input/event0`,
so their behavior does not depend on which app or descendant X11 window has
focus. A dedicated override-redirect volume surface provides feedback above
both System and Browser. Browser's private rootfs sees only this FIFO; it still
does not receive the phone's `/dev/snd` or unsafe camera devices.

## On-screen keyboard protocol

The keyboard is Moon system UI, not part of Browser or System. It is a dedicated
lower-screen override-redirect X11 window above the active managed app. Moon's
existing XI2 raw-touch path consumes touches in the key panel before its normal
app tap forwarding. The bottom 180 physical pixels contain no keys and remain
available as the global swipe-to-background start zone.

English (US) uses the familiar four-row iPhone portrait arrangement: staggered
QWERTY letter rows, wider Shift/Delete controls, standard `123` and `#+=`
layers, a dedicated number pad, and contextual Done/Search/Go keys. URL entry
uses the Safari-style `123`, Space, `.`, Go bottom row. Moon preserves its own
visual language while scaling geometry proportionally from a 390-point iPhone
width reference. Apple does not publish fixed system-keyboard rectangles;
measurements and source references are recorded in
`notes/a26-shell/APPLE-KEYBOARD-LAYOUT.md`.

A touch-down immediately highlights and emits a character-producing key. Holding
Character, Space, or Delete repeats after 200 ms at 45 events per second, matching
the development workstation's `xset r rate 200 45` configuration. Sliding off
the original key clears its highlight and cancels further repeats. Shift,
layout-switch, and submit keys never repeat. The delay and rate live in the Moon
config as `keyboard_repeat_delay_ms` and `keyboard_repeat_rate_hz`. Repeats use
bounded, nonblocking XTEST batches so they cannot starve the shell's touch/IPC
loop. A ten-second stale-contact watchdog clears both XI2 and pointer ownership
if a device release event is lost.

The raw XI2 keyboard path tracks every touch ID independently. Holding one key
therefore does not serialize or suppress a second finger: the next key emits on
its own touch-down, highlights independently, repeats on its own schedule, and
can end while the first remains held. This is multi-touch key rollover; app and
global-close gestures intentionally remain single-finger. Apple documents
stable per-contact touch identities and simultaneous touch delivery, but does
not publish private system-keyboard rollover, grace-period, or hysteresis
timings, so Moon adds no arbitrary inter-key delay.

Managed apps request keyboard state with one command on the existing control
socket, read the JSON response, and close the connection:

```text
keyboard show text
keyboard show url
keyboard show search
keyboard show password
keyboard show number
keyboard hide
```

There is no keyboard event socket. Moon leaves X focus on the app and emits each
key through XTEST using the server keyboard and modifier maps. Runtime input is
strictly one-way: Moon tracks focus through FocusIn/FocusOut events on managed
app windows, preserves the exact focused descendant/transient, queues key-down
and key-up requests, and flushes. It never waits for an X11 reply or checked
cookie from its long-lived event-loop connection. This avoids 16-bit X11
sequence-epoch ambiguity after long sessions;
one such wrapped reply wait was captured live and had frozen the entire shell
while typing. The Browser target has a narrow A26-only CEF compatibility path
that turns the resulting raw printable key into the missing renderer CHAR event;
no typed value crosses IPC or enters a Moon buffer. A managed app can dismiss the
keyboard explicitly over IPC or by ending its editable-field focus; Moon also
hides it on every security/app transition.

App taps use XTEST pointer injection as well. This lets the X server hit-test into
embedded descendant windows such as Chromium's page surface instead of sending a
synthetic event only to the managed top-level. A show request is ignored unless
the screen is awake and a managed Browser/System window is active; lock,
screen-off, home, app close, and the global close gesture always hide the
keyboard.

`state` exposes only this keyboard metadata:

```json
{
  "visible": true,
  "purpose": "password",
  "shift": false,
  "layout": "letters"
}
```

Moon never builds a text buffer for app input. In particular, password keys are
resolved and injected one at a time, are never included in diagnostics, and are
never present in public state.

## App safe area

Managed apps begin at physical Y=124 rather than drawing underneath the A26's
camera cutout. Moon owns that 124-pixel strip as a separate override-redirect,
double-buffered X11 surface: Wi-Fi and battery remain in the left safe region,
`MOON` remains in the right safe region, and the center cutout region stays
visually quiet. App height is reduced exactly once. When the global keyboard is
visible, the app occupies Y=124 through the keyboard's physical top edge, so the
browser's bottom navigation continues to meet the keyboard with no gap.

Development lifecycle from the project root:

```sh
# Install/reprovision without writing the PIN into source or argv.
read -rsp 'A26 Shell PIN: ' A26_SHELL_PIN; echo
export A26_SHELL_PIN
scripts/a26-shell/install.sh
unset A26_SHELL_PIN

# Start the complete safe native session, inspect it, and return to Android.
scripts/a26-shell/desktop-start.sh
scripts/a26-shell/ctl.sh state
scripts/a26-shell/desktop-stop.sh
```

`install.sh` also installs the AudioTrack bridge and the chroot/runtime helpers
required by `desktop-start.sh`; enabling autonomous boot remains a separate,
explicit `install-autostart.sh` operation.

### Autonomous boot

The phone can boot into Moon without a host-side ADB command. Android and the
Samsung vendor services still start first because they initialize and decrypt
the hardware environment used by Xorg. A Magisk late-start service then hands
DRM/input/network ownership to the native session and launches Moon:

```sh
scripts/a26-shell/install-autostart.sh
scripts/a26-shell/autostart.sh status
```

Safety policy:

- wait for `sys.boot_completed`, `/dev/dri/card0`, and input initialization;
- remain in Android after a kernel-panic reset or after three failed starts;
- require at least 20% battery before takeover;
- restore Android charging mode at 8%;
- verify Xorg and Moon using phone-local status/control paths;
- probe Moon's control loop every 30 seconds with a five-second hard timeout;
- after two failed probes, save root-only process/kernel-stack diagnostics and
  restart Moon's recorded process group locally;
- permit one local recovery per boot, retain at most ten incident reports, and
  restore Android if Moon fails again or the bounded recovery does not start;
- authorize and verify the AudioTrack bridge before suspending system_server;
- clean native Wi-Fi before restoring Android;
- press a volume key during the eight-second override window to skip Moon once;
- use `autostart.sh skip-once|disable|enable` for explicit host control.
