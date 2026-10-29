# App services: text areas, the clipboard, links, status bar colour, keeping the screen awake, icon badges

What Moon does for any app that asks, over its control socket
(`/run/a26-shell/control.sock`, one command per connection, a JSON reply
`{"ok": true, "result": …}` or `{"ok": false, "error": "…"}`). Each is
optional: an app that never asks looks and behaves as before. Dictation for
an in-app mic button is the voice hook's, not Moon's (`voice.md`).

| Command | Effect |
| --- | --- |
| `keyboard show multiline` | Moon's keyboard for a text area: Return (grey, `return`) types a newline and the keyboard stays up |
| `clipboard set "TEXT"` | the system clipboard holds TEXT, a JSON string (any Unicode, up to 1 MiB), and no image; `""` empties it. Reply `{"chars": N}` |
| `clipboard set image/png BASE64` | the clipboard holds the image, and no text: `image/png`, `image/jpeg`, `image/gif` or `image/webp`, its bytes in standard base64 (up to 8 MiB decoded; the bytes must be of that type). Reply `{"type": "image/png", "bytes": N}` |
| `clipboard get` | the clipboard's text, and whether it holds an image: `{"text": "…", "image": {"type": "image/png", "bytes": N}}`, each `null` when it holds none |
| `clipboard get image` | the clipboard's image: `{"image": {"type": "image/png", "bytes": N, "data": "BASE64"}}`, or `{"image": null}` |
| `open browser URL` | the Browser opens URL (`http` or `https` only) in a new tab in front, started first if it is not running; on the lock screen, after unlocking |
| `statusbar color #RRGGBB [light\|dark]` | the status bar over the app that asks is drawn in its colour, with light or dark time and glyphs (without a choice: whichever reads better) |
| `statusbar default` | back to Moon's black |
| `statusbar APP color …` / `statusbar APP default` | the same for a named app (`system`, `browser`, `phone`, `messages`, `exocortex`): tools and tests |
| `awake on` / `awake off` | the app that asks keeps the screen on while it is in front (a recording, a video): Auto-Lock neither dims nor locks, until `awake off` or the app exits |
| `awake APP on` / `awake APP off` | the same for a named app: tools and tests |
| `badge N` | the app that asks shows N on its home screen icon (0 to 99999; `0` removes it) |
| `badge APP N` | the same for a named app: a helper keeping a closed app's count, tools and tests |

