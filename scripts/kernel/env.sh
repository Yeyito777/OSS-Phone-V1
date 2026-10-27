# Pinned inputs for the OSS Phone kernel build (sourced by the other scripts).
# Everything heavy lives under $KROOT on the build host, never in this repo.

KROOT="${KROOT:-$HOME/oss-kernel}"
KERNEL_SCRIPTS="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Target firmware and the running kernel it ships.
FIRMWARE=A266MUBU1AYB8
KERNEL_RELEASE=5.10.223-android12-9-30597860-abA266MUBU1AYB8
STOCK_CONFIG="$KERNEL_SCRIPTS/configs/$FIRMWARE.config"

# Toolchain: the exact clang named in the stock /proc/version,
# "Android (7211189, based on r416183) clang version 12.0.4". It is the AOSP
# prebuilt clang-r416183, which lived in platform/prebuilts/clang/host/linux-x86
# only between commits 1006a161 (2021-03-16) and 2af2d1a1 (2021-04-26).
CLANG_COMMIT=1006a161859d41b822c06cacfbf76517a331ff5e
CLANG_DIR_NAME=clang-r416183
CLANG_URL="https://android.googlesource.com/platform/prebuilts/clang/host/linux-x86/+archive/$CLANG_COMMIT/$CLANG_DIR_NAME.tar.gz"
CLANG_BIN_SHA256=d505bf5264ad893a5aca90c543adff6152967d695acf50a199a1210e645d71ca # bin/clang-12
LLD_BIN_SHA256=84468a97fedc56ffa2a477050863d9ac231fbc507f3826263237eda4dc0a23d9   # bin/ld.lld
CLANG="$KROOT/toolchain/$CLANG_DIR_NAME"

# Source: Samsung's own SM-A266M_LA_15_Opensource.zip for firmware
# A266MUBU1AYB8 (opensource.samsung.com upload 13000, attach 38622). Getting it
# needs a person to accept Samsung's download terms (README.md); build-all.sh
# uses it from $SAMSUNG_ZIP when present. Its kernel tree is file-for-file
# identical to the public mirror below except the saved defconfig's
# LOCALVERSION, and both build to bit-identical kernels, so the mirror is a
# verified fallback for hosts without the zip.
SAMSUNG_ZIP_NAME=SM-A266M_LA_15_Opensource.zip
SAMSUNG_ZIP_SIZE=278700980
SAMSUNG_ZIP_SHA256=6b993454a1ddd97fd36a94b0ee61bbe97165db5c22674f82010aece86210453f
SAMSUNG_KERNEL_TAR_SHA256=f9ca20b4addcb9042c294969fc589f1aeaca85c42ebdfd6208949d1bc0ecd357
# sha256 of `find . -type f | sort | xargs sha256sum` over the extracted tree.
SAMSUNG_TREE_CONTENT_SHA256=7ce9859ea9edcbe7ae127010f93925d7592d6d7b329983e17dd0e8bbdd3c0f38
SAMSUNG_ZIP="${SAMSUNG_ZIP:-$KROOT/dl/$SAMSUNG_ZIP_NAME}"
MIRROR_URL=https://github.com/physwizz/A266m-V-LA.git
MIRROR_COMMIT=d569a47723e6eade9e97756db0b65c477f4db570
MIRROR_TREE=d779b6c1f9810bc9a78d75a58f9163ee3b82dc23

# Samsung built in-tree at this path; it is baked into the stock config
# (CONFIG_UNUSED_KSYMS_WHITELIST) and so into /proc/config.gz. The build runs
# in a bwrap sandbox with the source mounted here, so nothing is rewritten.
SAMSUNG_SRC_PATH=/home/dpi/qb5_8814/workspace/P4_1716/android/kernel/s5e8825

# ...and the toolchain lived here: the compiler's own headers (stdarg.h) are
# referenced by path in the debug info, which feeds ThinLTO module hashes and
# build IDs. Found by rebuilding kperfmon.ko until it matched Samsung's byte
# for byte (build ID included).
SAMSUNG_CLANG_PATH=/home/dpi/qb5_8814/workspace/P4_1716/android/prebuilts/clang/host/linux-x86/clang-r416183

# Stock build identity, reproduced in the banner.
STOCK_BUILD_USER=dpi
STOCK_BUILD_HOST=21DND505
STOCK_BUILD_TIMESTAMP="Fri Feb 21 20:40:50 KST 2025"

# Research workspace (stock firmware, magiskboot, AVB key).
RESEARCH="${A26_RESEARCH:-$HOME/Workspace/research/linux-galaxy-a26}"
MAGISKBOOT="${MAGISKBOOT:-$RESEARCH/tools/magiskboot-x86_64}"
STOCK_KERNEL="$RESEARCH/images/stock-A266MUBU1AYB8/unpacked/boot/kernel"
STOCK_KERNEL_SHA256=944905ed2dc05202fc0f2242f96d83c5c2c9c5f01817c299cdb40e3183ccd2bb
STOCK_VENDOR_BOOT="$RESEARCH/images/stock-A266MUBU1AYB8/vendor_boot.img"
STOCK_VENDOR_BOOT_SHA256=74723185dc3ae25be122418e8d401b91cfd27eac688395e97067f77a849692d2
