# Moon notifications, alarms and timers: `moon-notifyd` protocol, version 1

Revision 2 (2026-10-26): object arguments are named by kind (`notification`,
`alarm`, `timer`) instead of `id`, which is the request id; duplicate JSON
keys are rejected.

Revision 3 (2026-10-26): `notify.post` takes an optional `ringing` (incoming
calls: ringtone and vibration in a loop until the notification goes away,
§5), the `ringtone` sound, and reserved app ids `phone` and `messages`.
`ping` returns `revision`. Backward compatible: older posts, state files and
clients are unaffected; a revision-2 daemon rejects a post with `ringing`
(`invalid_argument`), so a client that needs it checks `ping`'s
`revision` ≥ 3 (absent means 2).

Revision 4 (2026-10-26): `haptic` (§4), a short vibrator pulse for a tap.
`ping` returns `revision: 4`; an older daemon answers `unknown_command`, so
a client checks `revision` ≥ 4 before sending it.

`moon-notifyd` is OSS-Phone-V1's central service for **notifications**,
**alarms** and **timers**. It runs as root inside the Alpine chroot next to
Moon, owns all persistent state, decides when something fires, plays the alert
sound and drives the vibrator, and keeps the RTC programmed so alarms survive
screen-off and the opt-in deep suspend. Moon is its main client: it shows
banners, the notification center and the alarm/timer UI, and turns the screen
on when the daemon says something needs the user. Apps post notifications.

This document is the contract. Moon, apps and the daemon are built against it;
anything not written here is not part of the protocol.

## 1. Transport

| | |
| --- | --- |
| Socket | `/run/moon-notify/notify.sock` (Unix stream, inside the chroot) |
| Directory | `/run/moon-notify`, `root:moon-notify`, mode `0750` |
| Socket mode | `root:moon-notify`, `0660` |
| Who may connect | peer UID 0, or a peer whose primary or supplementary groups include `moon-notify` (checked with `SO_PEERCRED` and `/proc/PID/status`; file modes are not trusted alone) |
| Framing | UTF-8 JSON, **one object per line** (`\n`-terminated); at most 16 KiB per line; a longer line closes the connection |
| Connections | long-lived; any number of requests per connection; requests on one connection are answered in order |

A client that cannot reach the socket must treat the service as unavailable
and retry later (it starts with the session and is restarted by its
supervisor); it must never block its own UI on the socket. Suggested client
timeouts: 1 s per request.

### Requests and responses

```json
{"id":7,"cmd":"notify.post","app":"system","title":"Wi-Fi","body":"Connected to UofT"}
```

- `id`: non-zero integer chosen by the client, echoed in the response. It is
  only ever the request id: commands that act on an object name it by its
  kind (`notification`, `alarm`, `timer`), never `id`.
- `cmd`: the command name (§3–§6). Other fields are the command's arguments.
  Unknown fields are rejected (`invalid_argument`), so typos surface.
- A request with a **duplicate key** anywhere (e.g. two `id`s) is rejected
  with `invalid_request` instead of silently keeping one value.

```json
{"id":8,"cmd":"notify.dismiss","notification":"n42"}
```

```json
{"id":7,"ok":true,"result":{"id":"n42","replaced":false}}
{"id":8,"ok":false,"error":{"code":"not_found","message":"no notification n99"}}
```

A request that cannot be answered by id (not JSON, duplicate keys, no `id`
or `cmd`) gets `"id": null`. Error codes: `invalid_request` (not JSON,
duplicate keys, no `id`/`cmd`), `unknown_command`,
`invalid_argument`, `not_found`, `permission_denied`, `limit_exceeded`,
`internal`. `message` is for logs, never for users.

### Events

After `{"cmd":"subscribe"}` the connection also receives events, one per line,
interleaved with responses. Events carry `event` and never `id`:

```json
{"event":"notification_posted","notification":{...}}
```

The first event after subscribing is always a full `snapshot` (§7), so a
client that (re)connects never needs to replay history. Unknown event names
must be ignored (future versions add events; they never change existing ones).

## 2. Objects

All times are **Unix seconds, UTC** (integers). Local wall-clock fields
(alarm `hour`/`minute`/`days`) are interpreted in the phone's time zone: the
`timezone` key of `/etc/a26-shell/config.json` (POSIX TZ rule, e.g.
`EST5EDT,M3.2.0,M11.1.0`), else the chroot's `/etc/localtime`, else UTC.
IDs are opaque strings (`n…` notification, `a…` alarm, `t…` timer); never
parse them.

### Notification

