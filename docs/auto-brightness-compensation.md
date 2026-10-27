# Auto-brightness without the HAL: screen-light compensation in Moon

Spec for Moon (`src/a26-shell/src/light.rs`, `brightness.rs`). It replaces
"sensor unavailable" (`docs/display-brightness.md`, Status) with a working
light reading in Moon mode. No kernel, vendor_boot or HAL changes.

## Why Moon has to do it (measured 2026-10-26, OSS kernel, Moon, lock screen)

The light sensor (TCS3701) sits under the OLED and also measures the screen's
own light. Samsung removes that in the sensors HAL, which Moon does not use.
The hub does not do it:

| Backlight (Moon level) | Hub lux, settled | auto_brightness(37) lux | Notes |
| --- | --- | --- | --- |
| 1 (0) | 88–89 | 88–89 | the same room light throughout |
| 130 (44) | 99 | 99 | brightness 44 injected |
| 665 (100) | 196–202 | 196–202 | without injection |
| 665 (100) | 195–202 | 195–202 | brightness 100 injected |

- The panel adds about 10 lx at backlight 130 and about 110 lx at 665
  (roughly ∝ backlight^1.5; two points only) with the lock screen showing.
- Injecting the brightness into the hub (`/dev/shub_data_injection`) and
  sending LCD-on (`/dev/shub_sensorhub`) reaches the hub: it echoes the
  value in the AB event and changes the sensor's integration time (31 → 17 →
  3 ms for 1/44/100). It does **not** change the lux.
- The hub's auto-brightness sensor (type 37) reports exactly the light
  sensor's lux.
- Samsung's own driver switches to the front camera's lux when AB-lux is
  low while brightness is high (`camera_lux_en`), so even Samsung does not
  trust this sensor there.

Do not inject brightness: it only makes the sensor's integration time
depend on the level, which makes the readings harder to model.

## 1. Reading the sensor without IIO

The HAL keeps the IIO device open (EBUSY), but three paths work alongside it:

- **Enable:** `/sys/class/sensors/ssp_sensor/enable`, `"51"` on and `"50"`
  off, each in **one** `write(2)`. The node returns more bytes than written,
  so a write loop crashes (toybox `echo` dies with FORTIFY). The hub counts
  references, so this coexists with the HAL. Rate: the existing
  `/dev/batch_io` ioctl.
- **Hub lux, about every 6 s:** while any hub sensor is on, the hub's
  periodic debug work writes `[SHUB] light_sensor(5) : <lux>(<timestamp ns>)
  (…)` to kmsg. Moon follows `/dev/kmsg` (open, `lseek(SEEK_END)`, read
  records) and parses that line. This is the hub's own lux, and the
  ground truth for everything below.
- **Raw channels, at the sampling rate:**
  `/sys/class/sensors/light_sensor/raw_data` gives
  `r,g,b,c,atime,again` of the last event, readable at any time. The field
  meaning on this hub version is not yet verified: early events show
  shifted fields. Treat it as an optional faster path (§6) and fit it
  against the kmsg lux before using it.

v1 uses the kmsg lux. Its 6 s cadence fits the existing hysteresis (brighter
after 1 s, darker after 4 s), at the cost of a few seconds' lag. v2 adds
raw_data for 0.2 s response.

## 2. Model

```
measured_lux = ambient + leak
leak = g(bl) · (α · Y_roi + β · Y_screen)
g(bl) = (bl / bl_max) ^ γ          γ ≈ 1.5 (from 10 lx at 130 and 110 lx at 665), bl_max = 665
```

- `bl`: the backlight value Moon has written. Use the value in effect during
  the sample: the hub timestamp in the kmsg line tells which.
- `Y_roi`: mean linear luminance (0–1) of the pixels over the sensor (§4),
  from the frame being shown. `Y_screen`: the same over the whole screen.
  sRGB → linear, then `Y = 0.2126 R + 0.7152 G + 0.0722 B`.
- `α`, `β`: lux at full backlight per unit of content luminance. Seed:
  `α = 0`, `β` so that the lock screen at 665 gives 110 lx, i.e.
  `β = 110 / Y_screen(lock screen)`. Then learn both (§3).
- `ambient_est = max(0, measured − leak_est)`, with an uncertainty
  `u = 0.3 · leak_est + 5 lx`, used by the decision rules (§5).

Persist `α, β, γ` and the fit statistics in
`/var/lib/a26-shell/light-calibration.json` (atomic write, as
`display.json`).

## 3. Self-calibration on every wake (no user action)

Moon already ramps the backlight up on wake. Make the start of the ramp a
measurement:

1. **Power key pressed:** enable the sensor at once (before the panel is
   lit) and present the lock-screen frame at backlight 1.
2. **Hold the backlight at 1 until the first hub lux after the enable**,
   at most 300 ms. Then start the ramp as today. If nothing arrives,
   ramp anyway and skip this wake's calibration. At backlight 1 the leak is
   below 1 lx, so this first reading is the ambient light, `A0`. Use it as
   the "first reading" that `AutoBrightness::screen_on` applies at once.
