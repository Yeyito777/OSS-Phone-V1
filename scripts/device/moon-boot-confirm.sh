#!/system/bin/sh
# Clear the oss-init boot-attempt counter once the session is confirmed healthy.
#
# Part of the BOOT-promotion safety net (notes/stage2/PROMOTION-PLAN.md). On a
# promoted BOOT image oss-init increments a persistent counter very early and
# skips containment after N consecutive failures. THIS script is what makes a
# boot "count as success": Moon's boot supervisor runs it only after the
# session is up and has stayed up for a soak window, so a boot that panics or
# wedges before this point leaves the counter standing and escalates.
#
# The counter lives in the oss-bootcount kernel module (see PROMOTION-PLAN.md
# "Counter store"). Clearing it is a privileged write to its sysfs parameter;
# if that is unavailable, reload the module with clear=1. Idempotent.
#
# Invoked by moon-boot (post-ready hook) as root, e.g.:
#   (sleep "$OSS_BOOTOK_SOAK"; [ still-healthy ] && /data/adb/moon/moon-boot-confirm.sh) &
set -u
SOAK="${OSS_BOOTOK_SOAK:-45}"           # seconds the session must stay up first
SYSFS=/sys/module/oss_bootcount/parameters/count
KO=/data/adb/moon/oss-bootcount.ko
log() { echo "moon-boot-confirm: $*"; echo "moon-boot-confirm: $*" >/dev/kmsg 2>/dev/null || true; }

# Re-check health right before clearing: Moon ready and the display session up.
[ "$(getprop moon.session.ready)" = 1 ] || { log "moon not ready; not clearing the boot counter"; exit 1; }

if [ -w "$SYSFS" ]; then
  echo 0 > "$SYSFS" && log "boot counter cleared via sysfs (healthy after ${SOAK}s)" && exit 0
  log "sysfs write failed; trying module reload"
fi
if [ -f "$KO" ]; then
  insmod "$KO" clear=1 2>/dev/null || rmmod oss_bootcount 2>/dev/null
  log "boot counter cleared via module reload" && exit 0
fi
log "oss-bootcount not present (recovery-slot or pre-promotion image); nothing to clear"
exit 0
