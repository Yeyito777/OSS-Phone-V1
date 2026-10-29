#!/bin/sh
# Build the Moon-mode native audio binaries with the Android NDK.
#   NDK=/path/to/android-ndk-r27c src/moon-audiod/build.sh OUT_DIR
# audio-policy.cpp links against the firmware's libaudioclient.so (a private
# platform library); pull it once with:
#   adb pull /system/lib64/libaudioclient.so build/moon-audiod/
set -eu
here=$(cd "$(dirname "$0")" && pwd)
out=${1:?output directory required}
: "${NDK:?set NDK to an Android NDK (r27 tested)}"
lib=${AUDIOCLIENT:-$here/../../build/moon-audiod/libaudioclient.so}
cc="$(ls -d "$NDK"/toolchains/llvm/prebuilt/*/bin | head -1)/aarch64-linux-android30-clang"
mkdir -p "$out"
"$cc++" -O2 -Wall -Wextra -fno-exceptions -fno-rtti -static-libstdc++ \
    -x c "$here/moon-audiod.c" -x c++ "$here/audio-policy.cpp" -x none \
    -o "$out/moon-audiod" "$lib" -laaudio -lm -Wl,--allow-shlib-undefined
"$cc" -O2 -Wall -Wextra -o "$out/moon-permissiond" "$here/moon-permissiond.c" \
    -lbinder_ndk -ldl
"$cc++" -O2 -Wall -fno-exceptions -fno-rtti -static-libstdc++ \
    -o "$out/moon-audio-volume" "$here/moon-audio-volume.cpp" "$lib" \
    -Wl,--allow-shlib-undefined
"$cc" -O2 -Wall -Wextra -o "$out/moon-voice-capture" "$here/moon-voice-capture.c" -laaudio