```json
{
  "id": "n42",
  "app": "system",
  "tag": "wifi-state",
  "title": "Wi-Fi",
  "body": "Connected to UofT",
  "urgency": "normal",
  "silent": false,
  "posted_at": 1793001234,
  "expires_at": null,
  "seen": false,
  "open": {"app": "system", "target": "wifi"},
  "ringing": false
}
```

| Field | Rules |
| --- | --- |
| `app` | required; the posting app's id, `[a-z0-9-]{1,32}` (Moon's app ids: `system`, `browser`; `moon` for Moon's own; `moon-notifyd` for the daemon's; reserved: `phone` for the Phone app, `messages` for the Messages app) |
| `tag` | optional, `[A-Za-z0-9._-]{1,64}`; a post with the same `app`+`tag` **replaces** the old one (same id, `replaced: true`) instead of adding |
| `title` | required, 1–120 chars after trimming |
| `body` | optional, ≤ 1000 chars; plain text, `\n` allowed |
| `urgency` | `low` (no banner, no sound: center only), `normal` (banner + short sound/vibration), `high` (banner that stays until acted on, wakes the screen) |
| `silent` | `true` suppresses sound and vibration for this one |
| `expires_at` | optional; the daemon withdraws it then (reason `expired`) |
| `ringing` | optional (r3), default `false`; `true` rings like an incoming call until the notification is withdrawn, dismissed or activated (§5). The daemon sets it back to `false` when ringing stops without that (timeout, do-not-disturb, re-post) |
| `seen` | set by Moon when shown in the center (`notify.mark_seen`) |
| `open` | optional; what tapping it should do. `{"app": APP_ID}` brings that app forward; `target` is an opaque string ≤ 256 chars handed to the app (see `notification_activated`) |

Limits: 50 notifications per app, 200 in total; beyond that the oldest
`low`/`normal` ones of that app are removed (reason `evicted`). Notifications
persist across reboots until withdrawn, dismissed or expired.

### Alarm

```json
{
  "id": "a3",
  "hour": 7, "minute": 30,
  "days": [1, 2, 3, 4, 5],
  "label": "Lecture",
  "enabled": true,
  "sound": "radar",
  "vibrate": true,
  "snooze_minutes": 9,
  "next_fire_at": 1793055000,
  "snoozed_until": null,
  "ringing": false
}
```

- `days`: weekdays it repeats on, `0` = Sunday … `6` = Saturday, sorted,
  unique. `[]` = one-shot: rings once at the next `hour:minute`, then
  `enabled` becomes `false`.
- `next_fire_at`: computed by the daemon; `null` when disabled. If the local
  time does not exist (spring-forward gap) it fires shifted forward by the
  jump (02:30 → 03:30); an ambiguous time (fall-back) fires on its first
  occurrence.
- `sound`: `radar` (default), `chimes`, `beacon`, `ringtone` (r3), or `none`
  (vibration only).
- `snooze_minutes`: 1–30, default 9.
- Up to 32 alarms. Persistent.

### Timer

```json
{
  "id": "t5",
  "label": "Pasta",
  "duration_s": 600,
  "state": "running",
  "ends_at": 1793001834,
  "remaining_s": 600,
  "sound": "chimes"
}
```

`state` is `running` (`ends_at` set, `remaining_s` = time left when the object
was produced), `paused` (`ends_at` null, `remaining_s` frozen), or `ringing`.
A finished timer that the user has stopped is deleted. Up to 16 timers,
duration 1 s – 24 h. Persistent (a running timer keeps counting across a
reboot, by wall clock).

## 3. Commands for everyone

| `cmd` | Arguments | Result |
| --- | --- | --- |
| `ping` | — | `{"version":1,"revision":3}` |
| `subscribe` | — | `{}`; then a `snapshot` event and all later events on this connection |
| `notify.post` | Notification fields: `app`, `title`, optional `tag`, `body`, `urgency` (default `normal`), `silent`, `expires_at`, `open`, `ringing` | `{"id":"n42","replaced":false}` |
| `notify.withdraw` | `notification`, or `app` + `tag` | `{}` (`not_found` if absent). Event reason `withdrawn` |
| `notify.list` | optional `app` | `{"notifications":[…]}`, newest first |

Apps should withdraw notifications that no longer apply (a finished download,
a reconnected network) instead of piling new ones up; `tag` makes that a
single post.

## 4. Commands for Moon (the user's actions)

Any authorized peer may send these; by convention only Moon does, because
they reflect what the user did.

