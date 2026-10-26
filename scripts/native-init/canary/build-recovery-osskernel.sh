#!/usr/bin/env bash
# Recovery-stage-2 FOUNDATION (calibration), UNFLASHED: the OSS kernel (PID_NS)
# packaged into the stock recovery image. Stock recovery ramdisk/dtb/dtbo are
# unchanged; only the kernel is swapped. Proves the kernel swap + recovery
# packaging + AVB. Booting it (via the canary one-shot) shows the OSS kernel
# running the stock recovery UI from the recovery slot. The Magisk-RECOVERYMODE
# + moon overlay + oss-init containment layer is the documented next build
# (RECOVERY-STAGE2-DESIGN.md), to be assembled with on-device iteration once the
# canary proves the slot is safe.
set -euo pipefail
R="${A26_RESEARCH:-$HOME/Workspace/research/linux-galaxy-a26}"
STOCK="$R/images/stock-A266MUBU1AYB8/recovery.img"
OSS_IMAGE="$HOME/oss-kernel/out/oss-mirror/Image"
KEY="$R/keys/recovery-avb-rsa4096.pem"
SALT=9413599eee2c3c4ad1a16b47eb3dc5036d93c908c656b18881680b979e1e547c
PARTITION_SIZE=$((24576 * 4096))
OUT="${1:?out dir}"; WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT" "$WORK/un" "$WORK/v"
unpack_bootimg --boot_img "$STOCK" --out "$WORK/un" --format=mkbootimg -0 > "$WORK/args.bin"
A=(); while IFS= read -r -d '' a; do A+=("$a"); done < "$WORK/args.bin"
for ((i=0;i<${#A[@]};i++)); do [[ "${A[$i]}" == --kernel ]] && A[$((i+1))]="$OSS_IMAGE"; done
mkbootimg "${A[@]}" --output "$WORK/core.img"
IMG="$OUT/recovery-osskernel-A266MUBU1AYB8.img"
cp "$WORK/core.img" "$IMG"; printf 'SEANDROIDENFORCE' >> "$IMG"
avbtool add_hash_footer --image "$IMG" --partition_name recovery \
  --partition_size "$PARTITION_SIZE" --algorithm SHA256_RSA4096 --key "$KEY" --salt "$SALT"
[[ "$(stat -c %s "$IMG")" -eq "$PARTITION_SIZE" ]]
unpack_bootimg --boot_img "$IMG" --out "$WORK/v" >/dev/null 2>&1
cmp "$WORK/v/kernel" "$OSS_IMAGE" && echo "kernel = OSS Image (verified)"
cmp "$WORK/v/ramdisk" "$WORK/un/ramdisk" && echo "ramdisk = stock recovery (unchanged)"
cmp "$WORK/v/dtb" "$WORK/un/dtb" && echo "dtb = stock recovery (unchanged)"
( cd "$OUT" && sha256sum recovery-osskernel-A266MUBU1AYB8.img > SHA256SUMS ); cat "$OUT/SHA256SUMS"
