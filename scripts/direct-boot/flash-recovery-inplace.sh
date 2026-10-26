#!/usr/bin/env bash
# Write (or restore) the RECOVERY partition from the running OS with root dd,
# with the same safety envelope as flash-boot-inplace.sh. BOOT is never touched.
#
#   scripts/direct-boot/flash-recovery-inplace.sh [options] IMAGE
#   scripts/direct-boot/flash-recovery-inplace.sh --restore-stock
#   scripts/direct-boot/flash-recovery-inplace.sh --check-bcb
#
#   --dry-run         run every check and make the backup, write nothing
#   --restore-stock   write the known-good stock recovery (9ab6cefe…) and exit
#   --check-bcb       print whether the misc BCB (first 4 KiB) is all zero, exit
#
# Only images on the whitelist below are accepted (the canary and stock
# recovery); anything else is refused. The image must be exactly the RECOVERY
# partition size, start with "ANDROID!", be boot header v2 (recovery is v2 on
# this device), and carry an AVB footer. Before writing, the misc BCB must be
# all zero (a stuck boot-recovery command is refused so a test can never start
# from an already-sticky state). The installed recovery is read back to
# /data/local/tmp and copied to $A26_RESCUE/inplace-backups, the image is
# written with dd+fsync+sync, then re-read with O_DIRECT and must match its
# sha256; on a mismatch the backup is written back and verified.
#
# A failed recovery boot is designed to return to BOOT on its own (see
# notes/stage2/CANARY-RUNBOOK.md). If recovery ever sticks, the escape is to
# restore stock recovery here plus clear the misc BCB (misc-bcb-clear.img),
# from Download mode if there is no adb.
set -euo pipefail

declare -A KNOWN=(
  [9ab6cefe06b18ddd7d2410f8ce06233fb0c14a39687fe085b4e74ad18a91a7e8]='stock recovery (rescue baseline)'
  # Recovery-slot test images are added here only once built against the
  # verify-after-write oss-rebootmode-clear.ko (masked mode clear,
  # require_mode). Images pinned to the first module (a18606a8) are withdrawn:
  # it reports success while leaving an unrecognised reboot-cmd (e.g. 0x49)
  # unchanged, so a failed slot boot could still loop. Withdrawn: 0e4e70b2
  # (canary), 583ad993 (slot moon), 84768b63 (slot stage 2), and earlier
  # 25540aa1, aab7a301, fc974667, 5a445db7, b67b68d3.
)

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
RESCUE="${A26_RESCUE:-$HOME/Desktop/a26-rescue}"
STOCK_RECOVERY="$RESCUE/A266MUBU1AYB8-current/recovery.img"
STOCK_SHA=9ab6cefe06b18ddd7d2410f8ce06233fb0c14a39687fe085b4e74ad18a91a7e8
PARTITION=/dev/block/by-name/recovery
PART_SIZE=100663296
WORK=/data/local/tmp/recovery-flash

DRY_RUN=0 RESTORE=0 CHECK_BCB=0 IMAGE=''
while (( $# )); do
  case "$1" in
    --dry-run) DRY_RUN=1 ;;
    --restore-stock) RESTORE=1 ;;
    --check-bcb) CHECK_BCB=1 ;;
    -h|--help) sed -n '2,37p' "$0"; exit 0 ;;
    -*) echo "unknown option: $1" >&2; exit 2 ;;
    *) [[ -z "$IMAGE" ]] || { echo "only one image" >&2; exit 2; }; IMAGE="$1" ;;
  esac; shift
done
SERIAL="$(a26_resolve_serial)"
phone() { adb -s "$SERIAL" shell "/data/local/tmp/su -c '$1'" | tr -d '\r'; }
fail() { echo "flash-recovery-inplace: $*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" 2>/dev/null | awk '{print $1}' || sha256sum "$1" | awk '{print $1}'; }

# 0 = all-zero (clean). Fails closed: anything but two real 64-hex hashes that
# match (adb error, su failure, quoting slip) counts as NOT clean. No single
# quotes inside: phone() already wraps the command in them.
ZERO4K_SHA=ad7facb2586fc6e966c004d7d1d16b024f5805ff7cb47c7a85dabd8b48892ca7
bcb_zero() {
  local h; h="$(phone "dd if=/dev/block/by-name/misc bs=4096 count=1 2>/dev/null | sha256sum | cut -c1-64")"
  [[ "$h" =~ ^[0-9a-f]{64}$ ]] || { echo "cannot read the misc BCB (got: ${h:0:80})" >&2; return 1; }
  [[ "$h" == "$ZERO4K_SHA" ]]
}

if (( CHECK_BCB )); then
  if bcb_zero; then echo "misc BCB: all zero (clean)"; exit 0
  else echo "misc BCB: NON-ZERO (a boot-recovery command may be stuck)"; exit 1; fi
fi

if (( RESTORE )); then
  IMAGE="$STOCK_RECOVERY"
  [[ -f "$IMAGE" ]] || fail "stock recovery not found: $IMAGE"
  [[ "$(sha256 "$IMAGE")" == "$STOCK_SHA" ]] || fail "stock recovery sha mismatch"
fi

[[ -f "$IMAGE" ]] || { echo "usage: $0 [--dry-run] IMAGE | --restore-stock | --check-bcb" >&2; exit 2; }

