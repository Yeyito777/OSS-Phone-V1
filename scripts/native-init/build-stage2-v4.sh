#!/usr/bin/env bash
# Build the stage-2 v4 containment image for the RECOVERY slot, UNFLASHED.
#
#   scripts/native-init/build-stage2-v4.sh OUT_DIR
#
# Starts from today's working BOOT (OSS kernel + Magisk + Moon direct-boot,
# 8c0461ee), changes only its ramdisk, and wraps the result into a recovery-slot
# image with build-recovery-slot.sh. There is deliberately no BOOT output: an
# image whose /init is oss-init may only be tried once from the recovery slot
# (flash-boot-inplace.sh refuses any BOOT whose /init differs anyway).
#
# Ramdisk changes (everything else byte-identical):
#   /init            oss-init v4 (static aarch64, scripts/native-init/oss-init.c)
#   /init.magisk     the former /init (magiskinit), becomes the container's /init
#   /init.stock      the stock init, decompressed from Magisk's .backup/init.xz,
#                    oss-init's only fallback (exec'd through a held fd)
#   overlay.d/oss-contain.rc   liveness marker on /dev/kmsg (no mounts)
# build-recovery-slot.sh then adds overlay.d/sbin/oss-recovery-slot.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:?output dir}"
R="${A26_RESEARCH:-$HOME/Workspace/research/linux-galaxy-a26}"
BASE="${A26_BASE_IMG:-$HOME/oss-kernel/out/boot/boot-oss-mirror-A266MUBU1AYB8.img}"
BASE_SHA256=8c0461eef2ca07b3d35199149907b40ff5db07fc10b4b92567ef1e3443b15f73
MAGISKBOOT="${MAGISKBOOT:-$R/tools/magiskboot-x86_64}"
CC="${A26_CC:-aarch64-linux-gnu-gcc}"
NAME=recovery-slot-stage2-v4-A266MUBU1AYB8.img

for t in "$CC" xz sha256sum; do command -v "$t" >/dev/null || { echo "missing tool: $t" >&2; exit 1; }; done
printf '%s  %s\n' "$BASE_SHA256" "$BASE" | sha256sum -c - >/dev/null

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT" "$WORK/u"
"$CC" -static -no-pie -Os -s -Wall -Wextra -Werror "$HERE/oss-init.c" -o "$WORK/oss-init"
cp "$BASE" "$WORK/u/base.img"
(
  cd "$WORK/u"
  "$MAGISKBOOT" unpack base.img >/dev/null 2>&1
  "$MAGISKBOOT" cpio ramdisk.cpio "extract init $WORK/magiskinit" >/dev/null 2>&1
  "$MAGISKBOOT" cpio ramdisk.cpio "extract .backup/init.xz $WORK/init.xz" >/dev/null 2>&1
  xz -dc "$WORK/init.xz" >"$WORK/init.stock"
  "$MAGISKBOOT" cpio ramdisk.cpio \
    "mv init init.magisk" \
    "add 0750 init.stock $WORK/init.stock" \
    "add 0750 init $WORK/oss-init" \
    "add 0644 overlay.d/oss-contain.rc $HERE/overlay.d/oss-contain.rc" >/dev/null 2>&1
  for f in init init.magisk init.stock overlay.d/oss-contain.rc .backup/init.xz overlay.d/moon-direct-boot.rc; do
    "$MAGISKBOOT" cpio ramdisk.cpio "exists $f" >/dev/null 2>&1 || { echo "payload missing $f" >&2; exit 1; }
  done
  "$MAGISKBOOT" cpio ramdisk.cpio "extract init $WORK/chk-init" "extract init.magisk $WORK/chk-magisk" \
    "extract init.stock $WORK/chk-stock" >/dev/null 2>&1
  cmp "$WORK/chk-init" "$WORK/oss-init"
  cmp "$WORK/chk-magisk" "$WORK/magiskinit"
  cmp "$WORK/chk-stock" "$WORK/init.stock"
  "$MAGISKBOOT" repack base.img payload.img >/dev/null 2>&1
)
file "$WORK/init.stock" | grep -q 'statically linked' || { echo "stock init is not static" >&2; exit 1; }
# Into the recovery slot (adds the oss-recovery-slot marker, verifies, AVB).
"$HERE/canary/build-recovery-slot.sh" "$OUT" "$WORK/u/payload.img" "$NAME" | tail -4
echo "oss-init v4: $(stat -c %s "$WORK/oss-init") bytes, sha256 $(sha256sum <"$WORK/oss-init" | cut -c1-16)…"
