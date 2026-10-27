# Display brightness and auto-brightness: Moon interface

Moon owns the panel backlight (screen-off/on transactions, power fades), so
brightness, including automatic brightness from the ambient light sensor, is
part of Moon. Clients (the System app's display settings, scripts) use Moon's
existing root-only control socket (`/run/a26-shell/control.sock`, one text
command per connection, JSON reply; `scripts/a26-shell/ctl.sh` over adb).

## Commands

| Command | Effect | Reply `result` |
| --- | --- | --- |
| `brightness` | read only | the `display` object below |
| `brightness set N` | `N` = 0–100, the user's brightness. With auto off it is the level. With auto on it re-anchors the automatic curve: at the current light, the level becomes `N`, and the curve keeps that offset as the light changes | `display` |
| `brightness auto on` / `brightness auto off` | turns automatic brightness on or off; turning it off keeps the current level as the manual level | `display` |

Changes apply immediately (with a short ramp) when the screen is on and are
remembered for the next screen-on when it is off. They persist across Moon
restarts and reboots.

## State

`state` (and every reply above) carries:

```json
"display": {
  "brightness": 42,
  "auto": true,
  "auto_offset": -6,
  "lux": 180,
  "sensor": "ok"
}
```

| Field | Meaning |
| --- | --- |
| `brightness` | 0–100: the level shown now (with auto on, the automatic level including the user's offset). `0` is the dimmest visible level, not off; the screen is turned off only by the power key / screen-off |
| `auto` | automatic brightness on |
| `auto_offset` | −50…+50, the user's adjustment of the automatic curve (0 when never adjusted) |
| `lux` | last ambient light reading in lux, or `null` (screen off, sensor unavailable, or no reading yet) |
| `sensor` | `ok`, `unavailable` (no light sensor found: auto behaves as manual), or `off` (screen off; the sensor is not running) |

The level maps to the panel backlight on a perceptual (square) curve, from 1
(level 0) to the panel maximum (level 100).

## Behaviour

- **Curve.** Interpolated in log-lux: 0 lx → 6, 10 lx → 18, 50 lx → 30,
  200 lx → 45, 1000 lx → 65, 5000 lx → 85, ≥ 20000 lx → 100, plus
  `auto_offset`, clamped to 0–100.
- **Smoothing and hysteresis.** Readings are averaged in log-lux. The level
  only follows when the light leaves a ±20 % band around the light it was
  last set for: getting brighter is followed after 1 s, getting darker after
  4 s (so a passing shadow or a hand does not dim the screen). Level changes
  ramp over about 0.5–1.5 s.
- **First reading.** On screen-on the last level is used until the first
  reading, which is then applied at once (no hysteresis).
- **Screen off costs nothing.** The light sensor is disabled and Moon reads
  nothing while the screen is off.

## Files

| Path (chroot) | Contents |
| --- | --- |
| `/var/lib/a26-shell/display.json` | `{"brightness": N, "auto": bool, "auto_offset": n}`, written atomically on change |

The sensor backend (Samsung sensor hub, `light_sensor`) is an implementation
detail of Moon and may change; clients use only the commands and state above.

## Status (2026-10-26): compensated readings, v1

The light sensor (AMS TCS3701) sits under the panel and measures the
screen's own light too; Android's sensors HAL holds its IIO device. Moon
therefore follows docs/auto-brightness-compensation.md (v1):

- **Readings:** the hub's lux from the kernel log (`light.rs`): the first
  three events after each switch-on (~0.1 s) and the hub's periodic line
  (~6 s). `sensor` is `unavailable` after 20 s without a reading.
- **Wake:** with auto on, the fade-in waits (≤ 0.3 s) for the first reading,
  taken with the panel dark: that is the ambient light, applied at once.
- **While lit:** readings are compensated for the panel's light,
  `leak = k · (backlight / max)^1.5`, with an uncertainty `0.3 · leak + 5`
  lx; brighter only on the low end of the estimate (and only if it holds at
  the brighter level's leak; at most once per 10 s), darker only on the high
  end, and not at all below 20 lx when the leak is over half the reading.
  `lux` in the state is the ambient estimate.
- **Self-calibration:** `k` (seed 110) is learned from wakes whose ambient
  light was the same at the wake and at the next screen-off (≤ 2 min later;
  the sensor is re-switched with the panel dark to read it), from readings in
  the first 30 s. Stored in `/var/lib/a26-shell/light-calibration.json`;
  every reading is logged to `/var/lib/a26-shell/light-log.csv` (1 MB,
  rotated).
- **Not yet:** screen-content terms (`Y_roi`, `Y_screen`), the dark spot
  over the sensor, and the faster `raw_data` path (spec §4, §6).
