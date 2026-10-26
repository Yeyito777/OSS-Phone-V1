#!/usr/bin/env bash
# Pack a NORMAL boot into the RECOVERY partition (a second, one-shot boot slot).
#
#   scripts/native-init/canary/build-recovery-slot.sh OUT_DIR [BOOT_IMG] [NAME]
#
# The phone's normal boot is BOOT (header v4: kernel + generic ramdisk) plus
# vendor_boot (vendor ramdisk fragments, dtb, cmdline, bootconfig). RECOVERY is
# a standalone header v2 image. This builds a v2 image that carries exactly what
# a v4 bootloader assembles for a normal boot:
#   kernel        = BOOT's kernel (byte-exact, incl. Magisk's kernel hexpatches)
#   ramdisk       = vendor_ramdisk00 + vendor_ramdisk01 + BOOT's ramdisk, the v4
#                   load order (vendor fragments first, generic ramdisk last)
#   dtb           = stock recovery dtb (verified == vendor_boot dtb)
#   recovery_dtbo = stock recovery_dtbo (verified == dtbo partition content)
#   cmdline       = stock recovery cmdline "bootconfig buildtime_bootconfig=enable
#                   loop.max_part=7" (covers vendor_boot's cmdline and its whole
#                   bootconfig section, buildtime_bootconfig=enable)
# The ramdisk has no /system/bin/recovery or /sbin/recovery, so AOSP init
# (IsRecoveryMode is file-based) and magiskinit both take the normal 2SI path.
# BOOT itself is never touched: this goes to RECOVERY and is entered once via
# the BCB-free reboot-recovery tool.
#
# Every image this script builds carries overlay.d/sbin/oss-recovery-slot
# (magiskinit copies it to /debug_ramdisk). moon-direct-policy.sh lets a
# "recovery" boot reason continue to Moon ONLY when that marker exists, so a
# recovery-slot test image can show Moon while normal recovery boots (stock
# recovery, which never runs the policy) and BOOT images (no marker) are
# unaffected. The marker is added with magiskboot, the tool and lz4 format
# Magisk itself uses for this ramdisk.
#
# Every image also carries /oss-rebootmode-clear.ko (sha256-pinned), and the
# script REFUSES a payload whose /init is not one of our recovery-slot pre-inits
# (oss-init or oss-pid1, recognised by the /init.magisk + /init.stock layout and
# /init differing from magiskinit). Those load the module as their first action;
# a slot image without that clear would loop in recovery after any failure
# (notes/stage2/RECOVERY-PERSISTENCE-ROOTCAUSE.md).
#
# Deterministic: inputs are copied verbatim, AVB uses a fixed salt.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
R="${A26_RESEARCH:-$HOME/Workspace/research/linux-galaxy-a26}"
OUT="${1:?output dir}"
BOOT="${2:-$HOME/oss-kernel/out/boot/boot-oss-mirror-A266MUBU1AYB8.img}"
NAME="${3:-recovery-slot-moon-A266MUBU1AYB8.img}"
VENDOR_BOOT="${A26_VENDOR_BOOT:-$HOME/oss-kernel/.recslot/in/vendor_boot.img}"
STOCK_RECOVERY="$R/images/stock-A266MUBU1AYB8/recovery.img"
KEY="$R/keys/recovery-avb-rsa4096.pem"
MAGISKBOOT="${MAGISKBOOT:-$R/tools/magiskboot-x86_64}"
KO="${A26_REBOOTMODE_CLEAR_KO:-$HOME/oss-kernel/out/modules/oss-rebootmode-clear/oss-rebootmode-clear.ko}"
# Pin of the verify-after-write module (masked clear, require_mode).
# The first module a18606a8 is withdrawn; set this when the kernel agent
# reports the new .ko. Builds refuse until then.
KO_SHA=AWAITING-NEW-OSS-REBOOTMODE-CLEAR-KO
SALT=9413599eee2c3c4ad1a16b47eb3dc5036d93c908c656b18881680b979e1e547c
PARTITION_SIZE=$((24576 * 4096))   # RECOVERY, PIT entry #22
VENDOR_BOOT_SHA=74723185dc3ae25be122418e8d401b91cfd27eac688395e97067f77a849692d2
STOCK_RECOVERY_SHA=9ab6cefe06b18ddd7d2410f8ce06233fb0c14a39687fe085b4e74ad18a91a7e8

for t in unpack_bootimg mkbootimg avbtool sha256sum; do
  command -v "$t" >/dev/null || { echo "missing tool: $t" >&2; exit 1; }; done
printf '%s  %s\n' "$VENDOR_BOOT_SHA" "$VENDOR_BOOT" | sha256sum -c - >/dev/null
printf '%s  %s\n' "$STOCK_RECOVERY_SHA" "$STOCK_RECOVERY" | sha256sum -c - >/dev/null
[[ -f "$BOOT" ]] || { echo "missing BOOT image: $BOOT" >&2; exit 1; }
printf '%s  %s\n' "$KO_SHA" "$KO" | sha256sum -c - >/dev/null

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"
# Add the recovery-slot marker to (a copy of) the BOOT ramdisk.
mkdir -p "$WORK/mb"; cp "$BOOT" "$WORK/mb/b.img"
printf 'OSS-Phone recovery-slot test image\n' >"$WORK/mb/marker"
( cd "$WORK/mb" && "$MAGISKBOOT" unpack b.img >/dev/null 2>&1 )
# Interlock: only payloads whose /init is a clear-first pre-init.
( cd "$WORK/mb"
  for f in init.magisk init.stock; do
    "$MAGISKBOOT" cpio ramdisk.cpio "exists $f" >/dev/null 2>&1 ||
      { echo "refusing: payload has no /$f, so its /init is not an oss pre-init" >&2; exit 1; }
  done
  "$MAGISKBOOT" cpio ramdisk.cpio "extract init $WORK/p-init" "extract init.magisk $WORK/p-magisk" >/dev/null 2>&1
  ! cmp -s "$WORK/p-init" "$WORK/p-magisk" || { echo "refusing: /init is magiskinit (no rebootmode clear)" >&2; exit 1; }
  grep -q oss-rebootmode-clear.ko "$WORK/p-init" ||
    { echo "refusing: /init does not reference oss-rebootmode-clear.ko" >&2; exit 1; } )
