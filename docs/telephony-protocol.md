# Moon telephony: `moon-telephonyd` protocol, version 1

Revision 1.1 (2026-10-26): call history (`call.history`,
`call.history_delete`, `call_history_changed`); `sim.slots` (physical
slots, eSIM), `sim.state` `no_profile`, `network.emergency_only`; raw SIM
logical channels for eSIM management (root only, §3.1); the incoming-call
notification rings (`ringing`) with moon-notifyd r3. Backward compatible.

`moon-telephonyd` is OSS-Phone-V1's telephony service: SIM, network and
signal status, SMS, voice calls and mobile data. It talks to the modem through
Samsung's vendor radio HAL (rild's `android.hardware.radio@1.x::IRadio` over
hwbinder) with Android's Java framework never running, keeps the SMS history,
posts notifications through `moon-notifyd`, switches call audio, and brings
the mobile data interface up. Moon (the status bar, a Phone app, a Messages
app) is its client. Design notes: `notes/telephony/DESIGN.md`.

This document is the contract. Anything not written here is not part of the
protocol. The conventions are `moon-notifyd`'s (docs/notifications-protocol.md).

## 1. Transport

| | |
| --- | --- |
| Socket | `/run/moon-telephony/telephony.sock` inside the chroot (Unix stream). The daemon runs in Android's root namespace; the file is `/data/local/tmp/moon-telephony/telephony.sock` there, bind-mounted into the chroot |
| Who may connect | peer UID 0, or a peer whose groups include `moon-telephony` (chroot `/etc/group`; checked with `SO_PEERCRED`) |
| Framing | UTF-8 JSON, one object per line, at most 16 KiB per line |
| Connections | long-lived; requests on one connection are answered in order, except that commands which wait for the modem (marked ⧗) may be answered after later ones |

Clients must never block their UI on the socket; the daemon is restarted by
its supervisor and may be absent for a few seconds. Suggested timeouts: 1 s,
15 s for ⧗ commands.

```json
{"id":7,"cmd":"sms.send","to":"+16475551234","text":"On my way"}
{"id":7,"ok":true,"result":{"message":{...}}}
{"id":8,"ok":false,"error":{"code":"unavailable","message":"no SIM ready"}}
```

`id` is a non-zero integer echoed in the response. Unknown arguments are
rejected (`invalid_argument`). Error codes: `invalid_request`,
`unknown_command`, `invalid_argument`, `not_found`, `permission_denied`,
`unavailable` (no modem connection / no SIM), `invalid_state` (e.g. answering
with no incoming call), `busy`, `refused` (a safety rule, see `sim.unlock`),
`radio_error` (the modem said no; `message` names the RIL error, e.g.
`password_incorrect; 2 attempts left`), `internal`. Messages are for logs.

After `{"cmd":"subscribe"}` the connection also receives events (objects
with `event`, never `id`). The first is always a `snapshot`. Unknown events
and unknown fields must be ignored.

## 2. Objects

Times are Unix seconds (UTC). Ids are opaque strings (`m…` message, `c…`
call).

### State

```json
{
  "available": true,
  "radio": "on",
  "airplane": false,
  "sim": {"slot": 1, "state": "ready", "pin_retries": null,
          "slots": [{"physical": 1, "logical": 1, "card": "present", "esim": false, "active": true}]},
  "network": {
    "registration": "home",
    "emergency_only": false,
    "data_registration": "home",
    "operator": "Freedom",
    "numeric": "30249",
    "rat": "lte",
    "ims_registered": false,
    "cell": {"mcc": "302", "mnc": "490", "area": 12345, "id": 67890}
  },
  "signal": {"level": 3, "dbm": -97},
  "data": {"enabled": true, "state": "connected", "apn": "internet.freedommobile.ca",
           "interface": "rmnet0", "addresses": ["10.12.34.56/30"], "dns": ["…"], "error": null},
  "calls": [],
  "sms": {"unread": 2}
}
```

