#!/usr/bin/env bash
# Build the kernel Image (and all in-tree modules, for comparison) from a
# pristine tree with the stock A266MUBU1AYB8 config.
#
#   scripts/kernel/build-kernel.sh stock [SOURCE_DIR]   # unmodified reproduction
#   scripts/kernel/build-kernel.sh oss   [SOURCE_DIR]   # + configs/oss-phone.fragment
#   scripts/kernel/build-kernel.sh oss-userns [SOURCE_DIR]  # + configs/oss-userns.fragment
#
# SOURCE_DIR defaults to $KROOT/src/samsung if it exists, else $KROOT/src/mirror.
# Results: $KROOT/out/<variant>-<source>/ (Image, vmlinux, System.map,
# Module.symvers, .config, modules/, build.log).
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

variant="${1:?usage: build-kernel.sh stock|oss [SOURCE_DIR]}"
src="${2:-}"
if [[ -z "$src" ]]; then
    if [[ -d "$KROOT/src/samsung" ]]; then src="$KROOT/src/samsung"; else src="$KROOT/src/mirror"; fi
fi
[[ -f "$src/Makefile" ]] || { echo "No kernel tree at $src (run fetch-source.sh)" >&2; exit 1; }
[[ -x "$CLANG/bin/clang" ]] || { echo "No toolchain (run fetch-toolchain.sh)" >&2; exit 1; }

case "$variant" in
stock)
    build_user="$STOCK_BUILD_USER"; build_host="$STOCK_BUILD_HOST"
    build_timestamp="$STOCK_BUILD_TIMESTAMP"; fragments=()
    ;;
oss|oss-userns)
    # Same release string as stock, so nothing that keys on uname -r changes;
    # the banner's user@host and date mark the OSS build.
    build_user=oss-phone; build_host=oss-phone-v1
    build_timestamp="Sun Oct 25 00:00:00 UTC 2026"
    fragments=("$KERNEL_SCRIPTS/configs/oss-phone.fragment")
    [[ "$variant" == oss-userns ]] && fragments+=("$KERNEL_SCRIPTS/configs/oss-userns.fragment")
    ;;
*) echo "unknown variant: $variant" >&2; exit 1 ;;
esac

name="$variant-$(basename "$src")"
work="$KROOT/build/$name"
out="$KROOT/out/$name"
rm -rf "$work" "$out"
mkdir -p "$work" "$out"
echo "Copying $src -> $work/src"
cp -a "$src" "$work/src"
rm -f "$work/src"/.tree-*

# Release suffix exactly as stock (no git in the tree, so setlocalversion
# takes .scmversion verbatim): 5.10.223 + -android12-9-30597860-abA266MUBU1AYB8.
printf '%s\n' "${KERNEL_RELEASE#5.10.223}" >"$work/src/.scmversion"
cp "$STOCK_CONFIG" "$work/src/.config"
for fragment in "${fragments[@]}"; do
    { echo; echo "# --- $(basename "$fragment") ---"; grep -E '^(CONFIG_|# CONFIG_)' "$fragment"; } >>"$work/src/.config"
done

make_args=(
    ARCH=arm64 LLVM=1 LLVM_IAS=1
    CROSS_COMPILE=aarch64-linux-gnu- CROSS_COMPILE_COMPAT=arm-linux-gnueabi-
    PLATFORM_VERSION=12 ANDROID_MAJOR_VERSION=s TARGET_SOC=s5e8825
    # Production ("user") build: DEFEX, MZ and NGKSM pick their non-debug
    # code from this (without it DEFEX builds permissive, e.g. no
    # lower_adb_permission adbd check, unlike the stock kernel).
    TARGET_BUILD_VARIANT=user
    KBUILD_BUILD_USER="$build_user" KBUILD_BUILD_HOST="$build_host"
    KBUILD_BUILD_TIMESTAMP="$build_timestamp" KBUILD_BUILD_VERSION=1
)
sandbox() {
    bwrap --ro-bind /usr /usr --ro-bind /etc /etc \
        --symlink usr/lib /lib --symlink usr/lib /lib64 --symlink usr/bin /bin --symlink usr/bin /sbin \
        --proc /proc --dev /dev --tmpfs /tmp \
        --ro-bind "$CLANG" "$SAMSUNG_CLANG_PATH" \
        --bind "$work/src" "$SAMSUNG_SRC_PATH" --chdir "$SAMSUNG_SRC_PATH" \
        --unshare-net --clearenv \
        --setenv PATH "$SAMSUNG_CLANG_PATH/bin:/usr/bin" --setenv HOME /tmp --setenv LC_ALL C --setenv TZ UTC \
        "$@"
}

