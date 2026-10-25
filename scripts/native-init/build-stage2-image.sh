#!/usr/bin/env bash
# Build the stage-2 containment BOOT image: OSS Phone's own init (oss-init) as
# the real PID 1 on the OSS Phone kernel, with Android's init contained in a
# PID + mount namespace.
#
# Base is the OSS-kernel direct-boot image from the kernel agent:
#   boot-oss-mirror-A266MUBU1AYB8.img  (OSS kernel: PID_NS, DEVTMPFS, no DEFEX)
# whose ramdisk /init is magiskinit and which already carries the Moon
# direct-boot overlay. This script, leaving the kernel and every existing
# ramdisk byte untouched:
#   - renames /init (magiskinit) to /init.magisk   (contained-primary handoff),
#   - decompresses the Magisk stock-init backup to /init.stock (fallback),
#   - compiles scripts/native-init/oss-init.c statically for aarch64 as /init
#     (the real PID 1 / host supervisor),
#   - adds overlay.d/oss-contain.rc (one kernel-log liveness marker; no mount).
# AVB hash footer is re-added with the project key; top-level vbmeta stays
# verification-disabled, so it boots on the unlocked device like the base.
#
#   scripts/native-init/build-stage2-image.sh OUT_DIR
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:?output directory required}"
RESEARCH="${A26_RESEARCH:-$HOME/Workspace/research/linux-galaxy-a26}"
BASE="${A26_BASE_IMG:-$HOME/oss-kernel/out/boot/boot-oss-mirror-A266MUBU1AYB8.img}"
BASE_SHA256=8c0461eef2ca07b3d35199149907b40ff5db07fc10b4b92567ef1e3443b15f73
MAGISKBOOT="${MAGISKBOOT:-$RESEARCH/tools/magiskboot-x86_64}"
CC="${A26_CC:-aarch64-linux-gnu-gcc}"
KEY="$RESEARCH/keys/recovery-avb-rsa4096.pem"
SALT=9413599eee2c3c4ad1a16b47eb3dc5036d93c908c656b18881680b979e1e547c
PARTITION_SIZE=$((16384 * 4096))
SOURCE="$HERE/oss-init.c"
RELAY_RC="$HERE/overlay.d/oss-contain.rc"
NAME=boot-oss-stage2-A266MUBU1AYB8.img
# Moon overlay files that must survive unchanged from the base image.
MOON_FILES=(overlay.d/moon-direct-boot.rc overlay.d/sbin/moon-boot-mode.sh \
    overlay.d/sbin/moon-session.sh overlay.d/sbin/moon-fallback.sh)

for tool in avbtool sha256sum xz "$CC"; do
    command -v "$tool" >/dev/null || { echo "Missing tool: $tool" >&2; exit 1; }
done
[[ -f "$BASE" ]] || { echo "Missing base image: $BASE" >&2; exit 1; }
[[ -f "$SOURCE" ]] || { echo "Missing source: $SOURCE" >&2; exit 1; }
[[ -f "$RELAY_RC" ]] || { echo "Missing overlay: $RELAY_RC" >&2; exit 1; }
printf '%s  %s\n' "$BASE_SHA256" "$BASE" | sha256sum -c - >/dev/null

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/unpack" "$WORK/final" "$OUT"

"$CC" -static -no-pie -Os -s -Wall -Wextra -Werror "$SOURCE" -o "$WORK/oss-init"
[[ "$(od -An -tx1 -N4 "$WORK/oss-init" | tr -d ' ')" == 7f454c46 ]] || { echo "not ELF" >&2; exit 1; }

cp "$BASE" "$WORK/unpack/base.img"
(
    cd "$WORK/unpack"
    "$MAGISKBOOT" unpack base.img
    "$MAGISKBOOT" cpio ramdisk.cpio "extract init $WORK/init.magisk"
    "$MAGISKBOOT" cpio ramdisk.cpio "extract .backup/init.xz $WORK/init.stock.xz"
    xz -dc "$WORK/init.stock.xz" > "$WORK/init.stock"
    "$MAGISKBOOT" cpio ramdisk.cpio \
        "mv init init.magisk" \
        "add 0750 init.stock $WORK/init.stock" \
        "add 0750 init $WORK/oss-init" \
        "add 0644 overlay.d/oss-contain.rc $RELAY_RC"
    "$MAGISKBOOT" repack base.img new.img
)

