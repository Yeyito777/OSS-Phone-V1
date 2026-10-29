#!/usr/bin/env bash
# Build a recovery-slot image (unflashed) from today's working BOOT.
#
#   scripts/native-init/build-slot-image.sh OUT_DIR containment|plain
#
#   containment  /init = oss-init v4: Android's init in a PID+mount namespace
#                -> recovery-slot-stage2-v4-A266MUBU1AYB8.img
#   plain        /init = oss-pid1: loads the clear, puts magiskinit back at
#                /init and execs it (today's boot, from the slot)
#                -> recovery-slot-moon-A266MUBU1AYB8.img
# Both load oss-rebootmode-clear.ko first (rebootmode-clear.h).
#
# Starts from today's working BOOT (OSS kernel + Magisk + Moon direct-boot,
# 8c0461ee), changes only its ramdisk, and wraps the result into a recovery-slot
# image with build-recovery-slot.sh. There is deliberately no BOOT output: an
# image whose /init is oss-init may only be tried once from the recovery slot
# (flash-boot-inplace.sh stops on a BOOT whose /init differs unless given
# --allow-init-change).
#
# Ramdisk changes (everything else byte-identical):
#   /init            oss-init v4 or oss-pid1 (static aarch64)
#   /init.magisk     the former /init (magiskinit): the container's /init
#                    (containment) or renamed back to /init (plain)
#   /init.stock      the stock init, decompressed from Magisk's .backup/init.xz,
#                    oss-init's only fallback (exec'd through a held fd)
#   overlay.d/oss-contain.rc   liveness marker on /dev/kmsg (no mounts)
# build-recovery-slot.sh then adds overlay.d/sbin/oss-recovery-slot.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT="${1:?output dir}"
VARIANT="${2:?variant: containment or plain}"
R="${A26_RESEARCH:-$HOME/Workspace/research/linux-galaxy-a26}"
BASE="${A26_BASE_IMG:-$HOME/oss-kernel/out/boot/boot-oss-mirror-A266MUBU1AYB8.img}"
BASE_SHA256=8c0461eef2ca07b3d35199149907b40ff5db07fc10b4b92567ef1e3443b15f73
MAGISKBOOT="${MAGISKBOOT:-$R/tools/magiskboot-x86_64}"
CC="${A26_CC:-aarch64-linux-gnu-gcc}"
# Patched Mali GPU module for the containment image only. The stock mali_kbase
# uses find_get_pid(current->tgid), which resolves the global tgid in the
# CALLER's PID namespace and returns ESRCH inside the container, so every GPU
# context fails and SurfaceFlinger crash-loops (Stage-3 defect 1). The patched
# module uses get_task_pid(current, PIDTYPE_TGID). Drop-in: same deps, so no
# modules.dep/modules.load change; it overrides the vendor copy because the
# payload ramdisk is concatenated last (same last-wins path that makes oss-init
# the /init, proven on device). Pinned and verified; plain Moon does not need it
# (it runs Android as the real PID 1, outside any namespace).
MALI_KO="${A26_MALI_KO:-$HOME/oss-kernel/out/modules/mali-kbase-pid-namespace/mali_kbase.ko}"
MALI_KO_SHA=f36fd33955bca28479ed93e5c81613229b0ea2594e8df38a22eb74cbd4a614bc
MALI_PATH=lib/modules/mali_kbase.ko
case "$VARIANT" in
  containment) SRC=oss-init.c; NAME=recovery-slot-stage2-v7-A266MUBU1AYB8.img ;;
  plain)       SRC=oss-pid1.c; NAME=recovery-slot-moon-A266MUBU1AYB8.img ;;
  *) echo "variant must be containment or plain" >&2; exit 2 ;;
esac

for t in "$CC" xz sha256sum; do command -v "$t" >/dev/null || { echo "missing tool: $t" >&2; exit 1; }; done
printf '%s  %s\n' "$BASE_SHA256" "$BASE" | sha256sum -c - >/dev/null

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT
mkdir -p "$OUT" "$WORK/u"
"$CC" -static -no-pie -Os -s -Wall -Wextra -Werror "$HERE/$SRC" -o "$WORK/oss-init"
cp "$BASE" "$WORK/u/base.img"
if [ "$VARIANT" = containment ]; then
  printf '%s  %s\n' "$MALI_KO_SHA" "$MALI_KO" | sha256sum -c - >/dev/null
fi
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
  if [ "$VARIANT" = containment ]; then
    # Swap in the patched Mali module: it overrides the vendor copy (same path,
    # payload ramdisk concatenated last).
    "$MAGISKBOOT" cpio ramdisk.cpio "add 0644 $MALI_PATH $MALI_KO" >/dev/null 2>&1
  fi
  for f in init init.magisk init.stock overlay.d/oss-contain.rc .backup/init.xz overlay.d/moon-direct-boot.rc; do
    "$MAGISKBOOT" cpio ramdisk.cpio "exists $f" >/dev/null 2>&1 || { echo "payload missing $f" >&2; exit 1; }
  done
  "$MAGISKBOOT" cpio ramdisk.cpio "extract init $WORK/chk-init" "extract init.magisk $WORK/chk-magisk" \
    "extract init.stock $WORK/chk-stock" >/dev/null 2>&1
  cmp "$WORK/chk-init" "$WORK/oss-init"
  cmp "$WORK/chk-magisk" "$WORK/magiskinit"
  cmp "$WORK/chk-stock" "$WORK/init.stock"
  if [ "$VARIANT" = containment ]; then
    "$MAGISKBOOT" cpio ramdisk.cpio "exists $MALI_PATH" >/dev/null 2>&1 || { echo "payload missing $MALI_PATH" >&2; exit 1; }
    "$MAGISKBOOT" cpio ramdisk.cpio "extract $MALI_PATH $WORK/chk-mali" >/dev/null 2>&1
    cmp "$WORK/chk-mali" "$MALI_KO" || { echo "patched mali not added byte-exact" >&2; exit 1; }
  fi
  "$MAGISKBOOT" repack base.img payload.img >/dev/null 2>&1
)
file "$WORK/init.stock" | grep -q 'statically linked' || { echo "stock init is not static" >&2; exit 1; }
# Into the recovery slot (adds the oss-recovery-slot marker, verifies, AVB).
"$HERE/canary/build-recovery-slot.sh" "$OUT" "$WORK/u/payload.img" "$NAME" | tail -4
if [ "$VARIANT" = containment ]; then
  # The same binary is the userspace counter tool (`oss-init --bootcount ...`),
  # installed at /data/adb/moon/oss-init for moon-boot-confirm.sh.
  cp "$WORK/oss-init" "$OUT/oss-init-v7"
  ( cd "$OUT" && sha256sum oss-init-v7 >>SHA256SUMS )
fi
echo "$VARIANT /init ($SRC): $(stat -c %s "$WORK/oss-init") bytes, sha256 $(sha256sum <"$WORK/oss-init" | cut -c1-16)…"
