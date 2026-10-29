# OSS Phone backlog

Open work after v1.0, roughly in priority order. Each item links the notes
where the work resumes. Finished items are at the bottom.

## Open

### Boot and platform
- [ ] **Stage 2 promotion: OSS Phone's own init in BOOT.** It passes end to
  end from the recovery slot; next is the recovery-slot soak, then the
  gated BOOT flash path. Needs the user's explicit approval.
  `notes/stage2/PROMOTION-PLAN.md`, `notes/stage2/SOAK-RUNBOOK.md`
- [ ] **oss-init: contained reboot vs. power-off from non-PID-1 processes**
  (shared `sys.powerctl` reason channel). `notes/stage2/PROMOTION-PLAN.md`
- [ ] **Patched `mali_kbase` in vendor_boot** (needed only once stage 2 is
  in BOOT; needs a vendor_boot flash path and an attended test).
  `scripts/kernel/patches/README.md`

### Telephony
- [ ] **Test with a real SIM:** SIM status, signal, SMS, calls with audio and
  mobile data. Needs a physical SIM; this A26 has no eSIM.
  `notes/telephony/DESIGN.md`
- [ ] **VoLTE** (parked): Samsung's IMS client is a framework app, so native
  VoLTE needs its own IMS client. Matters before Freedom's 3G shutdown
  (2027-03-01). `notes/telephony/DESIGN.md` §4
- [ ] **Missing phone features:** contacts, composing in Messages, SIM PIN
  entry, screen-off at the ear (no proximity sensor; the touch controller's
  ear-detect mode is untested).

### Display and input
- [ ] **Turn auto-brightness on.** v2 compensation is built; it needs real
  usage data, then the acceptance check (median error <= 20 % above
  50 lux), then `AUTO_READY = true` and default on.
  `docs/display-brightness.md`, `docs/auto-brightness-compensation.md`
- [ ] **Multitouch and pinch** (only taps and one-finger swipes today).
- [ ] **Compositor speed-ups** measured with the Wayland demo in front:
  occlusion culling and staged uploads (`gpu-compositor` branch).
  `notes/gpu/DESIGN.md`

### Apps and UI
- [ ] **Browser GPU acceleration** (parked): Venus renders on the Mali, but
  Chromium still refuses it. `notes/gpu/BROWSER.md`
- [ ] **UI follow-ups** on branch `basic-os-followups`: DST-aware time zone
  offsets, `ctl power menu` counting for Auto-Lock, hiding Charge Cycles.
- [ ] **Fonts:** no CJK, Arabic, Hebrew or Indic coverage. `docs/fonts.md`
- [ ] **Haptics:** the coin motor is faint, so tap haptics ship off; revisit
  the pulse lengths with hands on the phone.
- [ ] **Home screen editing follow-ups:** a *Reset Home Screen Layout* entry
  in the System app (a26-system; Moon's side is `layout reset`), jiggling
  and removable widgets, more than one home page (page dots, dragging an
  icon to the next page), App Library search and categories.
  `docs/home-screen.md`

### Wayland (parked, opt-in)
- [ ] Real-finger check on Xwayland, then decide whether Xwayland becomes the
  default; the slow paint into dma-heap buffers; rootless Xwayland; the
  Browser as a native Wayland app. `notes/wayland/MOON-WAYLAND.md`
  ("Where to resume")

### Audio and connectivity
- [ ] Bluetooth and USB audio (built-in speaker only today).

### Exocortex (post-v1.0, `docs/EXOCORTEX.md`)
- [ ] **Exocortex under Android's init** (`scripts/device/exocortex/proposed/exocortex.rc`):
  an overlay.d rc so init itself supervises the keeper. Needs a BOOT image
  rebuild and the user's explicit approval; the keeper pair does the job
  from /data meanwhile.
- [ ] **Network in Android mode:** after a fallback Android had no Wi-Fi of
  its own (no saved network), so the daemon waited offline until Moon was
  back. Check DNS and routing for the daemon once Android has a network.
- [ ] Exocortex app: the rare partial frame over 8.33 ms while a long reply
  re-wraps (16 of 1,011 in the device run).

### Security
- [ ] The lock screen is a session UI, not a security boundary: root or an
  authorized ADB host can bypass it.

## Done

1. [x] Replace per-app child slots with a lifecycle-aware application registry.
2. [x] Make swipe-up hide and background apps instead of terminating them.
3. [x] Assign each app process tree to an isolated freezer cgroup.
4. [x] Unfreeze and remap background apps for instant reopening.
5. [x] Add bounded background-execution leases for media and transfers.
6. [x] Connect Vimbrowser media playback state to Moon’s lease protocol.
7. [x] Add memory-pressure eviction of least-recently-used background apps.
8. [x] Replace Moon’s idle 8 ms polling with blocking event waits.
9. [x] Add a device-local suspend coordinator with safe lock, display, and app sequencing.
10. [x] Enter deep suspend on screen-off and wake safely from approved hardware sources.
11. [x] Add a central alarm and notification service with RTC wake scheduling (moon-notifyd, `docs/notifications-protocol.md`).
12. [x] Measure and document foreground, background, audio, and suspended battery use (`docs/battery.md`).
13. [x] Exocortex on the phone (after v1.0): the daemon in the chroot with a keeper that survives Moon, Xorg, chroot-session teardown and the Android fallback, a graceful stop in the power-off sequence, OpenAI by API key and Claude through whale's link-only /proxy, the Exocortex app and its notification bridge (`docs/EXOCORTEX.md`, a26-system `docs/exocortex.md`).