| `cmd` | Arguments | Effect |
| --- | --- | --- |
| `notify.dismiss` | `notification` | removed, reason `dismissed` |
| `notify.dismiss_all` | optional `app` | all (of that app) removed, reason `dismissed` |
| `notify.activate` | `notification` | removed with reason `activated`; a `notification_activated` event (with `app`, `tag`, `open`) goes to every subscriber, so the posting app can react; Moon itself performs `open` |
| `notify.mark_seen` | `ids` (array) or `all: true` | sets `seen`; `notifications_seen` event |
| `haptic` (r4) | `style`: `selection`, `light`, `medium`, `error` | `{}`; one short pulse (12, 16, 24, 40 ms for now; to be tuned by hand), see below |
| `alarm.list` | — | `{"alarms":[…]}` sorted by `next_fire_at` |
| `alarm.create` | `hour` 0–23, `minute` 0–59, optional `days`, `label` (≤ 40), `enabled` (true), `sound`, `vibrate` (true), `snooze_minutes` | the alarm |
| `alarm.update` | `alarm` + any alarm field above | the alarm (recomputed `next_fire_at`; clears a pending snooze) |
| `alarm.delete` | `alarm` | `{}` (stops it if ringing) |
| `alarm.stop` | `alarm` | stops a ringing alarm; one-shot alarms become disabled |
| `alarm.snooze` | `alarm` | stops ringing; rings again after `snooze_minutes` (a snoozed one-shot rings once more and stays disabled) |
| `timer.list` | — | `{"timers":[…]}` |
| `timer.create` | `duration_s`, optional `label` (≤ 40), `sound` | the timer, `running` |
| `timer.pause` / `timer.resume` | `timer` | the timer |
| `timer.cancel` | `timer` | `{}`; deletes it (also stops it if ringing) |
| `timer.stop` | `timer` | stops a ringing timer and deletes it |
| `timer.restart` | `timer` | a ringing or finished timer runs again for its `duration_s` |
| `settings.get` | — | the settings object below |
| `settings.set` | any settings fields | the settings |

Settings (persistent):

```json
{"alert_volume": 70, "vibrate": true, "notification_sounds": true, "do_not_disturb": false}
```

`alert_volume` (0–100) is the loudness of alarms, timers and notification
sounds, independent of Moon's media volume. `do_not_disturb` silences and
un-vibrates notifications of every urgency and suppresses their screen wake;
**alarms and timers always ring**, and so do `ringing` notifications from the
`phone` app (calls); `ringing` from any other app is ignored under
do-not-disturb.

`haptic` rules: it is stateless (no persistence, no `dirty`, no events) and
best-effort: the daemon writes the vibrator once, or not at all when
anything rings (a call, an alarm or a timer, whatever its `vibrate`), while
a notification buzz runs (so a tap never cuts one short), or within 40 ms of
the previous pulse (so typing or scrolling cannot flood sysfs). Haptics obey
only Moon's own `haptics` setting (`settings.md`): not `vibrate`, which is
for alerts, and not do-not-disturb. Moon sends them only for the user's own
touches with the screen on, and never waits for the reply.

## 5. Events

| `event` | Payload | Moon should |
| --- | --- | --- |
| `snapshot` | `notifications`, `alarms`, `timers`, `ringing` (array of `{kind, id}`), `settings`, `now` | replace all of its state |
| `notification_posted` | `notification`, `replaced` (bool), `wake` (bool) | banner unless `urgency` is `low`; if `wake`, turn the screen on (showing the banner on the lock screen) |
| `notification_removed` | `id`, `reason`: `withdrawn`, `dismissed`, `activated`, `expired`, `evicted` | drop it from banners and the center |
| `notification_activated` | `id`, `app`, `tag`, `open` | (apps) handle `open.target` |
| `notifications_seen` | `ids` | update unread badges |
| `alarm_changed` | `alarm` | update the alarm list |
| `alarm_deleted` | `id` | — |
| `alarm_ringing` | `alarm` | **turn the screen on** and show the full-screen alarm (works locked; Stop and Snooze call `alarm.stop`/`alarm.snooze` without unlocking) |
| `alarm_ended` | `id`, `reason`: `stopped`, `snoozed`, `timeout` | dismiss the alarm screen |
| `timer_changed` | `timer` | update the timer UI (Moon counts down locally from `ends_at`; the daemon does not send ticks) |
| `timer_deleted` | `id` | — |
| `timer_ringing` | `timer` | turn the screen on; show Stop / Repeat (`timer.stop` / `timer.restart`) |
| `timer_ended` | `id`, `reason`: `stopped`, `restarted`, `timeout` | dismiss |
| `settings_changed` | `settings` | — |

Ringing: the daemon plays the sound in a loop and vibrates a 500 ms on /
500 ms off pattern until stopped. An alarm rings for at most **10 minutes**,
a timer for at most **5**; then it ends with reason `timeout`, a missed alarm
or timer becomes a `high` notification from `moon-notifyd`, and an alarm with
`snooze_minutes` is **not** re-armed (it waits for its next normal time).
Several things may ring at once; each has its own `alarm_ringing` /
`timer_ringing` and is stopped separately; sound and vibration continue while
any is ringing.

