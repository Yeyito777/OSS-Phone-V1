#!/usr/bin/env bash
# Build the direct-boot BOOT image: the proven Magisk v30.7 image for
# A266MUBU1AYB8 plus this directory's overlay.d (one init rc and three small
# failsafe scripts). Kernel, Magisk payload and every other byte of the ramdisk
# are unchanged. Runs on the research host, which holds magiskboot, avbtool and
# the project's AVB key (vbmeta has verification disabled; the hash footer only
# keeps the image layout identical to the flashed one).
#
#   scripts/direct-boot/build-boot-image.sh OUT_DIR
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:?output directory required}"
RESEARCH="${A26_RESEARCH:-$HOME/Workspace/research/linux-galaxy-a26}"
BASE="$RESEARCH/images/magisk-v30.7-A266MUBU1AYB8/boot-magisk-v30.7-A266MUBU1AYB8.img"
BASE_SHA256=365a8b3c0d59f94a58a89c4f35f2a5d774f643452a8f07a6ac60d4cfc12e08a0
MAGISKBOOT="${MAGISKBOOT:-$RESEARCH/tools/magiskboot-x86_64}"
KEY="$RESEARCH/keys/recovery-avb-rsa4096.pem"
SALT=9413599eee2c3c4ad1a16b47eb3dc5036d93c908c656b18881680b979e1e547c
PARTITION_SIZE=$((16384 * 4096))
NAME=boot-moon-direct-A266MUBU1AYB8.img
OVERLAY_FILES=(moon-direct-boot.rc sbin/moon-boot-mode.sh sbin/moon-session.sh sbin/moon-fallback.sh)

for tool in avbtool sha256sum; do
    command -v "$tool" >/dev/null || { echo "Missing tool: $tool" >&2; exit 1; }
done
printf '%s  %s\n' "$BASE_SHA256" "$BASE" | sha256sum -c - >/dev/null
for f in "${OVERLAY_FILES[@]}"; do
    [[ -f "$HERE/overlay.d/$f" ]] || { echo "Missing overlay file: $f" >&2; exit 1; }
done
for f in "$HERE"/overlay.d/sbin/*.sh; do sh -n "$f"; done

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/unpack" "$WORK/final" "$OUT"
cp "$BASE" "$WORK/unpack/base.img"
(
    cd "$WORK/unpack"
    "$MAGISKBOOT" unpack base.img
    "$MAGISKBOOT" cpio ramdisk.cpio \
        "add 0644 overlay.d/moon-direct-boot.rc $HERE/overlay.d/moon-direct-boot.rc" \
        "add 0755 overlay.d/sbin/moon-boot-mode.sh $HERE/overlay.d/sbin/moon-boot-mode.sh" \
        "add 0755 overlay.d/sbin/moon-session.sh $HERE/overlay.d/sbin/moon-session.sh" \
        "add 0755 overlay.d/sbin/moon-fallback.sh $HERE/overlay.d/sbin/moon-fallback.sh"
    "$MAGISKBOOT" repack base.img new.img
)
cp "$WORK/unpack/new.img" "$WORK/payload.img"
avbtool erase_footer --image "$WORK/payload.img"
[[ "$(tail -c 16 "$WORK/payload.img")" == SEANDROIDENFORCE ]]
cp "$WORK/payload.img" "$OUT/$NAME"
avbtool add_hash_footer --image "$OUT/$NAME" --partition_name boot \
    --partition_size "$PARTITION_SIZE" --algorithm SHA256_RSA4096 --key "$KEY" --salt "$SALT"

# Verify: same kernel, Magisk ramdisk intact, overlay files byte-exact.
(
    cd "$WORK/final"
    cp "$OUT/$NAME" final.img
    "$MAGISKBOOT" unpack final.img >/dev/null 2>&1
    mkdir base && (cd base && "$MAGISKBOOT" unpack "$WORK/unpack/base.img" >/dev/null 2>&1)
    cmp kernel base/kernel
    set +e
    "$MAGISKBOOT" cpio ramdisk.cpio test
    status=$?
    set -e
    [[ $status -eq 1 ]]
    for f in "${OVERLAY_FILES[@]}"; do
        "$MAGISKBOOT" cpio ramdisk.cpio "extract overlay.d/$f $WORK/check" >/dev/null 2>&1
        cmp "$WORK/check" "$HERE/overlay.d/$f"
    done
    "$MAGISKBOOT" cpio ramdisk.cpio "extract init $WORK/init-new" >/dev/null 2>&1
    "$MAGISKBOOT" cpio base/ramdisk.cpio "extract init $WORK/init-base" >/dev/null 2>&1
    cmp "$WORK/init-new" "$WORK/init-base"
)
[[ "$(stat -c %s "$OUT/$NAME")" -eq "$PARTITION_SIZE" ]]
avbtool info_image --image "$OUT/$NAME" >"$OUT/boot-avb-info.txt"
(cd "$OUT" && sha256sum "$NAME" >SHA256SUMS)
cat >"$OUT/README.txt" <<README
OSS-Phone-V1 direct-boot BOOT for SM-A266M firmware A266MUBU1AYB8.

Base: boot-magisk-v30.7-A266MUBU1AYB8.img (sha256 $BASE_SHA256), the image
currently flashed. Only overlay.d/moon-direct-boot.rc and overlay.d/sbin/
moon-{boot-mode,session,fallback}.sh were added to the ramdisk; kernel and
magiskinit are verified byte-identical. AVB hash footer re-added with the
project key and the same salt; top-level vbmeta stays verification-disabled.
README
cat "$OUT/SHA256SUMS"