| Field | Values |
| --- | --- |
| `available` | false while the daemon has no connection to rild (everything else is then stale or unknown) |
| `radio` | `on`, `off` (airplane mode or modem off), `unavailable`, `unknown` |
| `sim.state` | `absent`, `pin_required`, `puk_required`, `network_locked`, `not_ready` (detected, still reading), `no_profile` (an eSIM chip with no enabled profile), `ready`, `error`, `restricted`, `unknown` |
| `sim.slots` | the physical slots (IRadioConfig): `card` `absent`/`present`/`error`/`restricted`/`unknown`, `esim` (the card is an eUICC), `active`, and the `logical` slot it is mapped to; empty until known |
| `sim.pin_retries` | attempts left, known only after a PIN attempt; else `null` |
| `network.registration` | voice service: `home`, `roaming`, `searching`, `denied`, `none`, `unknown`; `data_registration` likewise for packet service |
| `network.emergency_only` | not registered, but the modem reports that emergency calls are possible (show "Emergency calls only") |
| `network.operator` | name to show (long name, else short); `null` when unknown |
| `network.rat` | `gsm`, `umts`, `lte`, `nr`, `iwlan`, `unknown`, or `null` |
| `signal.level` | 0–4 bars (Android's thresholds); `signal` is `null` until the modem reports |
| `data.state` | `off`, `connecting`, `connected`, `disconnecting`, `failed` (`error` says why; it retries with backoff) |

The status bar needs only `sim.state`, `network.registration`,
`network.emergency_only`, `network.operator`, `network.rat`,
`signal.level`, `data.state` and `airplane`.

### Call

```json
{"id": "c3", "number": "+16475551234", "name": null, "state": "active",
 "direction": "incoming", "multiparty": false, "started_at": 1793001234, "connected_at": 1793001240}
```

`state`: `dialing`, `alerting` (the other phone rings), `incoming`,
`waiting` (incoming while another call is active), `active`, `held`; in
`call_ended`, `ended`. `number` is `null` for private/unknown callers.

### Call record (history)

```json
{"id": "c3", "number": "+16475551234", "name": null, "direction": "incoming",
 "started_at": 1793001234, "connected_at": 1793001240, "ended_at": 1793001300,
 "duration_s": 60, "missed": false, "rejected": false}
```

One per ended call, with the call's id. `connected_at` is `null` for calls
that never connected; `missed` is an unanswered incoming call the user did not
decline, `rejected` one declined with `call.reject`/`call.hangup`. The newest
500 are kept.

### Message

```json
{"id": "m42", "peer": "+16475551234", "direction": "in", "text": "Hi!", "time": 1793001234,
 "status": "received", "read": false, "error": null}
```

`peer` is the thread key: numbers in international form (`+1…` for NANP),
short codes as digits, alphanumeric senders as sent. `status`: `received`;
for outgoing `sending`, `sent`, `delivered` (status report received),
`failed` (`error` says why). Texts up to 1600 characters; long ones are sent
as concatenated SMS and incoming parts are reassembled.

## 3. Commands

| `cmd` | Arguments | Result |
| --- | --- | --- |
| `ping` | — | `{"version":1}` |
| `subscribe` | — | `{}`, then events |
| `status` | — | the State object |
| `sim.unlock` ⧗ | `pin` (4–8 digits), optional `force` | `{}`. Errors: `invalid_state` unless `sim.state` is `pin_required`; `radio_error` `password_incorrect; N attempts left`; **`refused` when one attempt is left** (a wrong PIN would PUK-lock the SIM) unless `force: true`. The daemon never retries a PIN by itself |
| `sms.send` | `to`, `text` | `{"message": Message}` with `status: "sending"`; progress comes as `sms_updated` |
| `sms.threads` | — | `{"threads": [{"peer", "last": Message, "count", "unread"}]}`, most recent first |
| `sms.list` | `peer`, optional `before` (message id), `limit` (1–200, default 50) | `{"messages": [...]}`, newest first |
| `sms.mark_read` | `peer` or `ids` | `{"count": n}`; event `sms_read`; withdraws the thread's notification |
| `sms.delete` | `peer` or `ids` | `{"count": n}`; event `sms_deleted` |
| `call.dial` ⧗ | `number` (digits, `+ * # , ;`) | `{}` once the modem accepted it; the call appears through `calls_changed` |
| `call.answer` ⧗ | — | `{}`; answers the incoming call (holding an active one for a waiting call) |
| `call.reject` ⧗ | — | `{}`; declines the incoming/waiting call |
| `call.hangup` ⧗ | optional `id` | `{}`; default: the active/dialing call, else the incoming one |
| `call.swap` ⧗ | — | `{}`; swaps active and held |
| `call.dtmf` ⧗ | `digits` (`0-9 * # A-D`) | `{}` |
| `call.audio` | optional `speaker`, `mute` (bools) | `{"speaker", "mute"}` |
| `call.history` | optional `before` (call id), `limit` (1–200, default 50) | `{"calls": [CallRecord…]}`, newest first |
| `call.history_delete` | `ids` or `all: true` | `{"count": n}`; event `call_history_changed` |
| `data.get` | — | `{"data": {...}, "apn": {"apn","protocol","user","auth"}}` |
| `data.set` | optional `enabled`, `apn`, `protocol` (`IP`, `IPV6`, `IPV4V6`), `user`, `password`, `auth` (0 none, 1 PAP, 2 CHAP, 3 either) | as `data.get`; persistent |
| `radio.set` ⧗ | `airplane` (bool) | `{}`; persistent |

Placing calls and sending SMS cost money and reach real people: Moon must
only issue them on an explicit user action.

### 3.1 SIM logical channels (root only)

For eSIM management with lpac (GSMA SGP.22 LPA; Alpine package `lpac`):
`moon-telephonyctl lpac ARGS…` runs lpac with its stdio APDU driver and
carries its APDUs through these commands. They are refused
(`permission_denied`) for any peer but root.

| `cmd` | Arguments | Result |
| --- | --- | --- |
| `sim.channel_open` ⧗ | `aid` (hex), optional `p2`, `slot` (1–2, default the main slot) | `{"channel": n, "select": hex}` |
| `sim.apdu` ⧗ | `channel`, `apdu` (hex, the whole command APDU), optional `slot` | `{"response": hex}` (data followed by SW1 SW2) |
| `sim.channel_close` ⧗ | `channel`, optional `slot` | `{}` |

## 4. Events

| `event` | Payload | Moon should |
| --- | --- | --- |
| `snapshot` | `state`, `now` | replace all its telephony state |
| `state_changed` | `state` | update the status bar etc. (dBm-only changes come at most every 15 s) |
| `calls_changed` | `calls` | update the in-call UI |
| `call_incoming` | `call` | **turn the screen on and show the incoming-call screen** (works locked: Answer/Decline call `call.answer`/`call.reject` without unlocking) |
| `call_ended` | `call` (state `ended`), `duration_s`, `missed` | dismiss the call UI |
| `call_history_changed` | `ids` (records added or deleted) | refresh Recents (`call.history`) |
| `sms_received` | `message` | update Messages (the notification is posted by the daemon) |
| `sms_updated` | `message` | an outgoing message's status changed (also sent once at `sms.send`) |
| `sms_read` / `sms_deleted` | `ids` | update lists and badges |
| `ussd` | `message` | show it |

## 5. Notifications and ringing

The daemon posts to `moon-notifyd` (Moon does not need to):

| When | `notify.post` |
| --- | --- |
| SMS received | `app: "messages"`, `tag: "sms-<peer>"` (one per thread, replaced), `title`: peer, `body`: text, `urgency: normal`, `open: {"app":"messages","target":"thread:<peer>"}` |
| Incoming call | `app: "phone"`, `tag: "call-<n>"`, `title: "Incoming call"`, `urgency: high` (wakes the screen), `ringing: true` when moon-notifyd's `ping` reports revision ≥ 3; withdrawn (which stops the ringing) when answered, declined or ended |
| Missed call | `app: "phone"`, `tag: "missed-<peer>"`, `title: "Missed call"`, `urgency: normal`, `open: {"app":"phone","target":"call:<peer>"}` |

App ids `phone` and `messages` are reserved for Moon's Phone and Messages
apps. With an older moon-notifyd (revision < 3) an incoming call chimes once
instead of ringing.

## 6. Call audio

On call state changes the daemon sets the audio mode through
`moon-call-audio` (`ringtone` while only ringing, `in_call` from dialing or
answering until the last call ends, then `normal`). Samsung's audio HAL then
routes the modem's voice path to the earpiece; `call.audio speaker=true`
moves it to the loudspeaker, `mute=true` mutes the microphone. Both reset
when the last call ends.

## 7. Mobile data

With `data.enabled` the daemon keeps a default data connection up whenever
the SIM is ready and packet service is registered, and tears it down when
disabled. The interface is configured by the `mobile-data` hook: a routing
table and policy rule behind Wi-Fi's (Wi-Fi wins when connected), and the
chroot's `/etc/resolv.conf` when Wi-Fi is not up.

## 8. Files

| Path | Contents |
| --- | --- |
| `/data/local/a26-linux/opt/moon-telephony/moon-telephonyd` | the daemon (static musl; started outside the chroot) |
| `/data/local/a26-linux/opt/moon-telephony/moon-telephonyctl` | command-line client (also usable inside the chroot) |
| `/data/local/a26-linux/opt/moon-telephony/moon-call-audio` | bionic call-audio helper |
| `/data/local/a26-linux/opt/moon-telephony/mobile-data` | data interface hook |
| `/var/lib/moon-telephony/sms.json` (chroot) | message history, newest 5000 (root, `0600`) |
| `/var/lib/moon-telephony/calls.json` (chroot) | call history, newest 500 (`0600`) |
| `/var/lib/moon-telephony/settings.json` (chroot) | data/APN/airplane settings (`0600`) |

```sh
moon-telephonyctl status
moon-telephonyctl sms.send to=+16475551234 'text=Hello from Moon'
moon-telephonyctl call.dial number=+16475551234
echo 1234 | moon-telephonyctl sim.unlock pin=-     # the PIN never enters argv
moon-telephonyctl subscribe
```
