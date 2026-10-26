#!/usr/bin/env bash
# Build an out-of-tree module against a kernel built by build-kernel.sh, with
# the same toolchain, paths and sandbox, then check it like the vendor
# modules (vermagic, every import resolved with the kernel's CRC).
#
#   scripts/kernel/build-module.sh MODULE_SRC_DIR [KERNEL_BUILD]
#
# KERNEL_BUILD defaults to oss-samsung (the installed OSS kernel, BOOT
# 8c0461ee). Output: $KROOT/out/modules/<name>/<name>.ko (debug-stripped, as
# the vendor modules ship), kmi-report.txt, SHA256SUMS.
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

msrc="$(cd "${1:?module source directory required}" && pwd)"
kname="${2:-oss-samsung}"
ktree="$KROOT/build/$kname/src"
kout="$KROOT/out/$kname"
[[ -f "$ktree/Module.symvers" && -f "$kout/Module.symvers" ]] \
    || { echo "No built kernel $kname (run build-kernel.sh)" >&2; exit 1; }
cmp -s "$ktree/Module.symvers" "$kout/Module.symvers"
name="$(basename "$msrc")"
work="$KROOT/modbuild/$name"
out="$KROOT/out/modules/$name"
rm -rf "$work" "$out"
mkdir -p "$work" "$out"
cp -a "$msrc/." "$work/"
MOD_PATH=/home/dpi/qb5_8814/workspace/P4_1716/android/oss-modules/$name

make_args=(
    ARCH=arm64 LLVM=1 LLVM_IAS=1
    CROSS_COMPILE=aarch64-linux-gnu- CROSS_COMPILE_COMPAT=arm-linux-gnueabi-
    PLATFORM_VERSION=12 ANDROID_MAJOR_VERSION=s TARGET_SOC=s5e8825
    TARGET_BUILD_VARIANT=user
)
bwrap --ro-bind /usr /usr --ro-bind /etc /etc \
    --symlink usr/lib /lib --symlink usr/lib /lib64 --symlink usr/bin /bin --symlink usr/bin /sbin \
    --proc /proc --dev /dev --tmpfs /tmp \
    --ro-bind "$CLANG" "$SAMSUNG_CLANG_PATH" \
    --ro-bind "$ktree" "$SAMSUNG_SRC_PATH" \
    --bind "$work" "$MOD_PATH" --chdir "$SAMSUNG_SRC_PATH" \
    --unshare-net --clearenv \
    --setenv PATH "$SAMSUNG_CLANG_PATH/bin:/usr/bin" --setenv HOME /tmp --setenv LC_ALL C --setenv TZ UTC \
    make "${make_args[@]}" M="$MOD_PATH" modules >"$out/build.log" 2>&1 \
    || { tail -30 "$out/build.log" >&2; exit 1; }

ko="$(ls "$work"/*.ko)"
cp "$ko" "$out/"
"$CLANG/bin/llvm-strip" --strip-debug "$out/$(basename "$ko")"
python3 -I "$KERNEL_SCRIPTS/check-kmi.py" "$kout" "$out" | tee "$out/kmi-report.txt"
(cd "$out" && sha256sum ./*.ko >SHA256SUMS && cat SHA256SUMS)
