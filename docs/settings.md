# Moon settings: wallpaper, Auto-Lock and the home screen

Moon owns the user's appearance settings and applies them; other programs
(the System app, scripts) change them through Moon's control socket, never
by writing the file. Brightness and auto-brightness are a separate store
with their own commands: see `display-brightness.md`.

## Commands (`/run/a26-shell/control.sock`, one per connection)

| Command | Effect |
| --- | --- |
| `settings set wallpaper ID` | shows a built-in wallpaper (below) on the lock and home screens |
| `settings set auto_lock_s N` | Auto-Lock after `N` seconds untouched: `15`, `30` (default), `60`, `120`, `300`, or `0` (never) |
| `settings set timezone ZONE` | the local time zone, an IANA id from `src/a26-shell/src/tz.rs` (88 zones with their POSIX rules from tzdata 2026b; the chroot has no zoneinfo). Applied at once to Moon and to apps started afterwards; running apps follow `state.time` (`zone`, `tz` rule, `clock_24h`). Unset: config.json's `timezone` |
| `settings set clock_24h on\|off` | the 24-hour clock (unset: config.json's `clock_24h`) |
| `settings set haptics on\|off` | tap haptics (keyboard keys, Control Centre toggles and slider ends, switches; **off** by default: the A26's coin motor is too faint for crisp taps). The wrong-passcode buzz plays either way |
| `settings set haptic_STYLE_ms N` | the pulse length for `selection`, `light`, `medium` or `error` taps: 5–400 ms, or `0` for moon-notifyd's default (100, 120, 150, 300 ms) |
| `settings set power_hold voice\|menu` | what holding the power key does: talk to Exocortex (`voice`, the default, when its hook is installed; the power menu is then power + a volume key) or open the power menu (`menu`). See `voice.md` |
| `haptic STYLE [MS]` | an app asks for a tap haptic (with `MS`: that length once, for tuning) (`selection`, `light`, `medium`, `error`); Moon plays it through moon-notifyd's `haptic` when `haptics` is on and the screen is on |

An unknown key or value is refused (`ok: false`); nothing changes. Every
reply and `state` carry the current settings:

```json
"settings": {"version": 1, "wallpaper": "moon", "auto_lock_s": 30, "haptics": false, "haptic_ms": [0, 0, 0, 0], "power_hold": "voice",
             "home": {"dock": ["phone", "browser", "messages", "system"], "grid": ["exocortex"], "removed": [],
                      "widgets": ["calendar", "battery"]}}
```

## Home screen layout

`home` is the home screen (`home-screen.md`): the widgets on the top of
the page, left to right (`calendar`, `battery`; at most two), the dock's
apps (at most four), the grid's in reading order (four columns, six rows;
each widget takes two by two of its cells), and the apps taken off it,
which stay in the App Library. Every app is in exactly one of the three
lists. The user changes it by editing the home screen; the control socket
has:

| Command | Effect |
| --- | --- |
| `layout reset` | Reset Home Screen Layout: the default widgets, dock and grid, nothing removed |
| `layout set DOCK GRID` | both lists at once, comma-separated app ids in order (`-` for none); apps in neither are removed. Example: `layout set phone,messages exocortex,system,browser` |
| `layout remove APP` | takes an app off the home screen (refused when it is not on it) |
| `layout add APP` | puts it back, at the grid's first free place (refused when it is there) |
| `layout edit on\|off` | enters or leaves edit mode (on: only with the home screen shown) |
| `layout library on\|off` | shows the App Library or the home page |
| `layout widget add WIDGET` | adds a widget at the top of the page, the apps making room (refused when it is there, or when the grid's apps would no longer fit) |
| `layout widget remove WIDGET` | removes a widget; the apps move up into its place (refused when it is not there) |
| `layout widgets LIST` | the widgets at once, comma-separated in order, `-` for none: `layout widgets -` removes both |

App ids are `system`, `browser`, `phone`, `messages`, `exocortex`. Icons
move to their new places on screen; the file is saved as for any setting.
These are not user input (Auto-Lock does not count them).

Reading the file is lenient: unknown app and widget ids are skipped, an
app or widget listed twice counts once, a dock of more than four spills its
extra apps into the grid, an app the file never mentions (one newer than
the file) joins the grid, and a `home` without `widgets` (written before
widgets were part of it) keeps the default two; a `home` that is not a
layout at all gives the default, without touching the other settings.
Moons older than the layout ignore the key (and drop it if they save the
file); Moons older than its widgets keep the rest and drop `widgets` when
they save, so the widgets come back with a newer Moon.

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

## Power

| Command | Effect |
| --- | --- |
| `power` | the power key's short press (sleep / wake) |
| `power menu [restart]` | the power menu, as when the key is held for 0.9 s (with voice on, power + a volume key): *slide to power off* (or *slide to restart* after tapping Restart), Restart / Power Off, Cancel |
| `restart` | Moon quits cleanly (apps close, the phone locks) and runs the binary on disk again in the same process: an update goes live with no reboot, and the session, its mounts and the supervisor carry on (refused under Xwayland) |
| `voice start\|send\|cancel` | hold the power key to talk to Exocortex, release, or drop it (`voice.md`) |
| `open APP TARGET` | the app opens at a target, as a tapped notification does (after unlocking, when locked); `open browser URL` opens the link in the Browser (`app-services.md`) |
| `system poweroff\|reboot` | power off or restart now: Moon shows *Shutting down…* / *Restarting…*, then runs the supervisor's `/opt/a26-shell/bin/moon-power poweroff\|reboot` (docs/power.md), which starts the ordered shutdown and returns |

## App services

For apps (`app-services.md`):

| Command | Effect |
| --- | --- |
| `keyboard show multiline` | the keyboard for a text area: Return types a newline and the keyboard stays |
| `clipboard set "TEXT"` / `clipboard get` | the system clipboard (X11's CLIPBOARD, kept by Moon); TEXT is a JSON string |
| `statusbar [APP] color #RRGGBB [light\|dark]` / `statusbar [APP] default` | the status bar over the app that asks (or APP) in its own colour, or black again |

The battery policy is the supervisor's: its `moon` notifications tagged
`battery-low` show as usual, and `battery-critical` turns into a full-screen
countdown to its `expires_at` (no banner) until the phone powers off or a
charger withdraws it. Plugging a charger in lights the lock screen with a
charging card for 2.6 s; `Full` on a charger counts as charging (the bolt
stays).
