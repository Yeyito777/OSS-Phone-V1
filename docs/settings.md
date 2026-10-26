# Moon settings: wallpaper and Auto-Lock

Moon owns the user's appearance settings and applies them; other programs
(the System app, scripts) change them through Moon's control socket, never
by writing the file. Brightness and auto-brightness are a separate store
with their own commands: see `display-brightness.md`.

## Commands (`/run/a26-shell/control.sock`, one per connection)

| Command | Effect |
| --- | --- |
| `settings set wallpaper ID` | shows a built-in wallpaper (below) on the lock and home screens |
| `settings set auto_lock_s N` | Auto-Lock after `N` seconds untouched: `15`, `30` (default), `60`, `120`, `300`, or `0` (never) |

An unknown key or value is refused (`ok: false`); nothing changes. Every
reply and `state` carry the current settings:

```json
"settings": {"version": 1, "wallpaper": "moon", "auto_lock_s": 30}
```

## File

`/var/lib/a26-shell/settings.json` (chroot path; `MOON_SETTINGS` overrides it
off the phone), the same object, written atomically (temporary file, then
rename) on every change and read once when Moon starts. Unknown keys are
ignored and invalid values fall back to the defaults, so older and newer
Moons can share the file. `version` is 1.

## Wallpapers

Built in and drawn procedurally at any size: a vertical base gradient (top
to bottom color) lit by Gaussian glows (center as fractions of the screen,
radius as a fraction of the width, RGB color, strength), with a half-code-
value ordered dither. Moon (`src/a26-shell/src/wallpaper.rs`) and the System
app's picker (`a26-system/src/wallpapers.rs`) carry the same table.

| ID | Name | Look | Panel power |
| --- | --- | --- | --- |
| `moon` | Moon | night blue with indigo, blue and violet glows (the original) | the reference (≈ 213 mA on home at brightness 128, `battery.md`) |
| `black` | Black | true black: no pixel lit | lowest (the AMOLED panel draws nothing for black) |
| `graphite` | Graphite | near-black with faint gray glows | close to black |
| `dusk` | Dusk | plum with orange, magenta and violet glows | brightest of the set |
| `ocean` | Ocean | deep teal with cyan and blue glows | like `moon` |
| `forest` | Forest | deep green with green, lime and teal glows | like `moon` |

## Auto-Lock

With the screen on and no touch or key for `auto_lock_s` seconds, Moon dims
the panel to about 40 % of its level for the last five seconds, then turns
the screen off with the usual power fade (from the dimmed level). Any touch
or key during the dim restores the brightness and starts the count again.
While dimmed, Moon sets `idle_dimmed` and the brightness backend leaves the
backlight alone (`display-brightness.md`). Auto-Lock waits while a finger is
down, while an alarm or timer rings, and while the app in front plays media
(its media lease is held). A notification that woke the screen still turns
it off after 10 s untouched (`notifications-ui.md`).
