#!/bin/sh
# Build Moon's GPU compositor binaries with the Android NDK (bionic: they run
# outside the Alpine chroot, next to Android's Mali driver).
#   NDK=/path/to/android-ndk-r27c src/moon-gpu/build.sh OUT_DIR
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:?output directory required}
: "${NDK:?set NDK to an Android NDK (r27 tested)}"
cc="$(ls -d "$NDK"/toolchains/llvm/prebuilt/*/bin | head -1)/aarch64-linux-android30-clang"
mkdir -p "$out"
flags="-O2 -Wall -Wextra -Wno-unused-parameter -std=c11"
"$cc" $flags -o "$out/moon-gpu-poc" "$here/poc.c" "$here/gpu.c" "$here/kms.c" \
    -lEGL -lGLESv3 -lm
"$cc" $flags -o "$out/moon-compositor" "$here/compositor.c" "$here/render.c" "$here/cpu.c" \
    "$here/gpu.c" "$here/kms.c" -lEGL -lGLESv3 -lm
"$cc" $flags -o "$out/moon-gpu-proto-test" "$here/proto-test.c"