cp "$WORK/unpack/new.img" "$WORK/payload.img"
avbtool erase_footer --image "$WORK/payload.img"
[[ "$(tail -c 16 "$WORK/payload.img")" == SEANDROIDENFORCE ]]
cp "$WORK/payload.img" "$OUT/$NAME"
avbtool add_hash_footer --image "$OUT/$NAME" --partition_name boot \
    --partition_size "$PARTITION_SIZE" --algorithm SHA256_RSA4096 --key "$KEY" --salt "$SALT"

# Verify: kernel byte-identical to the OSS base; /init is our supervisor;
# /init.magisk is the base /init byte for byte; Moon overlay files unchanged;
# /init.stock is the decompressed backup; the relay rc is present byte-exact.
(
    cd "$WORK/final"
    cp "$OUT/$NAME" final.img
    "$MAGISKBOOT" unpack final.img >/dev/null 2>&1
    mkdir base && (cd base && "$MAGISKBOOT" unpack "$WORK/unpack/base.img" >/dev/null 2>&1)
    cmp kernel base/kernel
    "$MAGISKBOOT" cpio ramdisk.cpio "extract init $WORK/c-init" >/dev/null 2>&1
    cmp "$WORK/c-init" "$WORK/oss-init"
    "$MAGISKBOOT" cpio ramdisk.cpio "extract init.magisk $WORK/c-magisk" >/dev/null 2>&1
    cmp "$WORK/c-magisk" "$WORK/init.magisk"
    "$MAGISKBOOT" cpio ramdisk.cpio "extract init.stock $WORK/c-stock" >/dev/null 2>&1
    cmp "$WORK/c-stock" "$WORK/init.stock"
    "$MAGISKBOOT" cpio ramdisk.cpio "extract overlay.d/oss-contain.rc $WORK/c-relay" >/dev/null 2>&1
    cmp "$WORK/c-relay" "$RELAY_RC"
    for f in "${MOON_FILES[@]}"; do
        "$MAGISKBOOT" cpio ramdisk.cpio "extract $f $WORK/c-ov" >/dev/null 2>&1
        "$MAGISKBOOT" cpio base/ramdisk.cpio "extract $f $WORK/c-ov-base" >/dev/null 2>&1
        cmp "$WORK/c-ov" "$WORK/c-ov-base"
    done
)
[[ "$(stat -c %s "$OUT/$NAME")" -eq "$PARTITION_SIZE" ]]
avbtool info_image --image "$OUT/$NAME" >"$OUT/boot-avb-info.txt"
(cd "$OUT" && sha256sum "$NAME" >SHA256SUMS)

cat >"$OUT/README.txt" <<README
OSS-Phone-V1 stage-2 containment BOOT for SM-A266M firmware A266MUBU1AYB8.

Base: boot-oss-mirror-A266MUBU1AYB8.img (sha256 $BASE_SHA256), the OSS Phone
kernel (PID_NS, DEVTMPFS, no DEFEX) on today's direct-boot ramdisk. The ramdisk
/init (magiskinit) was renamed to /init.magisk; the Magisk stock-init backup was
decompressed to /init.stock; scripts/native-init/oss-init.c was compiled
statically and installed as /init (the real PID 1, which runs Android init in a
PID + mount namespace); and overlay.d/oss-contain.rc was added. Kernel,
magiskinit, Moon overlay files and every other ramdisk byte are verified
unchanged. AVB hash footer re-added with the project key and base salt;
top-level vbmeta stays verification-disabled.

REQUIRES the OSS kernel (PID_NS). oss-init falls back to an uncontained boot if
containment cannot start, so it is still safe on the OSS kernel; it must NOT be
expected to contain on the stock kernel (no PID_NS -> it falls back to today's
boot).

Supervisor size: $(stat -c %s "$WORK/oss-init") bytes
README
cat "$OUT/SHA256SUMS"
