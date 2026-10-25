# Direct boot, stage 1: research findings (2026-10-24)

Device: SM-A266M, A266MUBU1AYB8, Android 15, Magisk v30.7.

## Native audio without system_server: works

Proven on the live phone with the framework stopped (and with an audioserver
that had restarted without the framework, i.e. the cold state a direct boot
has): an NDK AAudio stream from a root process plays through audioserver →
primary output → ABOX RDMA0 (`/proc/asound/card0/pcm0p` RUNNING, `hw_ptr`
advancing at 48 kHz) → speaker (device id 3), with working volume control.

Two framework duties had to be replaced:

1. **Package lookup.** `AudioFlinger::createTrack` always calls
   `afutils::checkAttributionSourcePackage`, which calls
   `PermissionController::getPackagesForUid` on system_server's `permission`
   service. libbinder waits up to 10 s for that service; audioserver's
   TimeCheck aborts createTrack after ~5 s (+ second chance), so **every new
   playback track crashes audioserver** when the framework is down. This also
   means the old Java AudioTrack bridge could never recover from an audioserver
   restart under Moon. `src/moon-audiod/moon-permissiond.c` registers a native
   `android.os.IPermissionController` (`permission`) that answers like Android
   does for service UIDs (uid < 10000 granted, others refused, `android` for
   uid 1000). With it, createTrack returns immediately.
2. **Stream volume.** A fresh audioserver has every stream's index range
   uninitialized (min/max -1, cur 0) and mixes at -inf dB until AudioService
   calls `initStreamVolume` / `setStreamVolumeIndex`. `moon-audiod` makes the
   same two calls through the firmware's libaudioclient (`audio-policy.cpp`):
   music 0–15, speaker index `AUDIO_STREAM_INDEX` (default 5 = -31 dB, the
   level Android had; index 8 measured -22 dB), plus Moon's 0–100 linear gain,
   matching the old bridge's `AudioTrack.setStereoVolume`. It repeats them
   whenever it reopens a stream, so audioserver restarts are survived.

AppOps (also system_server) is not needed: audioserver exempts service UIDs
("OpPlayAudio: not muting track ... for service UID 0"). A uid-1000 Java
AudioTrack, however, did block on AppOps, so moon-audiod runs as root.

ALSA/tinyalsa directly on ABOX was not pursued: the HAL already owns routing,
ABOX firmware and the speaker amp; the AAudio path keeps all of that.

Incident: discovering (1) crashed audioserver several times (init restarted it
each time; it is not `critical`). That killed Moon's Java bridge, so the live
session was moved to moon-audiod on the same FIFO. Because audioserver is an
"updatable" process, more than 4 crashes set `sys.init.updatable_crashing`,
whose only consumer is `flags_health_check UPDATABLE_CRASHING` (clears
server-pushed device_config flags).

## Keeping the framework from starting

