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