3. **Keep `(bl, Y_roi, Y_screen, lux)` for each reading during the first
   30 s.** Each gives a residual `r = lux − A0` for the leak model.
4. **Accept the wake** only if the light did not change by itself: the
   last readings of the window, after subtracting the current leak
   estimate, are within ±15 % of `A0`. Also reject wakes where the
   proximity sensor reports near.
5. **Update `α, β`** by recursive least squares on `r = g(bl)·(α·Y_roi +
   β·Y_screen)`, with exponential forgetting (λ = 0.98 per accepted
   wake) and non-negative values. Re-fit `γ` from a log-log fit only once
   20 wakes span at least backlight 100–600.

Because the hub only reports every ~6 s, a wake yields two or three
residuals. Over a day of normal use that is tens to hundreds of
accepted points.

Optional, slower wakes: when the auto level is far above the dimmest, run the
ramp in two steps (to 25 %, hold 6 s, then the target) on one wake in ten.
That gives a mid-level point without touching the user's level.

## 4. Region over the sensor; dark pixels on it

- **Position.** The DT has `light-position = <44 623 10 265 3 0>` (units
  unknown). Find the sensor's spot from data instead. Moon computes
  `Y` per tile on a coarse grid (e.g. 9 × 20 tiles, from the GPU tier's
  downscaled frame or Moon's own drawing). For the first weeks, fit one
  `α_tile` per tile in the top third of the screen on accepted wakes,
  ridge-regularised. The tile with the largest weight is the sensor;
  freeze `Y_roi` to that tile and its neighbours.

  If an attended session is allowed later, a white square stepped across
  a black screen in a dark room finds it in about 30 s. It is not part of
  this spec.
- **Dark pixels over the sensor.** Once the spot is known, Moon keeps a
  small area over it (about 3 × 3 mm, around 25 px) black in every view it
  composites: status bar, lock screen, launcher, and an always-on-top dot
  over fullscreen apps. On an OLED, black pixels emit nothing, so the
  local term (`α`, normally the largest) drops to near zero and only the
  global scatter (`β`) remains. If the spot is inside the status bar this
  costs nothing visually; otherwise it looks like a camera hole. Measure
  `α` before and after: it is the success metric.

## 5. Decision rules: up-only while lit

The existing curve, smoothing, ±20 % band and ramp stay. Only the inputs
change:

- **Wake reading (`A0`, backlight 1):** trusted. It sets the level directly,
  as the first reading does today.
- **While the screen is lit:** compensated readings may **raise** the level
  only when `ambient_est − u > band_top` for the existing 1 s. They may
  **lower** it only when the measured lux minus the leak is below
  `band_bottom` even after adding `u` back, for the existing 4 s. Since `u`
  grows with the leak, a large or uncertain leak blocks lowering instead
  of dimming the screen by mistake.
- **No runaway.** A rise driven by lit readings is allowed only if
  `ambient_est − u` exceeds `band_top` at the **new** level's predicted
  leak too, i.e. the estimate must survive the larger leak it would cause.
  At most one such rise every 10 s.
- **Dark rooms.** Below 20 lx `ambient_est`, with the leak more than half
  of the measured lux, do not follow lit readings at all. Hold until the
  next wake or a reading at a lower backlight. This is where Samsung uses
  the camera.

## 6. v2: raw channels at 0.2 s

Log `raw_data` next to every kmsg lux for a few days. Fit
`lux ≈ k_c · c/(atime·gain) + k_g · g/(atime·gain)` (or whatever the
fields turn out to be) per gain range. If the fit stays within ±10 %,
use raw_data between kmsg lines for fast "brighter" reactions. The kmsg
lux stays the reference, and the leak model is unchanged (in lux).

## 7. Logging and acceptance

- Append `t, bl, Y_roi, Y_screen, lux, A0?, ambient_est, u, level` per
  reading to `/var/lib/a26-shell/light-log.csv`, capped at 1 MB, so the fit
  can be checked offline.
- **Acceptance:** over a week, for wakes that follow at least 2 min of lit
  screen, compare the `ambient_est` just before screen-off with the next
  wake's `A0` when the screen-off gap was under 60 s. Target median error
  ≤ 20 % above 50 lx.
- **Expected quality:**
  - fine in daylight (the leak is negligible above about 500 lx);
  - about 20 % indoors (50–500 lx);
  - unreliable below about 20 lx at high brightness, which the rules
    handle by holding.

## Implementation notes

- `light.rs`: replace the IIO reader with enable + kmsg follower (+ raw_data
  in v2). Keep stop-on-screen-off: the sensor costs nothing while off.
- `brightness.rs`: `AutoBrightness::reading` takes `(lux, bl_at_sample,
  Y_roi, Y_screen)` and applies §5. The wake hold (§3 step 2) is a new
  state before the first ramp step.
- The compositor exposes per-tile luminance of the current frame (cheap at
  9 × 20; update only when the frame changes).
- Health: if no kmsg lux arrives for 20 s while enabled, report
  `sensor: unavailable` as today.
