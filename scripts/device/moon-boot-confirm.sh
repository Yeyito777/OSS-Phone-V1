#!/system/bin/sh
# Clear oss-init's boot-attempt counter once a CONTAINED session is healthy.
#
# Part of the BOOT-promotion safety net (notes/stage2/PROMOTION-PLAN.md §2).
# oss-init bumps a counter in the misc partition on every boot it supervises
# and skips containment after 3 consecutive bumps without a clear. This script
# is what makes a boot count as a success. moon-post-ready.sh starts it only
# when /debug_ramdisk/oss-contained exists, a marker oss-init puts into the
# container's ramdisk copy. A boot where oss-init skipped containment (or any
# ordinary boot) has no marker, so its count is left alone: a skipped boot
# stays skipped until someone clears the count on purpose.
#
# The single writer of the record is oss-init itself
# (`oss-init --bootcount show|clear|init|bump`, format
# docs/bootcount-misc-record.md); this script never touches misc directly.
set -u
PERSIST=/data/adb/moon
OSS_INIT="$PERSIST/oss-init"
SOAK="${OSS_BOOTOK_SOAK:-60}"    # seconds Moon must stay up before the boot counts as healthy
MARK=/debug_ramdisk/oss-contained

log() { echo "moon-boot-confirm: $* [$(date --iso-8601=seconds 2>/dev/null || date) up=$(cut -d' ' -f1 /proc/uptime)]"; }

[ -e "$MARK" ] || { log "not a contained boot (no $MARK); boot count left alone"; exit 0; }
[ -x "$OSS_INIT" ] || { log "no $OSS_INIT; cannot clear the boot count"; exit 1; }
sleep "$SOAK"
if [ "$(getprop moon.session.ready)" != 1 ] || [ -z "$(pidof Xorg 2>/dev/null || true)" ]; then
    log "session not healthy after ${SOAK}s; boot count stays"
    exit 1
fi
out="$("$OSS_INIT" --bootcount clear 2>&1)"; rc=$?
log "$out (exit $rc)"
exit $rc
