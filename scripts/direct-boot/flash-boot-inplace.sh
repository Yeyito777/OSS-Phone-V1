#!/usr/bin/env bash
# Write a BOOT image from the running OS (Android or Moon) with root dd: no
# Download mode, no button presses. The default flash path for ramdisk-only
# images from build-boot-image.sh.
#
#   scripts/direct-boot/flash-boot-inplace.sh [options] IMAGE
#
#   --dry-run              run every check and make the backup, write nothing
#   --allow-kernel-change  accept a kernel that differs from the installed one
#   --allow-init-change    accept a ramdisk /init (magiskinit) that differs
#
# Refuses an image whose size is not exactly the partition's, that is not a
# boot v4 image ("ANDROID!", header version 4) with an AVB footer, or whose
# kernel or Magisk init differ from the installed image (unless allowed). The
# installed image is first read back into /data/adb/moon/boot-backups (newest
# three kept) and copied to the host under $A26_RESCUE/inplace-backups. After
# dd + fsync + sync, the partition is re-read with O_DIRECT and must match the
# image's sha256; on a mismatch the backup is written back and verified.
#
# A wrong-but-booting image is fixed by running this again with the backup. An
# image that does not boot needs Download mode (notes/direct-boot/RESCUE.md).
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
RESCUE="${A26_RESCUE:-$HOME/Desktop/a26-rescue}"
PARTITION=/dev/block/by-name/boot
WORK=/data/local/tmp/boot-flash

DRY_RUN=0
ALLOW_KERNEL=0
ALLOW_INIT=0
IMAGE=''
while (( $# )); do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --allow-kernel-change) ALLOW_KERNEL=1 ;;
        --allow-init-change) ALLOW_INIT=1 ;;
        -h|--help) sed -n '2,21p' "$0"; exit 0 ;;
        -*) echo "unknown option: $1" >&2; exit 2 ;;
        *) [[ -z "$IMAGE" ]] || { echo "only one image" >&2; exit 2; }; IMAGE="$1" ;;
    esac
    shift
done
[[ -f "$IMAGE" ]] || { echo "usage: $0 [--dry-run] [--allow-kernel-change] [--allow-init-change] IMAGE" >&2; exit 2; }
SERIAL="$(a26_resolve_serial)"
phone() { adb -s "$SERIAL" shell "/data/local/tmp/su -c '$1'" | tr -d '\r'; }

fail() { echo "flash-boot-inplace: $*" >&2; exit 1; }
sha256() { shasum -a 256 "$1" 2>/dev/null | awk '{print $1}' || sha256sum "$1" | awk '{print $1}'; }

# Host-side format checks.
size="$(wc -c <"$IMAGE" | tr -d ' ')"
partition_size="$(phone "blockdev --getsize64 $PARTITION")"
[[ "$partition_size" =~ ^[0-9]+$ ]] || fail "cannot read the size of $PARTITION"
[[ "$size" == "$partition_size" ]] || fail "image is $size bytes, partition is $partition_size"
[[ "$(head -c 8 "$IMAGE")" == 'ANDROID!' ]] || fail 'not an Android boot image (no ANDROID! magic)'
header_version="$(od -A n -t u4 -j 40 -N 4 "$IMAGE" | tr -d ' ')"
[[ "$header_version" == 4 ]] || fail "boot header version is $header_version, expected 4"
[[ "$(tail -c 64 "$IMAGE" | head -c 4)" == AVBf ]] || fail 'no AVB footer at the end of the image'
image_sha="$(sha256 "$IMAGE")"
echo "image $IMAGE sha256=$image_sha ($size bytes, boot v$header_version, AVB footer)"

PHONE_SCRIPT="$(mktemp)"
trap 'rm -f "$PHONE_SCRIPT"' EXIT
cat >"$PHONE_SCRIPT" <<'PHONE'
#!/system/bin/sh
# Phone side of flash-boot-inplace.sh: compare, back up, write, verify.
set -u
PARTITION=/dev/block/by-name/boot
WORK=/data/local/tmp/boot-flash
BACKUPS=/data/adb/moon/boot-backups
MAGISKBOOT=/data/adb/magisk/magiskboot
BB=/data/local/a26-linux/busybox.static
image_sha=$1 dry_run=$2 allow_kernel=$3 allow_init=$4
fail() { echo "phone: $*" >&2; exit 1; }
[ -x "$BB" ] || BB=''
dd_cmd() { if [ -n "$BB" ]; then "$BB" dd "$@"; else toybox dd "$@"; fi; }

[ "$(sha256sum "$WORK/new.img" | cut -d' ' -f1)" = "$image_sha" ] || fail 'pushed image hash mismatch'
size=$(blockdev --getsize64 "$PARTITION")
[ "$(blockdev --getro "$PARTITION")" = 0 ] || fail "$PARTITION is read-only"

# Back up the installed image (newest three kept on the phone).
mkdir -p "$BACKUPS"; chmod 0700 "$BACKUPS"
dd_cmd if="$PARTITION" of="$WORK/current.img" bs=4194304 2>/dev/null || fail 'cannot read the partition'
current_sha=$(sha256sum "$WORK/current.img" | cut -d' ' -f1)
backup="$BACKUPS/boot-$(date +%Y%m%d-%H%M%S)-$(echo "$current_sha" | cut -c1-12).img"
cp "$WORK/current.img" "$backup" && chmod 0600 "$backup" || fail 'cannot write the backup'
[ "$(sha256sum "$backup" | cut -d' ' -f1)" = "$current_sha" ] || fail 'backup verification failed'
ls -1t "$BACKUPS"/boot-*.img | tail -n +4 | while read -r old; do rm -f "$old"; done
echo "BACKUP=$backup"
echo "CURRENT_SHA=$current_sha"

