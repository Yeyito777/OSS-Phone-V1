# App services: text areas, the clipboard, links, status bar colour

What Moon does for any app that asks, over its control socket
(`/run/a26-shell/control.sock`, one command per connection, a JSON reply
`{"ok": true, "result": …}` or `{"ok": false, "error": "…"}`). Each is
optional: an app that never asks looks and behaves as before. Dictation for
an in-app mic button is the voice hook's, not Moon's (`voice.md`).

| Command | Effect |
| --- | --- |
| `keyboard show multiline` | Moon's keyboard for a text area: Return (grey, `return`) types a newline and the keyboard stays up |
| `clipboard set "TEXT"` | the system clipboard holds TEXT, a JSON string (any Unicode, up to 1 MiB); `""` empties it. Reply `{"chars": N}` |
| `clipboard get` | the clipboard's text: `{"text": "…"}`, or `{"text": null}` when it holds none |
| `open browser URL` | the Browser opens URL (`http` or `https` only) in a new tab in front, started first if it is not running; on the lock screen, after unlocking |
| `statusbar color #RRGGBB [light\|dark]` | the status bar over the app that asks is drawn in its colour, with light or dark time and glyphs (without a choice: whichever reads better) |
| `statusbar default` | back to Moon's black |
| `statusbar APP color …` / `statusbar APP default` | the same for a named app (`system`, `browser`, `phone`, `messages`, `exocortex`): tools and tests |

`state` carries `keyboard.purpose` (`"multiline"`) and `status_bar`, the
running apps' colours: `{"exocortex": {"color": "#00050f", "content":
"light"}}`.

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
- Text only. A copy with no text form (an image), longer than 1 MiB, or
  whose app does not answer within 2 s stays with its app; the clipboard
  then has no text (`clipboard get` is null, the paste key dims).
- Moon serves `UTF8_STRING`, `TEXT`, `text/plain;charset=utf-8`, `STRING` /
  `text/plain` (Latin-1) and `TARGETS`; it reads a copy as `UTF8_STRING`,
  else `STRING`, including INCR transfers.
- All of it runs on a thread with its own X connection, so Moon's event
  loop never waits on another app. The text is never logged.
- Wayland apps (Moon's experimental Wayland server) keep their own
  wl_data_device clipboard among themselves; the paste key types into them
  too.

**The paste key** sits on Moon's keyboard where iPhone has its emoji key,
between `123`/`ABC` and Space, on every page except the number pad. It is lit
while the clipboard holds text and dim otherwise. Tapping it types the
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
