# Notifications, alarms and timers in Moon

Moon is `moon-notifyd`'s main client (see `notifications-protocol.md`).
`src/a26-shell/src/notify_client.rs` holds the non-blocking socket client
(polled with the rest of Moon's event loop, reconnecting every 0.5–5 s while
the daemon is away); `alerts.rs` decides what is shown and what touches do,
and draws it; `banner.rs` is the banner window used over apps.

| Surface | Behaviour |
| --- | --- |
| Banner | `normal`/`high` notifications drop in from the top over the home screen, the switcher and apps. Tap: `notify.activate` and open its app. Swipe up: hides the banner; the notification stays in the centre (as on iPhone). `normal` hides after 5 s, `high` stays until acted on. A newer notification replaces the shown one. |
| Lock screen | Unseen notifications are listed under the clock (newest three, then a "N more" pill). Tapping one asks for the passcode and opens it after unlock. |
| Notification centre | Pulled down from the top edge (the status bar) on the home screen or over an app. Grouped by app (newest group first, three cards each, then "N more"). Swipe a card sideways to dismiss it; the × at the top arms "Clear", a second tap clears all. Opening it marks everything seen (`notify.mark_seen`). Swipe up to close. |
| Ringing screen | `alarm_ringing`/`timer_ringing` (or a snapshot's `ringing`) turn the screen on and show a full-screen alarm (Snooze / Stop) or timer (Stop / Repeat), locked or not. The power key snoozes an alarm or stops a timer. |
| Status bar | A small alarm clock when an enabled alarm (or a snooze) is pending. |
| Wake | A `notification_posted` with `wake` turns the screen on through the power key's wake path; untouched, it goes off again after 10 s. |

120 Hz: every animation here redraws little. A banner moves a card-sized
region. The centre is a curtain whose content stays in place while its edge
follows the finger, so a frame redraws only the strip the edge crossed (the
compositor damages just the difference when an image's clip changes). The
ringing screen rises the same way. Over an app, the banner is a small shaped
override-redirect window moved by the server; the centre and the ringing
screen cover the app with its snapshot beneath.

Alarms and timers are set in the System app (Alarms & Timers), not in a
separate Clock app: the System app already has the page navigation, grouped
lists and Moon-keyboard text entry a label needs, so a Clock app would have
duplicated all of that for one page. It talks to the daemon directly
(`alarm.*`, `timer.*`); Moon does not need to be involved for an alarm to
ring.

Off the phone, `MOON_NOTIFY_SOCKET` points Moon and the System app at a
stand-in or a `moon-notifyd` run with `MOON_NOTIFY_ROOT`.
