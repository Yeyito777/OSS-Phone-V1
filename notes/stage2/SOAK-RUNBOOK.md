# Soak runbook: oss-init v7 from the recovery slot (gate for BOOT promotion)

Everything runs from the **recovery slot**, where any failure still returns to
BOOT on its own (watchdog path proven 2026-10-26). BOOT `8c0461ee` is never
written. Promotion (PROMOTION-PLAN.md §4) is gated on every item below passing.
Record results in `notes/stage2/SOAK-<date>.md`; logs go to
`~/Desktop/a26-rescue/soak-<date>/`.

## Artifacts
| File | sha256 | What |
| --- | --- | --- |
| `images/oss-stage2/recovery-slot/recovery-slot-stage2-v7-A266MUBU1AYB8.img` | `0381b5ab…acab06ab` | v7 slot image (patched Mali, misc boot counter) |
| `images/oss-stage2/recovery-slot/oss-init-v7` | `db6b8679…81c2fc04` | the same binary as the image's `/init`; installed as `/data/adb/moon/oss-init` (counter CLI) |
| `scripts/device/moon-boot-confirm.sh` | — | clears the count after a healthy contained session |
| `images/oss-stage2/canary/reboot-recovery` | `89c036e0…a823c40e` | BCB-free entry into the slot |

Shorthand: `S='adb -s R5CY323ES7X shell /data/local/tmp/su -c'`;
`FR=scripts/direct-boot/flash-recovery-inplace.sh`.

## 0. Preconditions (every session)
- The phone is ours; USB to the Mac; battery ≥ 50 %. The Download-mode escape
  (CANARY-RUNBOOK.md) stays the last resort, though no item needs hands except
  0.6 and S8b/S9d (power-on after a power-off).
- `$FR --check-bcb` → all zero; BOOT `8c0461ee`, RECOVERY `9ab6cefe` (stock).

## 0. One-time setup
1. **Misc must be as the kernel agent measured.** Read-only:
   `$S 'dd if=/dev/block/by-name/misc bs=4096 skip=64 count=2 2>/dev/null | sha256sum'`
   → `9f1dcbc3…913d47` (8 KiB of zeros). Anything else: **stop** (someone uses
   the slots). Back up the whole misc to the Mac:
   `adb exec-out "su -c 'dd if=/dev/block/by-name/misc bs=4096 2>/dev/null'" > a26-rescue/soak-<date>/misc-before.img`.
2. Install the counter CLI (the exact `/init` binary):
   `adb push images/oss-stage2/recovery-slot/oss-init-v7 /data/local/tmp/` then
   `$S 'cp /data/local/tmp/oss-init-v7 /data/adb/moon/oss-init && chown 0:0 /data/adb/moon/oss-init && chmod 0700 /data/adb/moon/oss-init && sha256sum /data/adb/moon/oss-init'` → `db6b8679…`.
3. `$S '/data/adb/moon/oss-init --bootcount show'` →
   `status=none slot=- seq=0 count=0 wrote=0 slotA=0 slotB=0 direct=1`, exit 0.
   It must say `direct=1` (O_DIRECT on the real partition).
4. Install the clear hook: `scripts/a26-shell/install-autostart.sh` (ships the
   `moon-post-ready.sh` with the guarded hook), then push
   `scripts/device/moon-boot-confirm.sh` to `/data/adb/moon/` (root, 0700).
   Both are inert on an ordinary boot: no `/debug_ramdisk/oss-contained` marker.
5. `reboot-recovery` on the phone (`/data/local/tmp/reboot-recovery`, 0755,
   `89c036e0…`).
6. `$FR images/oss-stage2/recovery-slot/recovery-slot-stage2-v7-A266MUBU1AYB8.img`
   (backs up stock recovery, writes, reads back). RECOVERY stays v7 for the
   whole soak; `$FR --restore-stock` at the end.

## Checks after every slot boot ("standard check")
1. `$FR --check-bcb` → all zero.
2. `adb exec-out "su -c 'cat /proc/sec_log'" > …/bootN.sec_log`, then grep for
   the following lines. They should appear in order:
   - `oss-rebootmode-clear: … OK: NORMAL boot selected (cleared a reboot mode)`;
   - `oss-init: OSS-Phone init is the real PID 1 (v7, …)`;
   - `oss-init: bootcount status=ok count=1 found=0 wrote=1 froze=1 promoted=0`,
     at ~1.9–2.5 s, before the container's switch root;
   - `oss-init: container is live …`.

   There must be **no** `skipping containment`, `fallback` or `failed` line.
3. `moon.session.ready=1`. `/debug_ramdisk/oss-contained` and
   `/debug_ramdisk/oss-recovery-slot` exist.
4. Wait at least 70 s, then:
   - `boot-confirm.log` ends with `bootcount: status=ok … count=0 wrote=1`;
   - `oss-init --bootcount show` reports `count=0`.
