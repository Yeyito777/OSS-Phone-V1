# On-device run, 2026-10-27 00:31–00:34: stage-2 v5 (`4c0ea361`) — PASS

No hands. BOOT `8c0461ee` never written; misc BCB all zero before and after;
RECOVERY write read-back verified; stock recovery `9ab6cefe` restored and
verified; back on BOOT (`PINFORM = 12345670 ... REBOOT_MODE 0`), Moon ready, no
slot marker. Logs: `~/Desktop/a26-rescue/stage2v5-run-20261027/`.

## Evidence
- sec_log (global kernel log):
  ```
  oss-rebootmode-clear: before: reboot-cmd[0x810]=0x9 (mode 1) ... panic-inform[0x80c]=0x25b7ce71
  oss-rebootmode-clear: after:  reboot-cmd[0x810]=0x8 (mode 0) ... panic-inform[0x80c]=0x12345670
  oss-rebootmode-clear: OK: NORMAL boot selected (cleared a reboot mode)
  [0.721724] init: 1   oss-init: OSS-Phone init is the real PID 1 (v5, private-rootfs containment)
  [0.721736] init: 1   oss-init: PMU reboot state set to NORMAL (oss-rebootmode-clear loaded)
  [6.344662] init: 1   oss-init: container is live (Android second stage reached); oss-init supervising as the real PID 1
  [7.183786] init: 198 OSSPHONE_CONTAIN_LIVE
  ```
  "Live" fired at 6.34 s, before the marker (7.18 s): the new denied-read
  signal (the container's SELinux policy load). Only 2 kernel-domain kmsg read
  denials in the log (v4 flooded them every 200 ms).
- Namespace: Android's init logs as host PID 198 (7 214 lines) and is PID 1
  in its own namespace (`NSpid: 1` from inside; adb runs inside the container).
- Mali: loaded `mali_kbase` srcversion `CB269BB86C272D6570FC2B5` = the patched
  module (stock vendor copy is `F56CED9D912AABBA3E46544`). 0 `Failed to get pid
  pointer` lines.
- SurfaceFlinger: started once at 12.77 s, RenderEngine `vendor: ARM`,
  `renderer: Mali-G68`, **0 crashes** (v4: crash every ~5 s, `no suitable
  EGLConfig`). Stopped afterwards by Moon's takeover, as in a normal boot.
- Moon: `Xorg ready`, `Moon is the active default session` at 22 s;
  `moon.session.ready=1`, `moon.boot.mode=moon`; moon-compositor, Xorg,
  moon-audiod/notifyd/telephonyd/permissiond running.

## Minor finding
`adb reboot` (from inside the container) was handled by oss-init as
`container failed after sec_reboot loaded; controlled reboot to BOOT` rather
than `contained Android requested reboot/poweroff; performing it`: Android's
init evidently exits normally after its shutdown sequence instead of being
SIGHUP'd by `reboot(2)` in its PID namespace. The outcome is correct (clean
reboot to BOOT); the reboot reason string and the power-off-vs-reboot
distinction are lost on this path. Follow-up: tell the two apart (e.g. read
`sys.powerctl` intent before the container dies) so a contained power-off
powers off.