sandbox make "${make_args[@]}" olddefconfig >"$out/olddefconfig.log" 2>&1
for fragment in "${fragments[@]}"; do
    while read -r line; do
        grep -qxF "$line" "$work/src/.config" || { echo "Fragment line did not stick: $line" >&2; exit 1; }
    done < <(grep -E '^(CONFIG_|# CONFIG_)' "$fragment")
done
diff <(grep -E '^(CONFIG_|# CONFIG_)' "$STOCK_CONFIG") <(grep -E '^(CONFIG_|# CONFIG_)' "$work/src/.config") \
    >"$out/config-vs-stock.diff" || true
echo "Config lines differing from stock: $(grep -c '^[<>]' "$out/config-vs-stock.diff" || true)"
# The only tolerated stock drift: =m options whose driver is absent from the
# published tree (Samsung's release omits CONFIG_SEC_AUTO_INPUT, which the
# stock firmware does not ship as a module either). Built-in code must match.
if [[ "$variant" == stock ]] && grep '^[<>]' "$out/config-vs-stock.diff" | grep -qvE '^< CONFIG_[A-Z0-9_]+=m$'; then
    echo "Stock config did not survive olddefconfig:" >&2
    cat "$out/config-vs-stock.diff" >&2
    exit 1
fi

# For the reproduction, embed the phone's config text byte for byte
# (IKCONFIG, /proc/config.gz). The published tree lacks a few Kconfig entries
# Samsung's had (SEC_AUTO_INPUT, two kunit menus), so the regenerated .config
# text differs by those lines although every value is the same; point
# kernel/config_data at the stock file instead. Build-only, values unchanged.
if [[ "$variant" == stock ]]; then
    cp "$STOCK_CONFIG" "$work/src/kernel/stock.config"
    sed -i 's|^\$(obj)/config_data: \$(KCONFIG_CONFIG) FORCE$|$(obj)/config_data: $(srctree)/kernel/stock.config FORCE|' "$work/src/kernel/Makefile"
    grep -q 'kernel/stock.config FORCE' "$work/src/kernel/Makefile"
fi

echo "Building $name ($(nproc) jobs); log: $out/build.log"
start=$(date +%s)
if ! sandbox make "${make_args[@]}" -j"$(nproc)" Image modules >"$out/build.log" 2>&1; then
    tail -40 "$out/build.log" >&2
    exit 1
fi
echo "Built in $(( $(date +%s) - start )) s"

s="$work/src"
cp "$s/arch/arm64/boot/Image" "$s/vmlinux" "$s/System.map" "$s/Module.symvers" "$s/.config" "$out/"
sandbox make "${make_args[@]}" -s kernelrelease >"$out/kernelrelease"
mkdir -p "$out/modules"
find "$s" -name '*.ko' -exec cp {} "$out/modules/" \;
(cd "$out" && sha256sum Image vmlinux >SHA256SUMS)
echo "kernelrelease: $(cat "$out/kernelrelease")"
{ strings "$out/Image" | grep -m1 '^Linux version '; } || true  # grep -m1 SIGPIPEs strings
cat "$out/SHA256SUMS"
