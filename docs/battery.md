# Battery use in Moon mode (TODO 12)

Measured on the SM-A266M on 2026-10-26 with the current build: OSS Phone
kernel (`8c0461ee…`), direct boot into Moon, 36 unused Android services
stopped, screen-off through DPMS (panel and display pipeline powered down),
Wi-Fi associated with UofT (WPA2-Enterprise), moon-notifyd and moon-audiod
running, deep suspend **off** (the default).

## Method

The phone stays on USB, but the charger input is suspended
(`/sys/class/power_supply/battery/batt_slate_mode = 1`, with a phone-side
timer that always restores charging), so the battery supplies the whole
phone. After 15 s of settling, `current_now` is sampled every 5 s and
averaged. The battery was at 100 % (≈ 4.41 V). Hours are the rated
5000 mAh divided by the average current: an estimate that ignores the
voltage falling as the battery drains, so real runtimes are somewhat
shorter.

## Results

| Scenario | Average | Range | CPU busy | Estimated runtime |
| --- | --- | --- | --- | --- |
| Screen off (DPMS), locked, idle, Wi-Fi connected | **37 mA** | 22–66 | 1 % | ≈ 135 h (5.6 days) |
| Browser in the background (frozen), screen off | 38 mA | 22–59 | 1 % | ≈ 130 h |
| Audio playing through Moon (440 Hz tone, Moon volume 37), screen off | 69 mA | 59–90 | 7 % | ≈ 72 h |
| System app in front (mostly black UI), brightness 128 | 176 mA | 171–202 | 2 % | ≈ 28 h |
| Home screen, brightness 128 | 213 mA | 208–225 | 1 % | ≈ 23 h |
| Lock screen on, brightness 128 | 218 mA | 210–238 | 1 % | ≈ 23 h |
| Browser in front (white page, idle), brightness 128 | 259 mA | 253–277 | 1 % | ≈ 19 h |
| Home screen, maximum brightness (665) | 344 mA | 340–356 | 1 % | ≈ 14.5 h |

Brightness 128 is Moon's default (of 0–665).

## What it means

- **Screen off is the big win.** Before screen-off powered the panel down,
  the same idle state drew 193–218 mA (≈ 23–26 h); DPMS brings it to 37 mA.
  A frozen background app adds nothing measurable.
- **With the screen on, the display dominates** and CPU stays at 1–2 %. The
  panel is AMOLED, so content matters: the mostly black System app draws
  ≈ 40 mA less than the gradient home and lock screens, and a white web page
  ≈ 45 mA more. A darker wallpaper is the cheapest UI-side saving.
- **Audio** costs ≈ 32 mA over idle with the screen off (AudioFlinger, the
  speaker amplifier and moon-audiod's 7 % CPU).
- The remaining idle floor (37 mA) is the always-awake CPU and radios; the
  largest known consumer is the TEE worker activity from Samsung's
  `cass`/VaultKeeper polling (see `notes/direct-boot/RESEARCH.md`). The
  opt-in deep suspend removes it, at the cost of a warm reboot on wake
  (≈ 45 s); its wakes for alarms and timers come from moon-notifyd
  (`docs/notifications-protocol.md` §7).

Reproduce with the drain helper used here (a `slate mode` average over N
seconds): see `scripts/direct-boot/measure-drain.sh`.