* magiskinit appends `overlay.d/*.rc` to init.rc (before Magisk's own rc) and
  replaces `${MAGISKTMP}` (here `/debug_ramdisk`, where `overlay.d/sbin/*`
  lands). Appended actions run after init.rc's own action for the same trigger
  and before imported files'.
* A15 init.rc `on zygote-start` does `start zygote`/`zygote_secondary`
  unconditionally. Our `on zygote-start && property:moon.boot.mode=moon` stops
  them right after the fork; `stop` sets them disabled, `class_start main`
  (in `on nonencrypted`) skips disabled services, `onrestart` does not run.
  The only `restart zygote` users are `--only-if-running` (surfaceflinger) or
  `class_restart --only-enabled` (servicemanager).
* Trigger order: late-init queues early-fs … post-fs-data, load_bpf_programs,
  zygote-start, …, boot; mount_all queues `nonencrypted` (FBE); our property
  event is queued during post-fs-data, so `moon-session` starts after `boot`
  (≈ surfaceflinger time, ~12 s after kernel start).
* Rejected: `ro.zygote` games (no zygote service at all → no way to "start
  Android" without a reboot); overriding the zygote service definition with a
  gate binary (invasive, must mirror every option).

## Everything else that depended on the framework

* **USB:** `persist.sys.usb.config=mtp,adb`; Samsung binds the gadget when
  adbd's FunctionFS is ready, but `ffs.mtp` has no daemon without
  MediaProvider. The rc switches `mtp*`/`ptp*` configs to `adb` in Moon mode.
  UsbDeviceManager restores the user's mode when the framework starts.
* **boot_completed triggers** (vendor + system): battery driver
  `/sys/class/power_supply/battery/boot_completed`, ramplus zram swap,
  `vendor.thermal.bt_completed`, `lmkd --boot_completed`, pageboostd, tcp
  hystart, per_boot dir, sysinput/muic chowns, Magisk `--boot-complete`.
  Today's takeover already runs with all of these done. The direct supervisor
  sets `sys.boot_completed=1` once Moon is up. This also matters for init:
  before boot_completed, a `critical` service (servicemanager, lmkd, perfmond,
  ueventd) exiting more than 4 times reboots into the bootloader regardless of
  time; after it, only 4 crashes within the window do.
* **RescueParty:** never runs without system_server. The restore path resets
  both `sys.rescue_boot_count` and A15's `crashrecovery.rescue_boot_count`.
* **Magisk:** post-fs-data runs right after our policy; service.d runs at
  `nonencrypted` as before. The legacy supervisor exits when
  `moon.boot.mode` is set.
* **Storage:** Android has a lock credential (synthetic password with weaver),
  so CE storage stays locked until the user unlocks Android. Nothing in Moon,
  the chroot (/data/local, system DE) or the Wi-Fi backend uses CE paths; the
  current takeover already runs before any unlock.
* **Wi-Fi:** a26-system runs Alpine wpa_supplicant on `wlan0` from scsc_wlan
  (loaded by init, netdev present); it requires `framework-suspended=1`, which
  the takeover writes. Not yet verified on a boot where the framework never
  enabled Wi-Fi.
* **Time:** RTC only; no NITZ without the framework. Since 2026-10-25 the
  post-ready stage syncs with ntpd over Wi-Fi and writes the RTC (below).
* **Deep suspend:** resumes through a warm reboot; with direct boot that
  reboot lands in Moon in ~30 s and skips the volume window.
* **Watchdog:** kernel/init pet the hardware watchdog; system_server's
  Watchdog only watches itself.

## Results on the phone (2026-10-24, BOOT 3bf29162…e9f6)

Flashed with samloader 2.0.0 from macOS (libusb backend); the phone then
needed a manual restart (see RESCUE.md).

| Boot | adb up | Moon ready | kernel uptime at Moon ready |
| --- | --- | --- | --- |
| direct, restart from Download mode | 29.7 s | 48.9 s | ≈ 34 s |
| direct, `adb reboot` (×3) | 37.4–37.9 s | 56.4–56.9 s | 33.5–34.0 s |
| legacy (`DIRECT_BOOT=0`), `adb reboot` | 44.8 s | 81.3 s | — |
| Android (`skip-once`), `adb reboot` | 45 s | boot_completed 51 s | — |

Times from `adb reboot` include Android's shutdown, the bootloader and its
unlocked-bootloader warning. Kernel-time breakdown of a direct boot: zygote
forked 12.2 s and stopped at once (no ZygoteInit or system_server in logcat),
SurfaceFlinger 12.7 s, boot animation 13.1 s, supervisor ≈ 13 s, display
ready (animation + 3 s window) ≈ 18 s, Xorg exec 24.7 s, Xorg ready ≈ 32.6 s
(includes the Wi-Fi start the Xorg session runs before reporting ready),
Moon ≈ 34 s.

Verified over adb on direct boots: framework never started; `sys.usb.config`
adb with only `ffs.adb` + `ss_mon` bound, and a UDC unbind/rebind re-enumerated
with adb; tone through Moon's FIFO ran ABOX RDMA0 with a cold audioserver
(music index initialized 0–15 at 5) and Moon's volume keys changed the gain
file; Wi-Fi radio up and a scan found 7 networks; charging: usb online, Full,
+414 mA; zram swap on and `vendor.thermal.bt_completed=1` after the reported
boot completion; no crashes. Fallbacks: runtime `disable`, `stop moon-session`
(init rc fallback) and killing the supervisor (ramdisk wrapper fallback) each
started the framework (system_server in 29–46 s, keyguard focused, USB back to
`mtp,adb`); `skip-once` booted Android; `DIRECT_BOOT=0` restored the legacy
takeover.

## Follow-up on the phone (2026-10-25, same BOOT image)

All /data-side; Android's partitions are untouched.

**Boot time.** Kernel uptime at `moon.session.ready`, three `adb reboot`s each
(`scripts/direct-boot/measure-boot.sh`):

| | run 1 | run 2 | run 3 | adb reboot → ready |
| --- | --- | --- | --- | --- |
| before | 35.33 s | 35.76 s | 33.90 s | 57–59 s |
| after | 21.13 s | 21.34 s | 21.30 s | 43–44 s |

Before: display window 12.8 → 18.4 s, graphics handoff 18.5 → 24.2 s (5 s of
fixed sleeps), Xorg answering 25.6 s (1 s polls), then the Xorg session ran
the Wi-Fi start (≈ 8 s, mostly a26-wifi-stop forking `cat` for every /proc
entry) before reporting ready, audio ≈ 1 s, Moon. After: the handoff polls
init (≈ 0.7 s + 0.3 s for DRM release), Xorg answers at ≈ 20.5 s, audio has
already started alongside Xorg, Moon is ready ≈ 0.4 s later; Wi-Fi starts in
the post-ready stage (now 0.8 s with a26-system's faster stop script). The
3 s volume-key window after the boot animation appears is unchanged.

**Moon-mode service set.** `moon-mode-stop.list` stops 36 services (37 listed; `mediadrm` is not defined on this firmware) 5 s and
60 s after ready (init `stop`, so they stay down; the restore path starts the
same list again). Screen off, 60 s windows, minutes after boot:

| | init services running | processes | MemAvailable | CPU busy | battery (slate mode) |
| --- | --- | --- | --- | --- | --- |
| before | 105 | 617 | 6.446 GB | 1.67 % | −218 mA |
| after | 68 | 582 | 6.571 GB | 1.54 % | −210 mA |

`vaultkeeper`/`vaultkeeper_aidl` must stay: without them `cass` (vendor,
radio group) exits and init restarts it every 15 s. No critical-service
crashes, tombstones or watchdog events; audio (ABOX RDMA0 RUNNING), charging
and sensors unaffected. The idle CPU left is mostly TEEGRIS worker threads
(≈ 18 k context switches/min). That comes from a pre-existing loop: cass
calls VaultKeeper every few seconds, and VaultKeeper fails to reach PA
(`PA_TZ_LIB ... driver open`) whether or not `proca` runs. The trim does not
change it, and it is left alone as part of the RIL family.

**Battery, screen off.** Measured on battery by suspending the charger input
(`batt_slate_mode=1`, auto-reset by a phone-side timer): ≈ 193–218 mA.
Moon's screen-off only zeroes the backlight, so DECON/DSIM/DPP keep scanning
out at 120 Hz (≈ 190 IRQ/s each, plus `fast_switch_pos` and ACPM IPC). A test
modeset to the panel's 60 Hz mode failed (`Configure crtc 0 failed`) and
left the CRTC stopped: the drain fell to ≈ 6 mA, so the display pipeline is
≈ 185 mA of it. The CRTC could not be re-enabled in place (as for deep
suspend); a reboot restored the display. Never modeset Xorg's CRTC at
runtime on this firmware. A real screen-off needs DRM power-down that can be
re-enabled (kernel work) or the opt-in deep suspend, whose warm-reboot wake
is now ≈ 21 s. The Wi-Fi backend scanned every ~16 s even while connected;
after the a26-system fix: wlan IRQs 81 → 3/min, forks 166 → 68/min.

**Wi-Fi on UofT (WPA2-Enterprise).** a26-system branch `enterprise-wifi`
adds PEAP/MSCHAPv2 storing only the NT password hash
(`password=hash:`), with the server validated against the system CA bundle
and `domain_suffix_match="radius.wireless.utoronto.ca"`. Provisioned once
over adb stdin (`a26-wifi-ctl connect-enterprise`); it autoconnects at every
boot. Verified: EAP-PEAP SUCCESS, DHCP lease in policy table 1023, UofT DNS,
HTTP 204 and HTTPS; a wrong server domain is refused, then the backend falls
back to the saved network. No file on the phone contains the password.
Network time: first sync corrected a −185 s RTC drift; later boots sync about
8 s after Wi-Fi starts.

**Fallbacks**, with temporary hooks in the installed supervisor only (flag
files `test-never-ready` / `test-fail-early`, removed afterwards; the
installed supervisor is byte-identical to git again):

| Test | Result |
| --- | --- |
| Moon up but never ready | `ready-deadline` fallback 259 s after the policy ran; Android booted (system_server, boot_completed, USB `mtp,adb`); counter 1 |
| supervisor exits before ready | `supervisor-exited-before-ready`; Android in 80 s; counter 2 |
| next boot without hook | attempt 3/3 succeeded, counter reset to 0 |
| three failures in a row | counter 1, 2, 3; the 4th boot logged `failure limit reached (3/3)` and booted Android without trying Moon (54 s) |
| `autostart.sh enable` | counter cleared, next boot Moon |
| runtime `disable` after the trim | restore ended post-ready and restarted all 36 stopped services before the framework |

**In-OS flashing.** `flash-boot-inplace.sh` wrote the installed image over
itself: 131072 sectors to sda14, O_DIRECT read-back sha256 identical
(`3bf29162…e9f6`), the phone booted into Moon. RESCUE.md documents it as the
default flash path.
