# basic-os on the phone: power menu, battery screens, System pages (2026-10-27)

The device round for the `basic-os` merge (main `dfddd27`, a26-system
`d13e542`): Moon build `8c630dc` plus its last commit, GPU mode with display
planes (the default), OSS kernel, direct boot, phone on USB at 100 %.

## Method

- **Frame numbers** are Moon's own per-animation summary (`ctl state`,
  `animation`): back-to-back flip intervals from vblank timestamps, at
  120 Hz (8.30 ms).
- **Taps** went through the real touchscreen node (`sendevent` on
  `/dev/input/event3`). **Drags** went through Moon's control socket
  (`a26-touchpath`).
- **The power key** was pressed through the real key driver: `sendevent`
  `KEY_POWER` on `sec-pmic-key` (`/dev/input/event1`), which Moon reads
  with evdev.

## Frame times

| Animation | Runs | Frames | Missed vblanks | Interval mean / p95 / max (ms) |
| --- | --- | --- | --- | --- |
| Power menu opens (hold) | 3 | 65–70 | 1 | 8.43–8.44 / 8.30–8.32 / 16.57–16.80 |
| Power menu closes (Cancel) | 3 | 60–61 | 0 | 8.30 / 8.30–8.31 / 8.32–8.33 |
| Slider dragged halfway and released (spring back) | 3 | 65 | 4 | 8.82 / 16.50–16.57 / 16.59–16.67 |
| System → Battery page (push) | 1 | 45 | 0 | 8.29 / 8.30 / 8.31 |
| Battery → System (pop) | 1 | 46 | 1 | 8.48 / 8.45 / 16.59 |
| Charging card on the lock screen | 1 | 55 | 0 | 8.30 / 8.33 / 8.53 |

The slider's 4 misses come from the drag injection, not the menu. The
slider is a separate layer moved as a node, with nothing redrawn. Control
Centre drags (merged earlier) injected the same way miss as many, 3–5 of
91–95 frames over four runs.

## Behaviour checked

- **Hold timing on the real key** (threshold 0.9 s):
  - Presses held for 0.79 s and 0.93 s put the screen to sleep. These are
    the measured press-to-release times, which include `sendevent`'s
    overhead.
  - Holds of 1.2–1.3 s opened the menu. It was seen at 0.99–1.0 s, by
    polling `state` every ~0.1 s.
  - A short press while the menu is open closes it without sleeping.
- **Restart from the menu, twice:**
  - The slide to the lock screen on the next boot took 50–51 s.
  - Moon was ready at about 22 s of kernel uptime.
  - `moon-power.sh` logged the ordered stop and `sys.powerctl reboot`.
- **Low battery:** the supervisor's notifications were posted by hand with
  `moon-notifyctl`, exactly as `moon-boot-supervisor.sh` posts them.
  - `battery-low` at normal urgency: a banner that hides after 5 s.
  - `battery-low` at high urgency: a banner that stays.
  - `battery-critical` with `expires_at` 20 s ahead: a full-screen
    countdown. Withdrawing it returns to the app.
  - Letting it expire leaves *Shutting down…* on screen. After 30 s without
    a power-off, the phone carries on.
- **Charging card:** produced with `batt_slate_mode` 1 → 0, with the
  screen on and with the screen off. After the switch to power-supply
  uevents, the card and the screen wake followed the charger change within
  the next 1 s state poll. Before it, they waited for the 5 s status read.
- **System pages:**
  - Date & Time: 24-hour time and a time-zone change (to Bogotá) reached
    Moon's `state.time` and the status bar at once.
  - Storage: the count finished between 3 and 8 s after the page opened. A
    cold `du` of the Linux rootfs takes 1.3 s, 0.2 s warm.
  - About shows `OSS Phone` with the build's commit.
