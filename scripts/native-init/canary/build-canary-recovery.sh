#!/usr/bin/env bash
# Build the panic-canary RECOVERY image, UNFLASHED. Stock recovery kernel, dtb
# and recovery_dtbo; a tiny ramdisk whose /init is the canary (writes one kmsg
# line, exits -> PID-1 panic). Reuses the v3/v4a-proven recovery packaging:
# mkbootimg from the stock recovery's own args, Samsung SEANDROIDENFORCE marker,
# AVB footer with the project key at the exact PIT partition size. Top vbmeta
# stays verification-disabled on this phone, so it loads like v3 did.
set -euo pipefail
# Deterministic: ramdisk uses gzip -n and avbtool a fixed --salt, so the
# image hash is reproducible (authoritative sha in images/.../SHA256SUMS).
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
R="${A26_RESEARCH:-$HOME/Workspace/research/linux-galaxy-a26}"
STOCK="$R/images/stock-A266MUBU1AYB8/recovery.img"
KEY="$R/keys/recovery-avb-rsa4096.pem"
OUT="${1:?output dir}"; WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
PARTITION_SIZE=$((24576 * 4096))   # 100663296, the RECOVERY PIT size
SALT=9413599eee2c3c4ad1a16b47eb3dc5036d93c908c656b18881680b979e1e547c
mkdir -p "$OUT" "$WORK/rd"
aarch64-linux-gnu-gcc -static -O2 -s -Wall "$HERE/canary-init.c" -o "$WORK/rd/init"
( cd "$WORK/rd" && mkdir -p dev proc sys && chmod 0755 init
  find . | sort | cpio -o -H newc --reproducible 2>/dev/null > "$WORK/ramdisk.cpio" )
gzip -n -9 -c "$WORK/ramdisk.cpio" > "$WORK/ramdisk.gz"   # -n = deterministic; kernel RD_GZIP=y
unpack_bootimg --boot_img "$STOCK" --out "$WORK/un" --format=mkbootimg -0 > "$WORK/args.bin"
A=(); while IFS= read -r -d '' a; do A+=("$a"); done < "$WORK/args.bin"
for ((i=0;i<${#A[@]};i++)); do [[ "${A[$i]}" == --ramdisk ]] && A[$((i+1))]="$WORK/ramdisk.gz"; done
mkbootimg "${A[@]}" --output "$WORK/core.img"
IMG="$OUT/recovery-canary-A266MUBU1AYB8.img"
cp "$WORK/core.img" "$IMG"; printf 'SEANDROIDENFORCE' >> "$IMG"
avbtool add_hash_footer --image "$IMG" --partition_name recovery \
    --partition_size "$PARTITION_SIZE" --algorithm SHA256_RSA4096 --key "$KEY" --salt "$SALT"
[[ "$(stat -c %s "$IMG")" -eq "$PARTITION_SIZE" ]]
# verify kernel/dtb/dtbo are byte-identical to stock recovery
mkdir -p "$WORK/v"; unpack_bootimg --boot_img "$IMG" --out "$WORK/v" >/dev/null 2>&1
cmp "$WORK/v/kernel" "$WORK/un/kernel"
cmp "$WORK/v/dtb" "$WORK/un/dtb" 2>/dev/null || cmp "$WORK/v/recovery_dtbo" "$WORK/un/recovery_dtbo"
avbtool info_image --image "$IMG" > "$OUT/recovery-canary-avb-info.txt"
( cd "$OUT" && sha256sum recovery-canary-A266MUBU1AYB8.img > SHA256SUMS )
aarch64-linux-gnu-gcc -static -O2 -s -Wall "$HERE/reboot-recovery.c" -o "$OUT/reboot-recovery"
( cd "$OUT" && sha256sum reboot-recovery >> SHA256SUMS )
echo "built:"; cat "$OUT/SHA256SUMS"; ls -l "$IMG" "$OUT/reboot-recovery"
