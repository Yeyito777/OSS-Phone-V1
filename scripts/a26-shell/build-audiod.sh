#!/usr/bin/env bash
# Build Moon's native audio binaries (src/moon-audiod) with the Android NDK.
# NDK defaults to the newest under ~/Library/Android/sdk/ndk or Homebrew's
# android-ndk cask; the firmware's libaudioclient.so is pulled from the phone
# once into build/moon-audiod/.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
BUILD="$PROJECT_ROOT/build/moon-audiod"
OUTPUT="$PROJECT_ROOT/images/moon-audiod"

if [[ -z "${NDK:-}" ]]; then
    for candidate in "$HOME"/Library/Android/sdk/ndk/* /opt/homebrew/share/android-ndk \
        "$HOME"/Android/Sdk/ndk/*; do
        [[ -d "$candidate/toolchains/llvm/prebuilt" ]] && NDK="$candidate"
    done
fi
[[ -n "${NDK:-}" ]] || {
    echo "Android NDK not found; set NDK or run: brew install --cask android-ndk" >&2
    exit 1
}
export NDK

mkdir -p "$BUILD" "$OUTPUT"
if [[ ! -f "$BUILD/libaudioclient.so" ]]; then
    source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
    SERIAL="$(a26_resolve_serial)"
    adb -s "$SERIAL" pull /system/lib64/libaudioclient.so "$BUILD/libaudioclient.so" >/dev/null
fi
AUDIOCLIENT="$BUILD/libaudioclient.so" sh "$PROJECT_ROOT/src/moon-audiod/build.sh" "$OUTPUT"
echo "Built $OUTPUT/moon-audiod, moon-permissiond, moon-audio-volume"
