# Telephony UI: status bar, call screens, Phone and Messages

Moon's side of `moon-telephonyd` (`telephony-protocol.md`, revision 1.2).
With no SIM in the phone (the A26 today) the status bar says so, Phone keeps
its keypad and history but cannot place calls, and Messages says why it is
empty. The call screens are complete but, until a SIM is in, tested only
against `scripts/a26-shell/fake-telephonyd.py`.

## Status bar

From the daemon's state (`telephony_client.rs`), iPhone style, right to
left: battery, Wi-Fi, then the cellular indicator:

| State | Shows |
| --- | --- |
| registered | four signal bars (`signal.level`); with Wi-Fi off and mobile data connected, `5G` / `LTE` / `3G` / `E` takes the Wi-Fi glyph's place |
| `sim.state` `absent` or `no_profile` | `No SIM` |
| `pin_required`, `puk_required`, `network_locked` | `SIM Locked` |
| not registered, `network.emergency_only` | `SOS` |
| `searching` | `Searching` |
| otherwise unregistered | `No Service` |
| `airplane` | an airplane |
| no daemon, or `available: false` | nothing (the bar is as before) |

The lock screen and Control Centre show the carrier where the time would be
(`network.operator`, or "Emergency calls only", "No SIM", "Airplane Mode").

## Call screens (Moon, `calls.rs`)

Moon draws the calls, not the Phone app, so they work locked and straight
from screen-off:

- **Incoming** (`call_incoming`; the screen comes on, Auto-Lock waits).
  Locked: *slide to answer*. Unlocked: Decline and Accept. A call waiting
  behind another: Decline, Hold & Accept, End & Accept. The power key
  silences the ringtone (it dismisses the daemon's ringing notification),
  a second press declines. A ringing `phone` notification gets no banner
  and no lock-screen card: the call screen is its face.
- **In call**: caller (number formatted, `+1 (647) 555-1234`), status
  (`calling…`, `ringing…`, the timer, `on hold`), mute, keypad (DTMF),
  speaker, swap when a call is held, end. "Call Ended" shows for 1.2 s.
- A swipe up from the bottom edge tucks the in-call screen away; the
  status bar's clock then sits in a green pill, and tapping it brings the
  screen back.

Commands go out only on the user's action: `call.answer`, `call.reject`,
`call.hangup {call}`, `call.swap`, `call.dtmf`, `call.audio`. `ctl state`
has a `telephony` summary (`screen`, `pill`, `mute`, …).

Not done: a proximity sensor (no screen-off at the ear), contacts (callers
are numbers), compose in Messages, SIM PIN entry.

## Phone and Messages (the System app's binary)

Moon starts them as `a26-system --app phone` and `a26-system --app
messages` (their own processes, cards and freezer groups; the dock is
Phone, Browser, Messages, System, with icons Moon draws). Each connects to
the daemon itself.

- **Phone**: Keypad (number grouped as typed, Call, delete) and Recents
  (`call.history`; missed in red, tap to call back). Without service the
  keypad says why under the number ("No SIM", "Emergency calls only",
  "Cellular service unavailable") and Call explains instead of dialing.
- **Messages**: conversations (`sms.threads`, unread dot) and a
  conversation's bubbles (`sms.list`, delivery status; opening marks it
  read). Empty states: "No SIM", "No Messages". Sending is not built yet.

A tapped `messages`/`phone` notification opens its target
(`thread:PEER`, `call:PEER`): Moon puts it in its state as `open_target`
(`app`, `target`, `serial`, `age_ms`), where the app looks when it starts or
comes forward.

## Drags for Moon's own apps

Moon used to hand apps taps only. The System, Phone and Messages apps now
also get drags: once a touch outside the bottom gesture edge moves past the
tap slop, Moon sends the app a button press at the start point, the motion,
and the release (XTEST), so lists scroll with momentum. The Browser keeps
taps only.

## Preview

```sh
scripts/a26-shell/fake-telephonyd.py /tmp/moon-ui/telephony.sock /tmp/moon-ui/notify.sock
MOON_TELEPHONY_SOCKET=/tmp/moon-ui/telephony.sock …   # for Moon and the apps
```

The fake answers the protocol, seeds some history and messages, and takes
`fake.incoming number=…`, `fake.remote_hangup`, `fake.sms peer=… text=…` and
`fake.set sim=absent registration=none emergency_only=true signal=null`
(the A26 today) to script the other side.