( cd "$WORK/mb" &&
  "$MAGISKBOOT" cpio ramdisk.cpio "add 0644 overlay.d/sbin/oss-recovery-slot marker" \
                                  "add 0644 oss-rebootmode-clear.ko $KO" >/dev/null 2>&1 &&
  "$MAGISKBOOT" repack b.img slot.img >/dev/null 2>&1 )
"$MAGISKBOOT" cpio "$WORK/mb/ramdisk.cpio" "extract oss-rebootmode-clear.ko $WORK/chk-ko" >/dev/null 2>&1
cmp "$WORK/chk-ko" "$KO" || { echo "module not added byte-exact" >&2; exit 1; }
"$MAGISKBOOT" cpio "$WORK/mb/ramdisk.cpio" "exists overlay.d/sbin/oss-recovery-slot" >/dev/null 2>&1 ||
  { echo "marker not added" >&2; exit 1; }
unpack_bootimg --boot_img "$WORK/mb/slot.img" --out "$WORK/boot" >/dev/null
unpack_bootimg --boot_img "$BOOT" --out "$WORK/boot-orig" >/dev/null
cmp "$WORK/boot/kernel" "$WORK/boot-orig/kernel" || { echo "magiskboot changed the kernel" >&2; exit 1; }
unpack_bootimg --boot_img "$VENDOR_BOOT" --out "$WORK/vb" >/dev/null
unpack_bootimg --boot_img "$STOCK_RECOVERY" --out "$WORK/rec" --format=mkbootimg -0 >"$WORK/args.bin"

# The ramdisk must not put the kernel into recovery mode.
for frag in "$WORK/vb/vendor_ramdisk00" "$WORK/vb/vendor_ramdisk01" "$WORK/boot/ramdisk"; do
  if (lz4 -dc "$frag" 2>/dev/null || gzip -dc "$frag" 2>/dev/null || cat "$frag") |
      cpio -t 2>/dev/null | grep -qE '^(system/bin/recovery|sbin/recovery)$'; then
    echo "refusing: $frag contains a recovery binary" >&2; exit 1
  fi
done
cmp "$WORK/vb/dtb" "$WORK/rec/dtb" || { echo "vendor_boot dtb != recovery dtb" >&2; exit 1; }

cat "$WORK/vb/vendor_ramdisk00" "$WORK/vb/vendor_ramdisk01" "$WORK/boot/ramdisk" >"$WORK/ramdisk"
A=(); while IFS= read -r -d '' a; do A+=("$a"); done <"$WORK/args.bin"
for ((i = 0; i < ${#A[@]}; i++)); do
  case "${A[$i]}" in
    --kernel) A[$((i + 1))]="$WORK/boot/kernel" ;;
    --ramdisk) A[$((i + 1))]="$WORK/ramdisk" ;;
  esac
done
mkbootimg "${A[@]}" --output "$WORK/core.img"
IMG="$OUT/$NAME"
cp "$WORK/core.img" "$IMG"; printf 'SEANDROIDENFORCE' >>"$IMG"
avbtool add_hash_footer --image "$IMG" --partition_name recovery \
  --partition_size "$PARTITION_SIZE" --algorithm SHA256_RSA4096 --key "$KEY" --salt "$SALT"
[[ "$(stat -c %s "$IMG")" -eq "$PARTITION_SIZE" ]]

# Verify every component against its source.
unpack_bootimg --boot_img "$IMG" --out "$WORK/v" >/dev/null
cmp "$WORK/v/kernel" "$WORK/boot/kernel"
cmp "$WORK/v/ramdisk" "$WORK/ramdisk"
cmp "$WORK/v/dtb" "$WORK/rec/dtb"
cmp "$WORK/v/recovery_dtbo" "$WORK/rec/recovery_dtbo"
unpack_bootimg --boot_img "$IMG" --out "$WORK/v2" --format=info >"$OUT/${NAME%.img}-info.txt"
grep -q 'header version: 2' "$OUT/${NAME%.img}-info.txt"
avbtool info_image --image "$IMG" >"$OUT/${NAME%.img}-avb-info.txt"
( cd "$OUT" && sha256sum "$NAME" >>SHA256SUMS )
echo "kernel        = $(sha256sum <"$WORK/boot/kernel" | cut -c1-16)… from $(basename "$BOOT")"
echo "ramdisk       = vendor00 $(stat -c %s "$WORK/vb/vendor_ramdisk00") + vendor01 $(stat -c %s "$WORK/vb/vendor_ramdisk01") + boot $(stat -c %s "$WORK/boot/ramdisk") bytes"
echo "dtb, dtbo, cmdline = stock recovery (dtb verified == vendor_boot dtb)"
sha256sum "$IMG"
