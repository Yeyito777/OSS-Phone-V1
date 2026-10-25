#!/usr/bin/env bash
# Download and verify the exact clang that built the stock kernel.
#
#   scripts/kernel/fetch-toolchain.sh
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

if [[ -x "$CLANG/bin/clang" ]]; then
    echo "Toolchain already present: $CLANG"
else
    mkdir -p "$KROOT/toolchain"
    tmp="$(mktemp -d "$KROOT/toolchain/.dl.XXXXXX")"
    trap 'rm -rf "$tmp"' EXIT
    curl -fL --retry 3 -o "$tmp/clang.tar.gz" "$CLANG_URL"
    mkdir "$tmp/$CLANG_DIR_NAME"
    tar -xzf "$tmp/clang.tar.gz" -C "$tmp/$CLANG_DIR_NAME"
    mv "$tmp/$CLANG_DIR_NAME" "$CLANG"
fi

# gitiles tarballs are not byte-stable, so verify the binaries themselves.
(cd "$CLANG/bin" && sha256sum -c --quiet) <<SUMS
$CLANG_BIN_SHA256  clang-12
$LLD_BIN_SHA256  ld.lld
SUMS
"$CLANG/bin/clang" --version | head -1
