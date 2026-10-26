#!/usr/bin/env bash
# Build Moon's GPU compositor binaries (src/moon-gpu) with the Android NDK
# into images/moon-gpu/. NDK defaults to the newest under ~/Library/Android/
# sdk/ndk, Homebrew's android-ndk cask or ~/Android/Sdk/ndk. Without a local
# NDK, set MOON_GPU_BUILD_HOST=user@host (with NDK=/path there) to build
# over ssh.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
OUTPUT="$PROJECT_ROOT/images/moon-gpu"
source "$PROJECT_ROOT/scripts/gpu/source-hash.sh"
mkdir -p "$OUTPUT"
# Stamped only once this build succeeds (scripts/gpu/install.sh checks it).
SOURCE_HASH="$(moon_gpu_source_hash)"
rm -f "$OUTPUT/SOURCE"

if [[ -z "${NDK:-}" ]]; then
    for candidate in "$HOME"/Library/Android/sdk/ndk/* /opt/homebrew/share/android-ndk \
        "$HOME"/Android/Sdk/ndk/*; do
        [[ -d "$candidate/toolchains/llvm/prebuilt" ]] && NDK="$candidate"
    done
fi

if [[ -n "${NDK:-}" && -d "$NDK/toolchains/llvm/prebuilt" ]]; then
    NDK="$NDK" sh "$PROJECT_ROOT/src/moon-gpu/build.sh" "$OUTPUT"
elif [[ -n "${MOON_GPU_BUILD_HOST:-}" ]]; then
    remote_dir=".cache/moon-gpu-build"
    ssh "$MOON_GPU_BUILD_HOST" "mkdir -p $remote_dir"
    rsync -a --delete "$PROJECT_ROOT/src/moon-gpu/" "$MOON_GPU_BUILD_HOST:$remote_dir/src/"
    ssh "$MOON_GPU_BUILD_HOST" "cd $remote_dir && NDK=\${NDK:-${NDK:-}} sh src/build.sh out"
    rsync -a "$MOON_GPU_BUILD_HOST:$remote_dir/out/" "$OUTPUT/"
else
    echo "Android NDK not found; set NDK, run: brew install --cask android-ndk," >&2
    echo "or set MOON_GPU_BUILD_HOST to a host with one (and NDK=path there)." >&2
    exit 1
fi
echo "$SOURCE_HASH" >"$OUTPUT/SOURCE"
echo "Built $OUTPUT/moon-compositor, moon-gpu-poc (sources $SOURCE_HASH)"
