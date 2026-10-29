# Voice: hold the power key to talk to Exocortex

Hold the power key, say something, let go: Exocortex gets it as a message,
transcribed. As with Siri on an iPhone, the power menu moves to the power key
plus either volume key.

| Gesture | Effect |
| --- | --- |
| tap power | sleep / wake, as before |
| hold power (0.45 s) | Moon listens: the screen dims, "Listening…", and an orb whose glow swells with how loud you speak (it breathes while quiet); a buzz says it started |
| release | what was said is sent (held under 0.6 s: a slip, dropped) |
| hold power from a dark screen | wakes it and listens (on the lock screen) |
| power + volume up or down | the power menu (drops a recording in progress) |

Where it goes: always to the phone's voice conversation, named in
`/etc/exocortex-voice.json` (`{"conversation": "ID"}`; on this phone the
pinned main conversation, EXO); when that conversation is working the words
join its running turn (queue timing `next-turn`: Claude takes them in at its
next tool boundary) rather than waiting for the turn to end. It is sent in
the background: no app opens. A voice conversation that is gone means a new
conversation with Claude instead. Without the file: the conversation the
Exocortex app shows, when the app is in front on an unlocked screen, else a
new conversation with Claude (which, with several agents, sends words to
whichever chat happens to be open). Held on the lock screen, the passcode comes up on release
and the message is sent only once the phone is unlocked (within 2 minutes,
otherwise it is dropped): anyone can pick up a locked phone, and Exocortex
runs as root.

`settings set power_hold menu` puts the old hold back (the power menu after
0.9 s); `voice` (the default) brings voice back. Without the hook installed
the hold opens the menu whatever the setting says.

## How it works

```
 Moon (a26-shell)                    exocortex-voice (chroot)           Android's namespace
 ───────────────                     ────────────────────────           ───────────────────
 hold ─► overlay, buzz ─ spawn ─────► record ─ chroot /proc/1/root ───► moon-voice-capture
 orb ◄──── "level L\n" (stdout) ──── meter ◄── 16 kHz mono PCM ──────   (AAudio input)
 release ─ "send[ locked]\n" ───────► stop, normalize, WAV
                                      exocortexd: transcribe_audio (OpenAI)
                                      [locked: wait for a26-shellctl state: unlocked]
                                      send_message / queue_message, or
                                      new_conversation (Claude)
 open exocortex CONV ◄─ a26-shellctl ─┘
```

- **Moon** (`src/a26-shell/src/voice.rs`) owns the gesture and the overlay
  and runs the hook: `MOON_VOICE`, default `/usr/local/bin/exocortex-voice`.
  The contract: `HOOK record` records at once and reads one line on stdin
  when the key comes up: `send`, `send locked` (the hold began on the lock
  screen) or `cancel`; end of input (Moon gone) is `cancel`. Until then the
  hook prints `level L` lines on stdout (L in 0..1, how loud the voice is,
  every 50 ms); Moon reads them each frame, smooths them (up in about 50 ms,
  back down in about 250 ms) and scales the orb's glow (up to 1.5 times) and,
  a little, its core. Moon checks the
  hook is executable at each power-key press, so installing or removing it
  needs no Moon restart. Auto-Lock waits while Moon listens and counts again
  from the release (a hold longer than its timeout once slept the screen the
  moment the key came up).
- **moon-voice-capture** (`src/moon-audiod/moon-voice-capture.c`,
  `/opt/a26-audio`) records through AAudio (input preset
  VOICE_RECOGNITION) to stdout until SIGTERM, at most 120 s. It runs in
  Android's namespace (bionic, binder), like moon-audiod.
- **Recording without the framework** needs two more stand-ins in
  `moon-permissiond`. Samsung's AudioPolicyManager asks its enterprise policy
  service (`edmnativehelper`, `mdm.edmnativehelperservice`, call 7
  `isMicrophoneEnabled`) on every new input; absent, the lookup blocks until
  audioserver's 15 s TimeCheck aborts audioserver. And AudioPolicyService
  checks `CAPTURE_AUDIO_HOTWORD` through `permission_checker`
  (`android.permission.IPermissionChecker`), waiting 10 s for it. The
  stand-ins answer as Android does with no device policy and for service
  UIDs.