Ringing notifications (incoming calls, r3): a post with `ringing: true`
plays the ringtone (a marimba arpeggio, distinct from every alarm sound) in a
loop and vibrates the ringing pattern (if `vibrate` is on) at
`alert_volume`, instead of the notification chime; `silent`, `urgency` and
`notification_sounds` do not affect it. Its `notification_posted` has
`wake: true`. It rings until the notification is withdrawn, dismissed or
activated, or until it is re-posted (same `app`+`tag`) without `ringing`
(answered: update the notification in place to stop the ringtone). It rings
for at most **2 minutes**; then the daemon keeps the notification (a missed
call) and re-posts it with `ringing: false` (`replaced: true`,
`wake: false`). Ringing is not persisted: after a daemon restart no
notification rings and every `ringing` is `false`. While a call rings, the
ringtone replaces the alarm or timer sound (they keep ringing and need
stopping as usual) and vibration continues. Calls are not in the
`snapshot`'s `ringing` list (that lists alarms and timers); Moon finds them
by `notification.ringing`. The Phone app should post calls with
`app: "phone"`, a `tag` per call, `urgency: "high"` and an `open` target.

Notification sound: `normal` and `high` play a short two-tone chime (and a
200 ms vibration if `vibrate` is on) unless `silent`, `notification_sounds` is
off, or do-not-disturb is on. At most one notification sound per 2 s.

## 6. Missed events and reboots

The daemon recomputes everything at start. An alarm or timer that came due
while it was not running (reboot, crash, a deep-suspend warm reboot) **rings
now** if it is less than 15 minutes late; otherwise it becomes a "Missed
alarm" / "Timer done" `high` notification and the alarm moves to its next
time. Moon may connect after the daemon fired something: the `snapshot`'s
`ringing` list is how it learns that an alarm is ringing right now.

## 7. Power and wake

- **Screen off (default, DPMS).** The CPU stays awake; the daemon waits on
  `CLOCK_REALTIME_ALARM` timers (`timerfd`), which also arm the RTC if the
  kernel ever suspends. Nothing polls: the daemon sleeps until the next due
  time or a client message, so it costs nothing while idle. When something
  needs the user it sends the event with the screen-on instruction (§5);
  turning the panel on is Moon's job.
- **Deep suspend (opt-in, `deep_suspend: true`).** The daemon keeps
  `/var/lib/moon-notify/next-wake` (chroot path; one line, Unix seconds of the
  earliest enabled alarm, snooze or running timer, or empty) up to date.
  `moon-suspend-cycle.sh` arms `/sys/class/rtc/rtc0/wakealarm` to that time
  minus 60 s before entering deep sleep (the warm reboot that follows a wake
  brings Moon back in ≈ 45 s), treats that RTC wake as a user wake, and does
  not enter deep sleep at all when it is less than 2 minutes away. After the
  reboot, §6 makes the daemon ring on time.
- **Sound.** Alert audio is 48 kHz stereo signed 16-bit PCM written to
  `/run/moon-audio/alerts`, a second FIFO of `moon-audiod` that plays as its
  own AAudio track, mixed by AudioFlinger with media (so an alarm is heard
  over the browser and is not affected by the media volume). The daemon
  applies `alert_volume` itself. Tones are synthesized; there are no sound
  files. If audio is not up yet, ringing is vibration-only until it is.
- **Vibration.** `/sys/class/timed_output/vibrator/enable` (milliseconds,
  ≤ 10000; `0` stops) of Samsung's `sec_vibrator`, with `intensity` 0–10000.
  The daemon is its only writer in Moon mode (tap haptics come through
  `haptic`).

## 8. Files

| Path (chroot) | Contents |
| --- | --- |
| `/opt/a26-shell/bin/moon-notifyd` | the daemon |
| `/opt/a26-shell/bin/moon-notifyctl` | command-line client (one request per invocation, prints the JSON result) |
| `/var/lib/moon-notify/state.json` | alarms, timers, notifications, settings (root, `0600`, atomic writes) |
| `/var/lib/moon-notify/next-wake` | §7 |
| `/run/moon-notify/notify.sock` | §1 |

Examples with the CLI:

```sh
moon-notifyctl notify.post app=system title=Wi-Fi body='Connected to UofT' tag=wifi
moon-notifyctl alarm.create hour=7 minute=30 'days=[1,2,3,4,5]' label=Lecture
moon-notifyctl timer.create duration_s=600 label=Pasta
moon-notifyctl subscribe        # prints events until interrupted
```

`key=value` arguments become JSON fields; values that parse as JSON (numbers,
`true`, arrays) are sent as such, everything else as strings.
