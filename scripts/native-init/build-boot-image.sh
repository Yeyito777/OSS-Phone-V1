#!/usr/bin/env bash
# Build the stage-2 "oss-pid1" BOOT image: OSS Phone's own PID-1 pre-init in
# front of the proven direct-boot (Moon) image.
#
# Base is the CURRENT direct-boot image (the one the phone runs today):
#   images/moon-direct-boot-v1/boot-moon-direct-A266MUBU1AYB8.img
# whose ramdisk /init is magiskinit. This script:
#   - renames that /init to /init.magisk  (primary handoff, unchanged bytes),
#   - decompresses the Magisk stock-init backup (.backup/init.xz) to /init.stock
#     (fallback handoff),
#   - compiles scripts/native-init/oss-pid1.c statically for aarch64 and installs
#     it as the new ramdisk /init (the kernel's PID 1),
# leaving the kernel, magiskinit, the overlay.d Moon direct-boot files, and every
# other byte of the ramdisk untouched. AVB hash footer is re-added with the
# project key; top-level vbmeta stays verification-disabled, so this image boots
# on the unlocked device exactly like the base image does.
#
#   scripts/native-init/build-boot-image.sh OUT_DIR
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
OUT="${1:?output directory required}"
RESEARCH="${A26_RESEARCH:-$HOME/Workspace/research/linux-galaxy-a26}"
BASE="${A26_BASE_IMG:-$RESEARCH/images/moon-direct-boot-v1/boot-moon-direct-A266MUBU1AYB8.img}"
BASE_SHA256=3bf291627791e71ba00b4fcdde8b6bd92faa62d6fe2f14da9b906fd86619e9f6
MAGISKBOOT="${MAGISKBOOT:-$RESEARCH/tools/magiskboot-x86_64}"
CC="${A26_CC:-aarch64-linux-gnu-gcc}"
KEY="$RESEARCH/keys/recovery-avb-rsa4096.pem"
SALT=9413599eee2c3c4ad1a16b47eb3dc5036d93c908c656b18881680b979e1e547c
PARTITION_SIZE=$((16384 * 4096))
SOURCE="$HERE/oss-pid1.c"
NAME=boot-oss-pid1-A266MUBU1AYB8.img
# overlay.d Moon files that must survive unchanged from the base image.
OVERLAY_FILES=(overlay.d/moon-direct-boot.rc overlay.d/sbin/moon-boot-mode.sh \
    overlay.d/sbin/moon-session.sh overlay.d/sbin/moon-fallback.sh)

for tool in avbtool sha256sum xz "$CC"; do
    command -v "$tool" >/dev/null || { echo "Missing tool: $tool" >&2; exit 1; }
done
[[ -f "$BASE" ]] || { echo "Missing base image: $BASE" >&2; exit 1; }
[[ -f "$SOURCE" ]] || { echo "Missing source: $SOURCE" >&2; exit 1; }
printf '%s  %s\n' "$BASE_SHA256" "$BASE" | sha256sum -c - >/dev/null

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/unpack" "$WORK/final" "$OUT"

# Compile the pre-init: static, no-PIE, stripped, position-fixed EXEC.
"$CC" -static -no-pie -Os -s -Wall -Wextra -Werror "$SOURCE" -o "$WORK/oss-pid1"
[[ "$(od -An -tx1 -N4 "$WORK/oss-pid1" | tr -d ' ')" == 7f454c46 ]] || { echo "not ELF" >&2; exit 1; }

