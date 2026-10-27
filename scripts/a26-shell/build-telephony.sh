#!/usr/bin/env bash
# Build Moon's telephony service (src/moon-telephonyd): the static musl
# moon-telephonyd/moon-telephonyctl, and the bionic moon-call-audio helper
# with the Android NDK (local NDK, else on the build host NDK_HOST, default
# whale, with its ~/Workspace/tools-cache/android-ndk-r27c).
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
SOURCE="$PROJECT_ROOT/src/moon-telephonyd"
OUTPUT="$PROJECT_ROOT/images/moon-telephony"
TARGET=aarch64-unknown-linux-musl

if [[ "$(uname -s)" == Darwin ]] && command -v brew >/dev/null; then
    PATH="$(brew --prefix rustup)/bin:$PATH"
fi
CARGO_BUILD=(cargo build)
command -v aarch64-linux-musl-gcc >/dev/null || CARGO_BUILD=(cargo zigbuild)

mkdir -p "$OUTPUT"
(
    cd "$SOURCE"
    cargo fmt --check
    cargo test --locked
    "${CARGO_BUILD[@]}" --locked --release --target "$TARGET"
)
cp "$SOURCE/target/$TARGET/release/moon-telephonyd" "$SOURCE/target/$TARGET/release/moon-telephonyctl" "$OUTPUT/"
cp "$SOURCE/mobile-data" "$OUTPUT/"

if [[ -z "${NDK:-}" ]]; then
    for candidate in "$HOME"/Library/Android/sdk/ndk/* /opt/homebrew/share/android-ndk \
        "$HOME"/Android/Sdk/ndk/*; do
        [[ -d "$candidate/toolchains/llvm/prebuilt" ]] && NDK="$candidate"
    done
fi
if [[ -n "${NDK:-}" ]]; then
    cc="$(ls -d "$NDK"/toolchains/llvm/prebuilt/*/bin | head -1)/aarch64-linux-android30-clang"
    "$cc" -O2 -Wall -Wextra -o "$OUTPUT/moon-call-audio" "$SOURCE/call-audio/moon-call-audio.c" -ldl
else
    host="${NDK_HOST:-whale}"
    remote="/tmp/moon-call-audio-build.$$"
    ssh "$host" "mkdir -p $remote"
    scp -q "$SOURCE/call-audio/moon-call-audio.c" "$host:$remote/"
    ssh "$host" "cd $remote && NDK=\${NDK:-\$HOME/Workspace/tools-cache/android-ndk-r27c} &&
        \$(ls -d \$NDK/toolchains/llvm/prebuilt/*/bin | head -1)/aarch64-linux-android30-clang \
        -O2 -Wall -Wextra -o moon-call-audio moon-call-audio.c -ldl"
    scp -q "$host:$remote/moon-call-audio" "$OUTPUT/moon-call-audio"
    ssh "$host" "rm -rf $remote"
fi
echo "Built $OUTPUT: moon-telephonyd moon-telephonyctl moon-call-audio mobile-data"