# Kernel and Magisk init must be byte-identical to the installed image.
mkdir -p "$WORK/cur" "$WORK/new"
(cd "$WORK/cur" && "$MAGISKBOOT" unpack ../current.img >/dev/null 2>&1) || fail 'cannot unpack the installed image'
(cd "$WORK/new" && "$MAGISKBOOT" unpack ../new.img >/dev/null 2>&1) || fail 'cannot unpack the new image'
for part in cur new; do
    [ -s "$WORK/$part/kernel" ] || fail "no kernel in the $part image"
    (cd "$WORK/$part" && "$MAGISKBOOT" cpio ramdisk.cpio "extract init init" >/dev/null 2>&1) ||
        fail "no /init in the $part ramdisk"
done
if cmp -s "$WORK/cur/kernel" "$WORK/new/kernel"; then
    echo 'kernel: identical'
elif [ "$allow_kernel" = 1 ]; then
    echo 'kernel: DIFFERENT (allowed by --allow-kernel-change)'
else
    fail 'kernel differs from the installed image (use --allow-kernel-change)'
fi
if cmp -s "$WORK/cur/init" "$WORK/new/init"; then
    echo 'magisk init: identical'
elif [ "$allow_init" = 1 ]; then
    echo 'magisk init: DIFFERENT (allowed by --allow-init-change)'
else
    fail 'ramdisk /init differs from the installed image (use --allow-init-change)'
fi
[ "$current_sha" != "$image_sha" ] || echo 'note: the image is byte-identical to the installed one'
[ "$dry_run" = 1 ] && { echo 'dry run: nothing written'; exit 0; }

write_and_verify() {
    src=$1 expected=$2
    before=$(awk '{print $7}' "/sys/class/block/$dev/stat")
    dd_cmd if="$src" of="$PARTITION" bs=4194304 conv=fsync 2>/dev/null || return 1
    sync
    blockdev --flushbufs "$PARTITION" 2>/dev/null
    echo 3 >/proc/sys/vm/drop_caches
    after=$(awk '{print $7}' "/sys/class/block/$dev/stat")
    echo "sectors written to $dev: $((after - before))"
    if [ -n "$BB" ]; then
        readback=$("$BB" dd if="$PARTITION" bs=4194304 iflag=direct 2>/dev/null | sha256sum | cut -d' ' -f1)
    else
        readback=$(toybox dd if="$PARTITION" bs=4194304 2>/dev/null | sha256sum | cut -d' ' -f1)
    fi
    echo "READBACK_SHA=$readback"
    [ "$readback" = "$expected" ]
}
dev=$(basename "$(readlink -f "$PARTITION")")
if write_and_verify "$WORK/new.img" "$image_sha"; then
    echo 'RESULT=written-and-verified'
    exit 0
fi
echo 'read-back mismatch: restoring the backup' >&2
if write_and_verify "$backup" "$current_sha"; then
    fail 'flash failed; the previous image was restored and verified'
fi
fail 'FLASH FAILED AND RESTORE DID NOT VERIFY: do not reboot; retry with the backup'
PHONE
phone "rm -rf $WORK" >/dev/null
adb -s "$SERIAL" shell "mkdir -p $WORK" >/dev/null
adb -s "$SERIAL" push "$IMAGE" "$WORK/new.img" >/dev/null 2>&1 || fail 'cannot push the image'
adb -s "$SERIAL" push "$PHONE_SCRIPT" "$WORK/flash.sh" >/dev/null 2>&1 || fail 'cannot push the helper'

set +e
output="$(phone "sh $WORK/flash.sh $image_sha $DRY_RUN $ALLOW_KERNEL $ALLOW_INIT" 2>&1)"
status=$?
set -e
echo "$output"
backup="$(sed -n 's/^BACKUP=//p' <<<"$output")"
if [[ -n "$backup" ]]; then
    mkdir -p "$RESCUE/inplace-backups"
    # /data/adb is root-only; stage the copy where adb can read it.
    phone "cp $backup $WORK/backup.img && chmod 0644 $WORK/backup.img" >/dev/null
    adb -s "$SERIAL" pull "$WORK/backup.img" "$RESCUE/inplace-backups/${backup##*/}" >/dev/null 2>&1 ||
        fail "cannot copy the backup to the host ($backup)"
    expected="$(sed -n 's/^CURRENT_SHA=//p' <<<"$output")"
    [[ "$(sha256 "$RESCUE/inplace-backups/${backup##*/}")" == "$expected" ]] ||
        fail "host copy of the backup does not match ($backup)"
    echo "host backup: $RESCUE/inplace-backups/${backup##*/}"
fi
phone "rm -rf $WORK" >/dev/null
grep -q '^RESULT=written-and-verified$' <<<"$output" || {
    (( DRY_RUN )) && [[ $status == 0 ]] && exit 0
    fail "not flashed (status $status)"
}
echo "Flashed and verified sha256=$image_sha. It takes effect on the next reboot."