cp "$BASE" "$WORK/unpack/base.img"
(
    cd "$WORK/unpack"
    "$MAGISKBOOT" unpack base.img
    # Preserve the current /init (magiskinit) as the primary handoff target and
    # the stock-init backup as the fallback target.
    "$MAGISKBOOT" cpio ramdisk.cpio "extract init $WORK/init.magisk"
    "$MAGISKBOOT" cpio ramdisk.cpio "extract .backup/init.xz $WORK/init.stock.xz"
    xz -dc "$WORK/init.stock.xz" > "$WORK/init.stock"
    # Rebuild the ramdisk: magiskinit -> /init.magisk, stock init -> /init.stock,
    # our pre-init -> /init.
    "$MAGISKBOOT" cpio ramdisk.cpio \
        "mv init init.magisk" \
        "add 0750 init.stock $WORK/init.stock" \
        "add 0750 init $WORK/oss-pid1"
    "$MAGISKBOOT" repack base.img new.img
)

cp "$WORK/unpack/new.img" "$WORK/payload.img"
avbtool erase_footer --image "$WORK/payload.img"
[[ "$(tail -c 16 "$WORK/payload.img")" == SEANDROIDENFORCE ]]
cp "$WORK/payload.img" "$OUT/$NAME"
avbtool add_hash_footer --image "$OUT/$NAME" --partition_name boot \
    --partition_size "$PARTITION_SIZE" --algorithm SHA256_RSA4096 --key "$KEY" --salt "$SALT"

# Verify: kernel byte-identical to base; /init is our pre-init; /init.magisk is
# the base's /init byte for byte; overlay.d Moon files unchanged; init.stock is
# the decompressed Magisk backup.
(
    cd "$WORK/final"
    cp "$OUT/$NAME" final.img
    "$MAGISKBOOT" unpack final.img >/dev/null 2>&1
    mkdir base && (cd base && "$MAGISKBOOT" unpack "$WORK/unpack/base.img" >/dev/null 2>&1)
    cmp kernel base/kernel
    "$MAGISKBOOT" cpio ramdisk.cpio "extract init $WORK/check-init" >/dev/null 2>&1
    cmp "$WORK/check-init" "$WORK/oss-pid1"
    "$MAGISKBOOT" cpio ramdisk.cpio "extract init.magisk $WORK/check-magisk" >/dev/null 2>&1
    cmp "$WORK/check-magisk" "$WORK/init.magisk"
    "$MAGISKBOOT" cpio ramdisk.cpio "extract init.stock $WORK/check-stock" >/dev/null 2>&1
    cmp "$WORK/check-stock" "$WORK/init.stock"
    for f in "${OVERLAY_FILES[@]}"; do
        "$MAGISKBOOT" cpio ramdisk.cpio "extract $f $WORK/check-ov" >/dev/null 2>&1
        "$MAGISKBOOT" cpio base/ramdisk.cpio "extract $f $WORK/check-ov-base" >/dev/null 2>&1
        cmp "$WORK/check-ov" "$WORK/check-ov-base"
    done
)
[[ "$(stat -c %s "$OUT/$NAME")" -eq "$PARTITION_SIZE" ]]
avbtool info_image --image "$OUT/$NAME" >"$OUT/boot-avb-info.txt"
(cd "$OUT" && sha256sum "$NAME" >SHA256SUMS)

cat >"$OUT/README.txt" <<README
OSS-Phone-V1 stage-2 oss-pid1 BOOT for SM-A266M firmware A266MUBU1AYB8.

Base: boot-moon-direct-A266MUBU1AYB8.img (sha256 $BASE_SHA256), the direct-boot
image currently flashed. The ramdisk /init (magiskinit) was renamed to
/init.magisk; the Magisk stock-init backup was decompressed to /init.stock; and
scripts/native-init/oss-pid1.c was compiled statically and installed as /init,
OSS Phone's own PID-1 pre-init. It execs /init.magisk (reproducing today's
Magisk + Moon direct boot) and falls back to /init.stock (stock Android) if that
ever fails. Kernel, magiskinit, overlay.d Moon files and every other ramdisk
byte are verified unchanged. AVB hash footer re-added with the project key and
the base salt; top-level vbmeta stays verification-disabled.

Pre-init size: $(stat -c %s "$WORK/oss-pid1") bytes
README
cat "$OUT/SHA256SUMS"
