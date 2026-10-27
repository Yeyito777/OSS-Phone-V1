#!/usr/bin/env bash
# Build Moon's native audio binaries (src/moon-audiod) with the Android NDK.
# NDK defaults to the newest under ~/Library/Android/sdk/ndk or Homebrew's
# android-ndk cask; without one, MOON_AUDIOD_BUILD_HOST=user@host builds there
# (NDK=path on that host, default ~/Workspace/tools-cache/android-ndk-r27c).
# The firmware's libaudioclient.so is pulled from the phone once into
# build/moon-audiod/.
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
HOST="${MOON_AUDIOD_BUILD_HOST:-}"
if [[ -z "$HOST" && ! -d "${NDK:-/nonexistent}/toolchains/llvm/prebuilt" ]]; then
    echo "Android NDK not found; set NDK, run: brew install --cask android-ndk," >&2
    echo "or set MOON_AUDIOD_BUILD_HOST to a host with one." >&2
    exit 1
fi

mkdir -p "$BUILD" "$OUTPUT"
if [[ ! -f "$BUILD/libaudioclient.so" ]]; then
    source "$PROJECT_ROOT/scripts/a26-shell/common.sh"
    SERIAL="$(a26_resolve_serial)"
    adb -s "$SERIAL" pull /system/lib64/libaudioclient.so "$BUILD/libaudioclient.so" >/dev/null
fi
if [[ -d "${NDK:-/nonexistent}/toolchains/llvm/prebuilt" ]]; then
    NDK="$NDK" AUDIOCLIENT="$BUILD/libaudioclient.so" sh "$PROJECT_ROOT/src/moon-audiod/build.sh" "$OUTPUT"
else
    remote_dir=".cache/moon-audiod-build"
    ssh "$HOST" "mkdir -p $remote_dir/src"
    rsync -a --delete "$PROJECT_ROOT/src/moon-audiod/" "$HOST:$remote_dir/src/"
    rsync -a "$BUILD/libaudioclient.so" "$HOST:$remote_dir/libaudioclient.so"
    # Compiler temporaries stay in the build directory (a shared /tmp there
    # may be over quota).
    ssh "$HOST" "cd $remote_dir && mkdir -p tmp && TMPDIR=\$PWD/tmp \
        NDK=\${NDK:-\$HOME/Workspace/tools-cache/android-ndk-r27c} \
        AUDIOCLIENT=\$PWD/libaudioclient.so sh src/build.sh out"
    rsync -a "$HOST:$remote_dir/out/" "$OUTPUT/"
fi
echo "Built $OUTPUT/moon-audiod, moon-permissiond, moon-audio-volume"