- **exocortex-voice** (`scripts/device/exocortex/exocortex-voice`, Bun,
  installed by `scripts/exocortex/install.sh`) meters the recording as it
  arrives for the orb (the 50 ms level in dB above a noise floor, the 20th
  percentile of the last 2 s, over 20 dB), then raises it to a
  peak of -1 dBFS (VOICE_RECOGNITION is quiet), has the daemon transcribe it
  (`transcribe_audio`: the daemon's OpenAI login, no credential in the hook)
  and sends the text. The app in front is decided as `exocortex-notify`
  does: `/run/exocortex-app/view.json` names the conversation and its live
  pid, and Moon's `view` is `exocortex` on an awake, unlocked screen. A new
  conversation is opened with `a26-shellctl open exocortex CONV`. Problems
  (no audio, nothing recognized, transcription or daemon errors) post a
  notification (app `exocortex`, tag `voice`). It logs to Moon's log, never
  what was said. `EXOCORTEX_VOICE_PROVIDER` / `EXOCORTEX_VOICE_MODEL` pick
  another model for new conversations; `exocortex-voice send FILE.wav` sends
  a recording; `EXOCORTEX_VOICE_CAPTURE=CMD` replaces the microphone with a
  command printing 16 kHz PCM (testing). `exocortex-voice dictate` records
  for an app and returns the text instead (below).

## Dictation for apps

`exocortex-voice dictate` is for an app's own mic button (the Exocortex
app's composer): it records as `record` does but gives the words back to the
app instead of sending them. The app runs the hook itself
(`/usr/local/bin/exocortex-voice dictate`) with pipes on stdin and stdout:

```
 app                                   exocortex-voice dictate
 ───                                   ───────────────────────
 mic tapped ─ spawn ─────────────────► records at once (the same capture)
 strip ◄────── "level L\n" (stdout) ── every 50 ms while recording
 "stop\n" or "cancel\n" (stdin) ────► stops recording
 text at the caret ◄─ "text \"…\"\n" │ "error MESSAGE\n", then it exits
```

- stdin: `stop` transcribes; `cancel`, anything else or end of input drops
  the recording: no answer line, exit 0.
- stdout: `level L` lines while recording (L in 0..1, as for Moon's orb),
  then after `stop` exactly one line: `text` and the transcription as one
  JSON string (exit 0; `text ""` when nothing was said), or `error` and a
  one-line message (exit 1): `The microphone recorded nothing.`, `Exocortex
  is not running.`, `Transcription failed: …`.
- Nothing is sent and no notification is posted. stderr carries the hook's
  log lines (never the words). Recording stops by itself after 120 s (the
  capture's limit); `EXOCORTEX_VOICE_CAPTURE` replaces the microphone, as for
  `record`.
- One microphone: an app should not dictate while Moon listens
  (`voice_listening` in `state`).

Verified on the A26 (2026-10-29) with a recording replayed at about real
time through `EXOCORTEX_VOICE_CAPTURE`: 69 `level` lines over 7 s, then
`text "Please float test. Please reply with just the word okay."` (the
daemon transcribed it in 0.9 s); `cancel` and end of input printed nothing
and exited 0; silence gave `text ""`; a capture that failed gave `error The
microphone recorded nothing.` and no daemon `error Exocortex is not
running.`; no conversation got a message.

## Control socket

| Command | Effect |
| --- | --- |
| `voice start` | listen, as when the key is held (refused when voice is off) |
| `voice send` / `voice cancel` | as releasing the key / as power + volume |
| `open APP TARGET` | the app opens at a target, as when a notification is tapped; on the lock screen after unlocking; never wakes the screen |

`state` carries `voice_listening`.

## Verified on the A26 (2026-10-29)

- Capture: 4 s at 16 kHz in 4.3 s wall time, a 660 Hz tone from the speaker
  at about 200 times the background. Before the stand-ins: the first try
  aborted audioserver (TimeCheck, `EDMNativeHelper::isMicrophoneEnabled`),
  then 14 s per start (10 s PermissionChecker wait).
- The daemon transcribed a 4 s clip in 0.6 to 1.6 s.
- Moon's new build in a private mount namespace on a dummy-driver Xorg
  (`:91`; the real panel, keys, backlight and Moon's sockets masked), the
  hook replaying a recording: unlocked hold → a new Opus 5.5 conversation,
  opened in the Exocortex app, answered; with that conversation in front →
  sent to it; held on the lock screen → passcode on release, sent and opened
  once unlocked.
- Live on the phone without a reboot: the running Moon (older than
  `restart`) was replaced inside its own session (its session shell
  detached so its mounts stayed, `quit`, the new build started with the
  supervisor's environment in the same mount namespace and cgroup), then the
  overlay was checked on the panel through moon-compositor's own frame
  capture (SIGUSR1). `restart` itself was checked in the preview: same PID,
  ready again within a second.