`state` carries `keyboard.purpose` (`"multiline"`), `status_bar`, the
running apps' colours (`{"exocortex": {"color": "#00050f", "content":
"light"}}`), `awake`, the running apps keeping the screen on
(`["exocortex"]`), and `badges`, the icons' counts (`{"exocortex": 3}`).

## Text areas

`keyboard show multiline` is `text` with a different Return: Moon types
Return into the app (the app inserts a line break) and keeps the keyboard
up, so a chat composer takes several lines. Every other purpose still
submits and hides the keyboard on Return, as before. Wayland apps get it from
text-input-v3's multiline content hint. An app that sends on Return keeps
asking for `text`.

## The clipboard

The system clipboard is X11's CLIPBOARD selection, which the Browser
(Chromium) and any X toolkit already use, kept by Moon
(`src/a26-shell/src/clipboard.rs`):

- An app copies the X way (it takes CLIPBOARD, as the Browser's Copy does)
  or with `clipboard set` (Moon's own apps need no selection code). Either
  way Moon takes the text at once, while the app that copied is still in
  front, and owns CLIPBOARD from then on. So the text survives that app
  being frozen in the background or closed, and every paste is answered by
  Moon: an X app's own Paste (Ctrl+V, a context menu), the paste key, and
  `clipboard get`.
- Text and images. Moon asks a copy what it offers (`TARGETS`), then takes
  its text (`UTF8_STRING`, else `STRING`) and its image (`image/png`, else
  `image/jpeg`, `image/gif`, `image/webp`), one conversion after the other,
  INCR transfers included. Text over 1 MiB, an image over 8 MiB or not of
  its type, and a form its app does not deliver within 2 s are left out; a
  copy left with neither stays with its app (`clipboard get` is null, the
  paste key dims). Other forms (`text/html`, `text/uri-list`) are not kept.
  An app that does not answer `TARGETS` is asked for `UTF8_STRING`, else
  `STRING`, as before.
- The clipboard holds one copy, as iOS's pasteboard does: `clipboard set`
  of text clears the image and `clipboard set image/…` the text; a copy
  from an X app that offers both (text and an image) keeps both.
- Moon serves `UTF8_STRING`, `TEXT`, `text/plain;charset=utf-8`, `STRING` /
  `text/plain` (Latin-1) while it holds text, its image's type (in one
  property: 8 MiB is well within BIG-REQUESTS) while it holds an image, and
  `TARGETS` listing them.
- All of it runs on a thread with its own X connection, so Moon's event
  loop never waits on another app. `clipboard set image/…` and `clipboard
  get image` (megabytes of base64 on one line, at most 12 MiB) are read,
  decoded, encoded and written on a thread of their own too. Neither the
  text nor the image is ever logged.
- Wayland apps (Moon's experimental Wayland server) keep their own
  wl_data_device clipboard among themselves; the paste key types into them
  too.

**The paste key** sits on Moon's keyboard where iPhone has its emoji key,
between `123`/`ABC` and Space, on every page except the number pad. It is lit
while the clipboard holds text and dim otherwise (an image alone cannot be
typed: an app pastes an image itself, from `clipboard get image`). Tapping
it types the
clipboard into the app in front the way Moon's keys are typed (XTEST, or the
Wayland server for Wayland apps): a line break is Return in a text area
(`multiline`) and a space in a one-line field (as iPhone pastes into one); a
tab is four spaces in a text area and one elsewhere; at most 32,768
characters. Through XTEST and the server's US keymap only ASCII can be
typed, so other characters (é, emoji) are left out (`last_action` is
`paste_partial`; Wayland's text-input takes them). An app that wants every
character pastes itself from `clipboard get`.

## Links in the Browser

`open browser URL` takes `http` and `https` links only (no spaces or control
characters, at most 8 KiB); anything else is refused. Moon brings the Browser
forward as for any `open` target (a tapped notification with `open.app`
`browser` does the same; a target that is not such a link only brings the
Browser forward), then hands it the link over vimbrowser's own IPC socket
(`/opt/vimbrowser-a26/rootfs/var/lib/vimbrowser/profile/ipc.sock`,
`A26_BROWSER_IPC` overrides it): `open-focus-tab URL`, or `open-tab URL` on
builds without it (there it opens in front too). A thread waits up to 45 s
for the socket while a cold start brings CEF up, so the event loop never
blocks; `browser: opened a link` or `browser: link not opened: …` goes to
Moon's log (never the link). The Browser also gets `open_target` in `state`,
as every app does.

## Status bar colour

Moon draws the 124-pixel strip over apps black, so the camera notch
disappears into it. An app whose top edge is another colour asks for its own:
`statusbar color #00050f`. Moon finds the app from the connection (the
process asking, or its nearest ancestor that is an app Moon started: the app,
a thread of it, a tool it runs), so an app needs no name for itself. The
colour lasts as long as that process: an app that exits or restarts has
black again (and asks again when it starts). It shows whenever that app is
in front; with no choice of content Moon picks dark time and glyphs above
WCAG's 0.179 luminance (where black text has the higher contrast), light
below. Dark content keeps the battery's colours. Asked by something that is
not an app, without naming one: refused (`not asked by an app`).

## Keeping the screen awake

`awake on` is iOS's idle timer switch: while the app that asked is in front
with the screen on, Auto-Lock neither dims nor locks (`settings.md`), however
long nobody touches the phone (a voice recording, a video, a timer the user
watches). Moon finds the app from the connection as for `statusbar`, and
the request lasts as long as that process: `awake off`, the app exiting or
restarting ends it (it asks again when it starts). It only counts while the
app is in front: at home, in another app, on the lock screen or with the
screen off nothing changes, and the power key still sleeps the phone. When
it ends, or the app leaves the front, Auto-Lock counts from then (the
screen dims `auto_lock_s` − 5 s later, not at once). The app's media lease
(`lease acquire APP media N`) holds Auto-Lock off the same way, renewed
every 30 s at most, and also keeps the app running in the background;
`awake` needs no renewal and does not keep the app running once it leaves
the front. Asked by something that is not an app, without naming one:
refused (`awake: not asked by an app (name one)`); for an app that is not
running: refused (`awake: the app is not running`).

