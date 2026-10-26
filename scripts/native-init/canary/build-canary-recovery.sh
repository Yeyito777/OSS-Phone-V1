#!/usr/bin/env bash
# Build the panic-canary RECOVERY image (unflashed) and the reboot-recovery tool.
#
#   scripts/native-init/canary/build-canary-recovery.sh OUT_DIR
#
# Stock recovery kernel, dtb, recovery_dtbo, cmdline and header, and the
# complete stock recovery ramdisk with exactly two changes:
#   /init                     the canary (canary-init.c) instead of the stock
#                             symlink to /system/bin/init
#   /oss-rebootmode-clear.ko  the kernel agent's module (sha256-pinned)
# The canary loads the module first (clearing the PMU recovery selection), then
# exits PID 1 to panic; if the module does not load it does NOT panic and execs
# the stock recovery init, so stock recovery comes up. The ramdisk is edited with
# magiskboot (ownership, modes and device nodes kept) and stored gzip -n
# (deterministic; the kernel has RD_GZIP=y); AVB footer with a fixed salt.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
R="${A26_RESEARCH:-$HOME/Workspace/research/linux-galaxy-a26}"
OUT="${1:?output dir}"
STOCK="$R/images/stock-A266MUBU1AYB8/recovery.img"
STOCK_SHA=9ab6cefe06b18ddd7d2410f8ce06233fb0c14a39687fe085b4e74ad18a91a7e8
KO="${A26_REBOOTMODE_CLEAR_KO:-$HOME/oss-kernel/out/modules/oss-rebootmode-clear/oss-rebootmode-clear.ko}"
# Pin of the verify-after-write module (masked clear, require_mode).
# The first module a18606a8 is withdrawn; set this when the kernel agent
# reported the new .ko: v2, branch rebootmode-clear 0a40464.
KO_SHA=7130cf3c04626745b1036262bf264053539906058948c95b7dbda344054e2568
MAGISKBOOT="${MAGISKBOOT:-$R/tools/magiskboot-x86_64}"
KEY="$R/keys/recovery-avb-rsa4096.pem"
SALT=9413599eee2c3c4ad1a16b47eb3dc5036d93c908c656b18881680b979e1e547c
PARTITION_SIZE=$((24576 * 4096))   # RECOVERY, PIT entry #22
NAME=recovery-canary-A266MUBU1AYB8.img

for t in aarch64-linux-gnu-gcc unpack_bootimg mkbootimg avbtool lz4 gzip sha256sum; do
  command -v "$t" >/dev/null || { echo "missing tool: $t" >&2; exit 1; }; done
printf '%s  %s\n' "$STOCK_SHA" "$STOCK" | sha256sum -c - >/dev/null
printf '%s  %s\n' "$KO_SHA" "$KO" | sha256sum -c - >/dev/null

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT"
aarch64-linux-gnu-gcc -static -O2 -s -Wall -Wextra -Werror "$HERE/canary-init.c" -o "$WORK/canary"
aarch64-linux-gnu-gcc -static -O2 -s -Wall "$HERE/reboot-recovery.c" -o "$OUT/reboot-recovery"

unpack_bootimg --boot_img "$STOCK" --out "$WORK/un" --format=mkbootimg -0 >"$WORK/args.bin"
lz4 -dc "$WORK/un/ramdisk" >"$WORK/ramdisk.cpio"
"$MAGISKBOOT" cpio "$WORK/ramdisk.cpio" "exists system/bin/init" >/dev/null 2>&1 ||
  { echo "stock recovery ramdisk has no system/bin/init (the canary's fallback)" >&2; exit 1; }
"$MAGISKBOOT" cpio "$WORK/ramdisk.cpio" \
  "rm init" \
  "add 0750 init $WORK/canary" \
  "add 0644 oss-rebootmode-clear.ko $KO" >/dev/null 2>&1
"$MAGISKBOOT" cpio "$WORK/ramdisk.cpio" "extract init $WORK/chk-init" "extract oss-rebootmode-clear.ko $WORK/chk-ko" >/dev/null 2>&1
cmp "$WORK/chk-init" "$WORK/canary"
cmp "$WORK/chk-ko" "$KO"
gzip -n -9 -c "$WORK/ramdisk.cpio" >"$WORK/ramdisk.gz"

A=(); while IFS= read -r -d '' a; do A+=("$a"); done <"$WORK/args.bin"
for ((i = 0; i < ${#A[@]}; i++)); do [[ "${A[$i]}" == --ramdisk ]] && A[$((i + 1))]="$WORK/ramdisk.gz"; done
mkbootimg "${A[@]}" --output "$WORK/core.img"
IMG="$OUT/$NAME"
cp "$WORK/core.img" "$IMG"; printf 'SEANDROIDENFORCE' >>"$IMG"
avbtool add_hash_footer --image "$IMG" --partition_name recovery \
  --partition_size "$PARTITION_SIZE" --algorithm SHA256_RSA4096 --key "$KEY" --salt "$SALT"
[[ "$(stat -c %s "$IMG")" -eq "$PARTITION_SIZE" ]]

# kernel, dtb and recovery_dtbo byte-identical to stock recovery
unpack_bootimg --boot_img "$IMG" --out "$WORK/v" >/dev/null
for part in kernel dtb recovery_dtbo; do cmp "$WORK/v/$part" "$WORK/un/$part"; done
avbtool info_image --image "$IMG" >"$OUT/recovery-canary-avb-info.txt"
( cd "$OUT" && sha256sum "$NAME" reboot-recovery >SHA256SUMS )
cat "$OUT/SHA256SUMS"