# Host-side format checks.
size="$(wc -c <"$IMAGE" | tr -d ' ')"
[[ "$size" == "$PART_SIZE" ]] || fail "image is $size bytes, RECOVERY is $PART_SIZE"
[[ "$(head -c 8 "$IMAGE")" == 'ANDROID!' ]] || fail 'not an Android boot image'
hv="$(od -A n -t u4 -j 40 -N 4 "$IMAGE" | tr -d ' ')"
[[ "$hv" == 2 ]] || fail "boot header version is $hv, expected 2 (recovery)"
[[ "$(tail -c 64 "$IMAGE" | head -c 4)" == AVBf ]] || fail 'no AVB footer'
image_sha="$(sha256 "$IMAGE")"
[[ -n "${KNOWN[$image_sha]:-}" ]] || fail "refusing an unknown recovery image (sha $image_sha)"
echo "image $IMAGE sha256=$image_sha (${KNOWN[$image_sha]})"

# A stuck BCB must be cleared first (so a test never starts already-sticky).
bcb_zero || fail "misc BCB is non-zero; clear it before writing recovery (see CANARY-RUNBOOK.md)"
echo "misc BCB: all zero (clean)"

PHONE_SCRIPT="$(mktemp)"; trap 'rm -f "$PHONE_SCRIPT"' EXIT
cat >"$PHONE_SCRIPT" <<'PHONE'
#!/system/bin/sh
set -u
PARTITION=/dev/block/by-name/recovery
WORK=/data/local/tmp/recovery-flash
BACKUPS=/data/adb/moon/recovery-backups
BB=/data/local/a26-linux/busybox.static
image_sha=$1 dry_run=$2
fail() { echo "phone: $*" >&2; exit 1; }
[ -x "$BB" ] || BB=''
dd_cmd() { if [ -n "$BB" ]; then "$BB" dd "$@"; else toybox dd "$@"; fi; }
[ "$(sha256sum "$WORK/new.img" | cut -d' ' -f1)" = "$image_sha" ] || fail 'pushed image hash mismatch'
[ "$(blockdev --getsize64 "$PARTITION")" = 100663296 ] || fail 'unexpected RECOVERY size'
[ "$(blockdev --getro "$PARTITION")" = 0 ] || fail "$PARTITION is read-only"
mkdir -p "$BACKUPS"; chmod 0700 "$BACKUPS"
dd_cmd if="$PARTITION" of="$WORK/current.img" bs=4194304 2>/dev/null || fail 'cannot read RECOVERY'
current_sha=$(sha256sum "$WORK/current.img" | cut -d' ' -f1)
backup="$BACKUPS/recovery-$(date +%Y%m%d-%H%M%S)-$(echo "$current_sha" | cut -c1-12).img"
cp "$WORK/current.img" "$backup" && chmod 0600 "$backup" || fail 'cannot write backup'
[ "$(sha256sum "$backup" | cut -d' ' -f1)" = "$current_sha" ] || fail 'backup verify failed'
ls -1t "$BACKUPS"/recovery-*.img | tail -n +4 | while read -r o; do rm -f "$o"; done
echo "BACKUP=$backup"; echo "CURRENT_SHA=$current_sha"
[ "$current_sha" != "$image_sha" ] || echo 'note: image is byte-identical to installed recovery'
[ "$dry_run" = 1 ] && { echo 'dry run: nothing written'; exit 0; }
dev=$(basename "$(readlink -f "$PARTITION")")
write_and_verify() {
  dd_cmd if="$1" of="$PARTITION" bs=4194304 conv=fsync 2>/dev/null || return 1
  sync; blockdev --flushbufs "$PARTITION" 2>/dev/null; echo 3 >/proc/sys/vm/drop_caches
  if [ -n "$BB" ]; then rb=$("$BB" dd if="$PARTITION" bs=4194304 iflag=direct 2>/dev/null | sha256sum | cut -d' ' -f1)
  else rb=$(toybox dd if="$PARTITION" bs=4194304 2>/dev/null | sha256sum | cut -d' ' -f1); fi
  echo "READBACK_SHA=$rb"; [ "$rb" = "$2" ]; }
if write_and_verify "$WORK/new.img" "$image_sha"; then echo 'RESULT=written-and-verified'; exit 0; fi
echo 'read-back mismatch: restoring backup' >&2
write_and_verify "$backup" "$current_sha" && fail 'write failed; backup restored and verified'
fail 'WRITE FAILED AND RESTORE DID NOT VERIFY: do not reboot; reflash from Download mode'
PHONE

phone "rm -rf $WORK" >/dev/null
adb -s "$SERIAL" shell "mkdir -p $WORK" >/dev/null
adb -s "$SERIAL" push "$IMAGE" "$WORK/new.img" >/dev/null 2>&1 || fail 'cannot push image'
adb -s "$SERIAL" push "$PHONE_SCRIPT" "$WORK/flash.sh" >/dev/null 2>&1 || fail 'cannot push helper'
set +e; output="$(phone "sh $WORK/flash.sh $image_sha $DRY_RUN" 2>&1)"; status=$?; set -e
echo "$output"
backup="$(sed -n 's/^BACKUP=//p' <<<"$output")"
if [[ -n "$backup" ]]; then
  mkdir -p "$RESCUE/inplace-backups"
  phone "cp $backup $WORK/backup.img && chmod 0644 $WORK/backup.img" >/dev/null
  adb -s "$SERIAL" pull "$WORK/backup.img" "$RESCUE/inplace-backups/${backup##*/}" >/dev/null 2>&1 ||
    fail "cannot copy backup to host"
  exp="$(sed -n 's/^CURRENT_SHA=//p' <<<"$output")"
  [[ "$(sha256 "$RESCUE/inplace-backups/${backup##*/}")" == "$exp" ]] || fail "host backup mismatch"
  echo "host backup: $RESCUE/inplace-backups/${backup##*/}"
fi
phone "rm -rf $WORK" >/dev/null
grep -q '^RESULT=written-and-verified$' <<<"$output" || {
  (( DRY_RUN )) && [[ $status == 0 ]] && exit 0
  fail "not written (status $status)"; }
echo "RECOVERY written and verified sha256=$image_sha. BOOT was not touched."