## Icon badges

`badge N` puts N on the asking app's home screen icon, as iOS's badge: a
red circle with white digits over the icon's top-right corner, a pill for
more digits (over 999 it reads `999+`), on the home grid, in the dock and
in the App Library; in edit mode it jiggles with its icon, the "−" at the
other corner. `badge 0` removes it. The count is the app's to keep current:
it stays while the app is frozen or closed, and across Moon's restarts
and reboots (Moon keeps the counts in `badges.json` beside its settings,
`/var/lib/a26-shell/badges.json`), so an app sets 0 when nothing is left to
show. While the app is closed, a helper running for it names the app
(`badge exocortex 2`). Asked by something that is not an app, without
naming one: refused (`badge: not asked by an app (name one)`).

## Verified (2026-10-29)

On the phone, in `moon-sandbox` (a dummy-driver Xorg; the real Moon, panel
and sockets untouched), with xev standing in for an app:

- `keyboard show multiline`: Return reached the app as Return twice and the
  keyboard stayed up (`text` still hid it).
- `clipboard set` with a line break, a tab and é, then the paste key in a text
  area: the app got `Hello, world!`, Return, `line 2`, four spaces, `Tabbed `
  (`paste_partial`: é left out). Another X client read the text from Moon.
- A copy by another X client (a test owner): Moon took it within
  milliseconds; `clipboard get` and other clients still had it after the
  owner exited, and while the owner was stopped (SIGSTOP, as the freezer
  does) a paste was answered by Moon in 27 ms. A 300 KB INCR copy arrived
  exactly; a Latin-1-only copy as `café`; an image-like copy (no text
  form) and a 2 MB copy left the clipboard empty and stayed with their
  owner.
- The installed Browser (CEF) in the sandbox: `open browser
  https://example.org/` cold-started it and opened the link in a new tab in
  front (the installed build has only `open-tab`: the fallback); a second
  link while it ran opened at once; a link sent on the lock screen opened
  after the passcode. The paste key typed a URL into its address field
  (`go` loaded it), and its own Ctrl+C in that field became Moon's
  clipboard.
- `statusbar color #f2f2f7` asked by a tool the app ran: Moon found the app,
  drew the bar light grey with dark content; `#00050f` light content; the
  colour was gone once the app exited.

Images, keeping the screen awake and badges, in `moon-sandbox` the same way,
with a test X client standing in for other apps:

- An X client copied a PNG (no text): Moon took it within 0.7 s and
  `clipboard get image` returned the same bytes; after the client exited,
  another X client read `TARGETS image/png` and the PNG from Moon, byte for
  byte (`UTF8_STRING` refused). A 3 MB PNG over INCR arrived exactly; an
  8.3 MiB one stayed with its owner (`clipboard: a copied image over 8
  MiB`); a copy offering text and a PNG kept both.
- `clipboard set image/png` with an 8.0 MB PNG (10.6 MB of base64) was
  answered in 40–110 ms; another X client pasted it byte for byte and
  `clipboard get image` gave it back. Meanwhile Moon answered a ping every
  2 ms within 11 ms (9 ms idle); done on its event loop, the same transfer
  had held Moon for 40–150 ms. Text then replaced the image; the paste key
  dimmed for the image and lit again for the text.
- `awake system on`, Auto-Lock at 15 s: no dim or lock in 24 s untouched,
  Moon idle meanwhile (no CPU time); after `awake system off` the screen
  dimmed at 10 s and locked at 15 s. Closing the app ended it, and it did
  not come back with the next start.
- `badge`: 1, 3, 42 and 1234 (`999+`) on the dock, the grid and in the App
  Library, and in edit mode (a change while jiggling showed at once); the
  counts were back after `restart`. Edit mode's frame times were the same
  with four badges as with none (the sandbox's XRender: 33–41 ms and 33–36
  ms a frame). The GPU compositor (none in the sandbox) was not tried.
