# Power: shutdown, reboot and low battery in Moon mode

Moon (the UI) asks; the device side (Android's root namespace, where the boot
supervisor runs) does. Moon never runs `reboot` itself: its process is one of
the things that has to stop first.

## Power off / reboot

```sh
/opt/a26-shell/bin/moon-power poweroff     # or: reboot
```

Run inside the chroot, as root (Moon, `a26-shellctl`, scripts over adb). It
validates the argument, starts the device-side sequence detached (its own
session, outside the chroot through `/proc/1/root`, the same way Moon starts
the deep-suspend helper) and returns at once with status 0, or 2 for a bad
argument. The sequence then stops Moon itself, so the caller shows its
"Shutting down…" / "Restarting…" screen **before** calling and expects to be
killed within a second or two. There is no reply and no cancel.

The device side, `/data/adb/moon/moon-power.sh poweroff|reboot`, logs to
`/data/adb/moon/power.log` and runs once (a second request while one runs is
ignored):

1. Sets `moon.boot.mode=shutdown`: nothing restarts or falls back to Android
   from here on (the boot supervisor, the notifyd and audio keepers and the
   init fallback trigger all stand down), and stops the boot supervisor.
2. Stops, in order, each bounded (TERM, then KILL after a few seconds):
   Moon (`quit` over its control socket first), moon-notifyd, moon-telephonyd
   (`moon-telephony-stop.sh`, which takes mobile data down), the audio keeper
   and native audio (`moon-audio-stop.sh`, without the Android-handback
   audioserver restart), native Wi-Fi (`a26-wifi-stop`), then Xorg and
   moon-compositor.
3. `sync`, then a normal Android shutdown: `setprop sys.powerctl shutdown` or
   `reboot` (init stops the remaining services, unmounts and calls the
   kernel with no special reason, so the next boot is a normal boot: the PMU
   reboot mode stays NORMAL and the direct-boot policy starts Moon again). If
   the phone is still up 30 s later: `/system/bin/reboot [-p]`.

`moon-power.sh` also works from adb (`su -c 'sh /data/adb/moon/moon-power.sh
reboot'`).

## Low battery

The device side owns the policy and the thresholds (`/data/adb/moon/config`);
Moon shows notifications and needs no logic of its own. The supervisor checks
every 30 s while Moon runs; "charging" means USB or AC power is online.

| Battery (not charging) | What happens |
| --- | --- |
| ≤ `BATTERY_WARN` (20 %) | moon-notifyd notification, app `moon`, tag `battery-low`, urgency `normal`: "Battery low" / "20% remaining" |
| ≤ `BATTERY_LOW` (10 %) | the same tag replaced, urgency `high` (wakes the screen): "Battery very low" / "10% remaining" |
| ≤ `BATTERY_CRITICAL` (3 %) | tag `battery-critical`, urgency `high`, `expires_at` = the shutdown time (now + `CRITICAL_GRACE`, 15 s): "Shutting down" / "Battery critically low". Moon may count down to `expires_at`. Then `moon-power.sh poweroff`, unless a charger is connected meanwhile: then the notification is withdrawn and nothing happens |

Each warning is posted once per discharge; plugging in a charger withdraws
`battery-low` and re-arms the warnings. 3 % (not 0 %) leaves the fuel gauge's
error and the shutdown itself some margin. Moon does not fall back to Android
on low battery any more (the old `LOW_BATTERY_FALLBACK`).

At boot the direct-boot policy starts Moon only above `MIN_START_BATTERY`
(5 %, above the critical level, so a phone switched on at 4 % cannot loop
through Moon start and critical shutdown) or with a charger connected;
otherwise it boots stock Android, which handles its own low battery.

## Charging while off

Plugging a charger into a phone that is off boots Samsung's off-mode
charging (LPM: `ro.bootmode=charger`). The direct-boot policy returns
`android` for it before anything else (no attempt counted, nothing of Moon
started), so the stock charging screen runs; a long press of the power key
boots normally, into Moon.
