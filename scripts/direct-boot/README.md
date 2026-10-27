# Direct boot: power-on straight into Moon

Android's init stays PID 1 and brings the hardware up exactly as stock does
(kernel modules, firmware, watchdog, vendor HALs, adbd/USB). In Moon mode the
Java framework never starts: zygote is stopped right after zygote-start, before
it can spawn system_server, and Xorg + Moon replace SurfaceFlinger as soon as
the panel is initialized.

## Pieces

| Where | File | Role |
| --- | --- | --- |
| boot ramdisk | `overlay.d/moon-direct-boot.rc` | appended to init.rc by magiskinit |
| boot ramdisk | `overlay.d/sbin/moon-boot-mode.sh` | post-fs-data: runs the policy (5 s timeout), sets `moon.boot.mode` |
| boot ramdisk | `overlay.d/sbin/moon-session.sh` | init service `moon-session`: runs the supervisor, falls back to Android |
| boot ramdisk | `overlay.d/sbin/moon-fallback.sh` | self-contained "start Android's framework" |
| /data/adb/moon | `moon-direct-policy.sh` | decides moon / android / legacy for this boot |
| /data/adb/moon | `moon-boot-supervisor.sh --direct` | display handoff, audio, Moon, liveness monitor |
| /data/adb/moon | `moon-post-ready.sh --direct` | after Moon is up: Wi-Fi, stopping unused services, network time |
| /data/adb/moon | `moon-mode-stop.list` | the Android services stopped in Moon mode |

The ramdisk part is deliberately small and fails toward stock Android; the
policy and supervisor live on /data so they can change without reflashing.
`build-boot-image.sh` (research host) adds only these four files to the proven
Magisk v30.7 BOOT image and verifies kernel and magiskinit are byte-identical.

## Boot flow (Moon mode)

1. Bootloader, kernel, first-stage init, magiskinit (unchanged).
2. `post-fs-data`: `exec moon-boot-mode.sh` → `moon-direct-policy.sh` prints
   `moon` unless: `DIRECT_BOOT=0` (→ legacy), charger boot, `disabled`,
   `skip-once`, recovery/download boot reason, previous kernel panic,
   `start-failures` ≥ `MAX_START_FAILURES` (3), battery < 20 % without power,
   or a required file is missing. It counts the attempt in `start-failures`.
3. `zygote-start`: stock `start zygote`, then `stop zygote zygote_secondary`.
   They stay disabled, so `class_start main` skips them.
4. `boot`: `sys.usb.config` becomes `mtp,adb`; the rc switches it to `adb`
   (MTP's FunctionFS daemon is MediaProvider, which never runs, and would keep
   the gadget from binding). adb is available from here on.
5. `moon-session` starts the supervisor in direct mode. Composer and
   SurfaceFlinger start as in stock and show the boot animation.
6. Volume-key window: from session start until the animation has been visible
   for `DIRECT_OVERRIDE_SECONDS` (3). Either volume key starts the framework
   instead (normal Android boot continues; prefer Volume Up: Volume Down held
   until the framework starts enters Android safe mode). Skipped after a
   deep-suspend resume, whose wake key can be a volume key.
7. Takeover (`a26-xorg-takeover.sh`): stop bootanim, SurfaceFlinger, composer
   (polling until init reaped them, no fixed sleeps); then, by default,
   `moon-gpu-session.sh start` brings up `moon-compositor` as DRM master and
   Xorg runs headless (`--gpu`); without a ready compositor, native Xorg on
   the panel as before. Native audio (`moon-permissiond` + `moon-audiod`)
   starts alongside Xorg, then Moon (with `MOON_GPU_SOCKET` in GPU mode).
8. Moon answers IPC → `start-failures=0`, `moon.session.ready=1`, and
   `sys.boot_completed=1` / `dev.bootcomplete=1` so vendor boot-completed
   actions run (battery driver, zram, thermal, lmkd) and init's critical-crash
   counting gets its normal time window. Kernel uptime here: ≈ 21 s.
9. Detached post-ready stage (`moon-post-ready.sh`), off the critical path:
   native Wi-Fi (autoconnects to saved networks), then 5 s and 60 s after
   ready `stop` every running service in `moon-mode-stop.list` (camera,
   Bluetooth, GPS, biometrics, Knox extras, telemetry, media/DRM, NPU,
   framework-only daemons; recorded with the boot ID in
   `/data/local/tmp/moon-boot/stopped-services`), and network time with
   ntpd once Wi-Fi has a default route (then every 12 h; RTC written in
   UTC). Kept: display, touch, audio, Wi-Fi, USB, charging, sensors, keys,
   thermal, keystore/TEE, and the whole cellular/RIL stack. It also runs
   and restarts moon-notifyd (notifications, alarms, timers;
   `docs/notifications-protocol.md`) for as long as Moon owns the phone.

## Fallbacks

* Policy missing/slow/failing → stock Android (unset mode = legacy takeover).
* Volume key in the window → framework starts; nothing else changes.
* Supervisor exits, or Moon not ready within 240 s → `moon-fallback.sh`.
* Session service dies while Moon owns the boot → rc starts `moon-fallback`.
* Moon dies later → one local restart, then restore Android (as before).
* Reset or hang before Moon is up → counted; after 3, Android until success.
* Previous boot was a kernel panic → one Android boot.

"Restore Android" (`a26-android-graphics-restore.sh`, `moon-fallback.sh`) is
now simply "start the framework": stop Moon's audio and permission stand-in,
start composer and SurfaceFlinger again, end the post-ready stage, start the
services it stopped, reset the RescueParty counters, start zygote.
`moon.boot.mode` becomes `android`.

All of these were exercised on the phone on 2026-10-25 (RESEARCH.md),
including the 240 s deadline and three consecutive failures.

## Flashing and measuring

* `flash-boot-inplace.sh IMAGE` writes a BOOT image from the running OS
  (checks, backups, verified read-back); see `notes/direct-boot/RESCUE.md`.
  Download mode is only for an image that does not boot.
* `measure-boot.sh [N]` reboots N times and prints the kernel uptime of each
  critical-path stage up to `moon.session.ready`.

## Controls

* `scripts/a26-shell/autostart.sh skip-once|disable|enable|status`
* `scripts/gpu/install.sh --disable|--enable`: `GPU_COMPOSITOR=0|1` in
  `/data/adb/moon/config`; GPU mode is the default. A compositor that dies is
  restarted twice in a row (the count starts over after 10 minutes of
  running), then the session returns to Xorg on the panel in
  place (`notes/gpu/DESIGN.md`). `--no-planes|--planes`: `GPU_PLANES=0`
  (or not), the GPU composes every frame without display planes.
* `/data/adb/moon/config`: `DIRECT_BOOT=0` returns to the legacy post-boot
  takeover without reflashing; `DIRECT_OVERRIDE_SECONDS`, `AUDIO_STREAM_INDEX`.
* Without the flashed ramdisk, everything here is inert and the Magisk
  service.d takeover runs as before.
