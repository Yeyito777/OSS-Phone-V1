#!/usr/bin/env bash
# Put a built kernel into the current BOOT image, changing nothing else.
#
#   scripts/kernel/make-boot-image.sh BUILD_OUT OUT_DIR [BASE_BOOT_IMG]
#
# BASE_BOOT_IMG defaults to the direct-boot image flashed on the phone
# (Magisk v30.7 + scripts/direct-boot overlay). The ramdisk, header and
# everything else are kept; only the kernel is swapped. Magisk's Samsung
# kernel patches are re-applied exactly as the base had them (DEFEX execve
# check and PROCA neutralised, RKP pattern absent), so root keeps working. The
# AVB hash footer is re-added with the project key and salt, as in
# scripts/direct-boot/build-boot-image.sh (top-level vbmeta is
# verification-disabled). Output is never flashed by this script.
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

build="${1:?build output directory required}"
out="${2:?output directory required}"
base="${3:-$RESEARCH/images/moon-direct-boot-v1/boot-moon-direct-A266MUBU1AYB8.img}"
mkdir -p "$out"
build="$(cd "$build" && pwd)" out="$(cd "$out" && pwd)" base="$(cd "$(dirname "$base")" && pwd)/$(basename "$base")"
BASE_SHA256_DEFAULT=3bf291627791e71ba00b4fcdde8b6bd92faa62d6fe2f14da9b906fd86619e9f6
KEY="$RESEARCH/keys/recovery-avb-rsa4096.pem"
SALT=9413599eee2c3c4ad1a16b47eb3dc5036d93c908c656b18881680b979e1e547c
PARTITION_SIZE=$((16384 * 4096))
DEFEX_FROM=821B8012 DEFEX_TO=E2FF8F12
PROCA_FROM=70726F63615F636F6E66696700 PROCA_TO=70726F63615F6D616769736B00

for tool in avbtool sha256sum python3; do
    command -v "$tool" >/dev/null || { echo "Missing tool: $tool" >&2; exit 1; }
done
[[ -f "$build/Image" ]] || { echo "No Image in $build" >&2; exit 1; }
base_sha="$(sha256sum "$base" | cut -d' ' -f1)"
if [[ "$base" == *boot-moon-direct-A266MUBU1AYB8.img && "$base_sha" != "$BASE_SHA256_DEFAULT" ]]; then
    echo "Base image hash changed: $base_sha" >&2
    exit 1
fi

count() { python3 -c 'import sys; print(open(sys.argv[1],"rb").read().count(bytes.fromhex(sys.argv[2])))' "$1" "$2"; }

name="boot-$(basename "$build")-$FIRMWARE.img"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/unpack" "$work/final" "$work/base" "$out"
cp "$base" "$work/unpack/base.img"
cp "$build/Image" "$work/Image.patched"
# Same patch set as the base: DEFEX once, PROCA twice in the stock kernel.
# A kernel built without DEFEX (oss-phone.fragment) has nothing to patch there.
defex_want=1
if [[ -f "$build/.config" ]] && grep -qx '# CONFIG_SECURITY_DEFEX is not set' "$build/.config"; then
    defex_want=0
fi
[[ "$(count "$work/Image.patched" $DEFEX_FROM)" -eq $defex_want ]] || { echo "DEFEX pattern count unexpected" >&2; exit 1; }
[[ "$(count "$work/Image.patched" $PROCA_FROM)" -eq 2 ]] || { echo "PROCA pattern count differs from stock" >&2; exit 1; }
(
    cd "$work"
    if [[ $defex_want -eq 1 ]]; then "$MAGISKBOOT" hexpatch Image.patched $DEFEX_FROM $DEFEX_TO; fi
    "$MAGISKBOOT" hexpatch Image.patched $PROCA_FROM $PROCA_TO
) >/dev/null 2>&1
[[ "$(count "$work/Image.patched" $DEFEX_FROM)$(count "$work/Image.patched" $PROCA_FROM)" == 00 ]]
(
    cd "$work/unpack"
    "$MAGISKBOOT" unpack base.img >/dev/null 2>&1
    cp "$work/Image.patched" kernel
    "$MAGISKBOOT" repack base.img new.img >/dev/null 2>&1
)
cp "$work/unpack/new.img" "$work/payload.img"
avbtool erase_footer --image "$work/payload.img"
[[ "$(tail -c 16 "$work/payload.img")" == SEANDROIDENFORCE ]]
cp "$work/payload.img" "$out/$name"
avbtool add_hash_footer --image "$out/$name" --partition_name boot \
    --partition_size "$PARTITION_SIZE" --algorithm SHA256_RSA4096 --key "$KEY" --salt "$SALT"

# Verify: kernel is the patched build, ramdisk and header fields untouched.
(
    cd "$work/final"
    cp "$out/$name" final.img
    "$MAGISKBOOT" unpack -h final.img >/dev/null 2>&1
    (cd "$work/base" && "$MAGISKBOOT" unpack -h "$work/unpack/base.img" >/dev/null 2>&1)
    cmp kernel "$work/Image.patched"
    cmp ramdisk.cpio "$work/base/ramdisk.cpio"
    diff <(grep -v '^kernel' header) <(grep -v '^kernel' "$work/base/header")
)
[[ "$(stat -c %s "$out/$name")" -eq "$PARTITION_SIZE" ]]
avbtool info_image --image "$out/$name" >"$out/${name%.img}.avb-info.txt"
cp "$build/SHA256SUMS" "$out/${name%.img}.kernel-SHA256SUMS"
cat >"$out/${name%.img}.README.txt" <<README
UNFLASHED test BOOT image for SM-A266M firmware $FIRMWARE.

Kernel:  $(basename "$build") (sha256 $(sha256sum "$build/Image" | cut -d' ' -f1) before
         Magisk's Samsung hexpatches (PROCA$( [[ $defex_want -eq 1 ]] && echo ", DEFEX")); $(strings "$build/Image" | grep -m1 '^Linux version ' | cut -c1-80)...)
Base:    $(basename "$base") (sha256 $base_sha)
Ramdisk, header and Magisk payload are byte-identical to the base; only the
kernel differs. AVB hash footer: project key, same salt as the base.
README
(cd "$out" && sha256sum ./*.img >SHA256SUMS && grep -F "$name" SHA256SUMS)
