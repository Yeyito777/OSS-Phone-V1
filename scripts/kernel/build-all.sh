#!/usr/bin/env bash
# End-to-end, on the build host (whale): toolchain, source, the stock
# reproduction and the OSS variants, every offline check, unflashed BOOT
# images. Reports land in $KROOT/out/<variant>/ and $KROOT/out/boot/.
#
#   scripts/kernel/build-all.sh [SAMSUNG_ZIP]
#
# Builds from Samsung's official SM-A266M_LA_15_Opensource.zip (argument, or
# $SAMSUNG_ZIP, default $KROOT/dl/), else from the pinned mirror, which
# produces bit-identical kernels. BASE_BOOT_IMG overrides the
# BOOT image the kernels are dropped into (default: today's direct-boot image).
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"
S="$KERNEL_SCRIPTS"

bash "$S/fetch-toolchain.sh"
zip="${1:-$SAMSUNG_ZIP}"
if [[ -f "$zip" ]]; then
    bash "$S/fetch-source.sh" samsung "$zip"
    src="$KROOT/src/samsung"
else
    echo "No $SAMSUNG_ZIP_NAME at $zip: using the verified-equivalent mirror" >&2
    bash "$S/fetch-source.sh" mirror
    src="$KROOT/src/mirror"
fi
bash "$S/check-fragment.sh" "$src"

# Stock modules: vendor_boot's dlkm ramdisk (304) from the stock firmware, plus
# vendor_dlkm's two, copied read-only from the phone into $KROOT/stock/vendor_dlkm.
printf '%s  %s\n' "$STOCK_VENDOR_BOOT_SHA256" "$STOCK_VENDOR_BOOT" | sha256sum -c --quiet -
printf '%s  %s\n' "$STOCK_KERNEL_SHA256" "$STOCK_KERNEL" | sha256sum -c --quiet -
mods="$KROOT/stock/vendor_ramdisk01/lib/modules"
if [[ ! -d "$mods" ]]; then
    work="$(mktemp -d)"
    (cd "$work" && "$MAGISKBOOT" unpack "$STOCK_VENDOR_BOOT" >/dev/null 2>&1 || true)
    mkdir -p "$KROOT/stock/vendor_ramdisk01"
    ramdisk="$(ls "$work"/vendor_ramdisk/dlkm.cpio 2>/dev/null || true)"
    if [[ -z "$ramdisk" ]]; then
        ramdisk="$RESEARCH/images/stock-A266MUBU1AYB8/unpacked/vendor_boot/vendor_ramdisk01"
    fi
    (cd "$KROOT/stock/vendor_ramdisk01" && { lz4 -dc "$ramdisk" 2>/dev/null || cat "$ramdisk"; } | cpio -idm --quiet)
    rm -rf "$work"
fi
dlkm="$KROOT/stock/vendor_dlkm"
[[ -f "$dlkm/input_booster_lkm.ko" ]] || echo "warning: $dlkm missing (copy /vendor_dlkm/lib/modules from the phone)" >&2

# Stock symbol table recovered from the Image (for whole-export comparison).
nm_file="$KROOT/stock/stock-vmlinux.nm"
if [[ ! -s "$nm_file" ]]; then
    if [[ ! -x "$KROOT/tools/venv-v2e/bin/vmlinux-to-elf" ]]; then
        python3 -m venv "$KROOT/tools/venv-v2e"
        "$KROOT/tools/venv-v2e/bin/pip" install -q \
            "git+https://github.com/marin-m/vmlinux-to-elf@29eac31a38db48b51c0dc6aae3be0162237601eb"
    fi
    "$KROOT/tools/venv-v2e/bin/vmlinux-to-elf" "$STOCK_KERNEL" "$KROOT/stock/stock-vmlinux.elf"
    "$CLANG/bin/llvm-nm" -n "$KROOT/stock/stock-vmlinux.elf" >"$nm_file"
fi

name_of() { echo "$1-$(basename "$src")"; }
for variant in stock oss oss-userns; do
    bash "$S/build-kernel.sh" "$variant" "$src"
    out="$KROOT/out/$(name_of "$variant")"
    python3 -I "$S/check-kmi.py" "$out" "$mods" "$dlkm" --json "$out/kmi-report.json" | tee "$out/kmi-report.txt"
    python3 -I "$S/stock-exports.py" "$STOCK_KERNEL" "$nm_file" --compare "$out/Module.symvers" \
        | tee "$out/exports-vs-stock.txt" || true
    python3 -I "$S/compare-builds.py" "$STOCK_KERNEL" "$out" "$mods" --strip "$CLANG/bin/llvm-strip" \
        --other "$KROOT/out/$(name_of stock)" >"$out/compare-stock.txt"
    python3 -I "$S/compare-functions.py" "$STOCK_KERNEL" "$nm_file" "$out/Image" "$out/System.map" \
        >"$out/functions-vs-stock.txt"
    bash "$S/make-boot-image.sh" "$out" "$KROOT/out/boot" ${BASE_BOOT_IMG:+"$BASE_BOOT_IMG"}
done

# Pipeline self-test: the stock kernel through make-boot-image.sh must give
# back the base image byte for byte.
self="$KROOT/out/selftest-stockkernel"
mkdir -p "$self"
cp "$STOCK_KERNEL" "$self/Image"
(cd "$self" && sha256sum Image >SHA256SUMS)
bash "$S/make-boot-image.sh" "$self" "$self/boot" ${BASE_BOOT_IMG:+"$BASE_BOOT_IMG"} >/dev/null
cmp "$self/boot/boot-selftest-stockkernel-$FIRMWARE.img" \
    "${BASE_BOOT_IMG:-$RESEARCH/images/moon-direct-boot-v1/boot-moon-direct-A266MUBU1AYB8.img}" \
    && echo "self-test: stock kernel reproduces the base BOOT image byte for byte"
cat "$KROOT/out/boot/SHA256SUMS"