5. Misc is unchanged outside the slots: dump it again and compare with
   `misc-before.img`, excluding `0x40000`–`0x41fff`
   (`scripts/native-init/test/misc-tool.py check NOW misc-before.img` →
   `outside=UNCHANGED`).
6. The containment is intact:
   - Android's init logs as a host PID other than 1 in sec_log;
   - `mali` "Failed to get pid pointer" appears 0 times;
   - logcat has 0 `surfaceflinger native crash` lines.

## Items
**S1 — Repeated boots (≥ 10).** Each iteration:
1. From BOOT, run `$S 'sync; /data/local/tmp/reboot-recovery'`, which boots the
   v7 slot.
2. Do the standard check.
3. `adb reboot` from inside the container. This is the contained reboot handback:
   - sec_log should show `contained Android requested reboot; rebooting to BOOT`;
   - the phone lands on BOOT (`PINFORM 12345670 → REBOOT_MODE 0`).

Every iteration passes and the count returns to 0 each time.
(`reboot-recovery` from *inside* the container is also a contained reboot. It
lands on BOOT, not the slot, because oss-init performs the reboot itself.)

**S2 — Screen on/off.** At least 10 lock/timeout cycles, using power-key
events and `moon-suspend-cycle.sh` with an RTC test of 10–30 s. Pass if:
- the display wakes every time;
- `suspend.log` shows complete cycles;
- SurfaceFlinger crashes stay at 0 and Mali errors at 0.

**S3 — Audio.** `moon-audiod` and the audio bridge are running. Playback (a
notification sound or the ringtone preview) is audible before and after a
screen-off/on cycle. Pass if `audio` lines in `boot.log` show `ready`, with no
restarts.

**S4 — Wi-Fi.** The post-ready native Wi-Fi associates and gets DHCP:
- `ip addr show wlan0` has an address;
- the network-time sync succeeds;
- `ping -c3 1.1.1.1` works;
- it still holds after a suspend/resume (S2).

**S5 — Telephony.** `moon-telephonyd` is up. RIL connects, with no endless
`RILClient … Connection refused` after boot. SIM state is sane, or the compact
no-SIM status shows. Airplane mode on/off works.

**S6 — Notifications and alarms.**
- Post a notification; it shows.
- Set an alarm and a timer 3–5 min out. Both fire with the screen off.
- `notifyd.log` shows them.

This exercises wakelocks and the RTC from inside the container.

**S7 — Long uptime (≥ 2 h idle).**
- No oss-init line after `container is live`.
- No SurfaceFlinger restart, no OOM kill in the container, no reboot.
- The final standard check passes.

**S8 — Android handback inside containment.**
- **a.** Contained reboot: covered by S1.
- **b.** Contained power-off (attended: needs a hand to power on).
  `$S 'reboot -p'` → sec_log (read on the next boot) shows `contained Android
  requested power-off; powering off`, and the phone is actually off (not
  rebooting).
- **c.** Contained `reboot recovery` / `reboot bootloader`: expected to land on
  BOOT (oss-init reboots with no reason string; the reason channel is not built,
  PROMOTION-PLAN §1). Record what happens.
- **d.** A container crash is covered by the bench (`layout-crash`, `late-reboot`).
  It is not forced on the device: the container's init cannot be signalled
  from inside its namespace.

**S9 — The counter.**
- **a.** Clear after health: covered by every standard check.
- **b.** Escalation at 3, from the slot:
  1. In a BOOT session run `oss-init --bootcount bump` twice → `count=2`.
  2. Run `reboot-recovery`. The slot boot bumps to 3, and sec_log shows
     `skipping containment (consecutive failed boots reached the limit);
     booting magiskinit as /init`.
  3. Moon comes up **uncontained**: there is no
     `/debug_ramdisk/oss-contained`, and `boot-confirm` does not run.
  4. `show` still reports `count=3`.
  5. Run `oss-init --bootcount clear` → `count=0`. The next slot boot contains
     again.
- **c.** Torn/foreign slots, the promoted-mode escalations, the wrong size and
  an ambiguous `misc`: these are **bench-only**
  (`test/run-bootcount-tests.sh`, 131 checks). Writing foreign bytes into the
  phone's misc would break the misc write rules.
- **d.** Persistence across a cold power-off (attended):
  1. `bump` → `count=1`.
  2. Power off, then power on into BOOT.
  3. `show` still reports `count=1`.
  4. `clear`.

## Teardown
- `oss-init --bootcount show`, then `clear` if the count is not 0.
- `$FR --restore-stock`, then verify RECOVERY is `9ab6cefe` and reboot to BOOT.
- Check that misc outside the slots equals `misc-before.img`.
- `/data/adb/moon/oss-init` and `moon-boot-confirm.sh` may stay. They are inert
  without the marker, and promotion requires them anyway.

## Stop conditions (stop and report)
- Any `skipping containment` not caused on purpose by S9b.
- Any change in misc outside `0x40000`–`0x41fff`.
- A bootcount status other than `ok` on the device.
- A boot that does not come back hands-free, or BCB not zero.
