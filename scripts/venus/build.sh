#!/usr/bin/env bash
# Builds the Venus experiment (notes/gpu/BROWSER.md, option C) into
# build/venus/:
#   android/  virglrenderer's vtest server, render server and library, with
#             the NDK (bionic: they run beside moon-compositor on the Mali's
#             Vulkan driver);
#   debian/   Mesa's Venus Vulkan driver, the Vulkan loader and their
#             libraries from Debian bookworm arm64 (the Browser's root), and
#             vkfirst (src/venus/vkfirst.c), built in an arm64 container.
# Runs on a Linux host with podman (arm64 emulation) and git; NDK as for
# scripts/gpu/build.sh (default: the r27c under ~/Workspace/tools-cache).
#   scripts/venus/build.sh
set -euo pipefail
PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$PROJECT_ROOT/build/venus"
WORK="${VENUS_WORK:-$OUT/work}"
NDK="${NDK:-$HOME/Workspace/tools-cache/android-ndk-r27c}"
VIRGL_REV=e0a1caf          # virglrenderer main, 2026-10
MESA_BRANCH=25.2           # Mesa 25.2.8
mkdir -p "$WORK" "$OUT"

if [[ ! -d "$WORK/virglrenderer" ]]; then
    git clone -q https://gitlab.freedesktop.org/virgl/virglrenderer.git "$WORK/virglrenderer"
fi
git -C "$WORK/virglrenderer" checkout -q -f "$VIRGL_REV"
# vtest is only built with the OpenGL renderer (vrend); Venus alone needs
# neither it nor libepoxy.
sed -i 's/^if with_vrend and not with_host_windows$/if not with_host_windows/' \
    "$WORK/virglrenderer/meson.build"
if [[ ! -d "$WORK/mesa" ]]; then
    git clone -q --depth 1 -b "$MESA_BRANCH" https://gitlab.freedesktop.org/mesa/mesa.git "$WORK/mesa"
fi

# --- android: virglrenderer with the NDK
python3 -m venv "$WORK/venv" >/dev/null
"$WORK/venv/bin/pip" -q install meson mako
TC="$(ls -d "$NDK"/toolchains/llvm/prebuilt/*/bin | head -1)"
cat >"$WORK/android-arm64.cross" <<EOF
[binaries]
c = '$TC/aarch64-linux-android30-clang'
cpp = '$TC/aarch64-linux-android30-clang++'
ar = '$TC/llvm-ar'
strip = '$TC/llvm-strip'
pkg-config = '/bin/false'

[host_machine]
system = 'android'
cpu_family = 'aarch64'
cpu = 'armv8'
endian = 'little'
EOF
(
    cd "$WORK/virglrenderer"
    rm -rf out-android
    # NDK stand-ins for the AOSP headers Mesa's util code expects on Android.
    PATH="$WORK/venv/bin:$PATH" CFLAGS="-I$PROJECT_ROOT/src/venus/ndk-shim" LDFLAGS="-llog" \
        meson setup out-android --cross-file "$WORK/android-arm64.cross" \
        -Dvrend=false -Dvenus=true -Dtests=false -Drender-server-worker=thread \
        -Dbuildtype=release >/dev/null
    PATH="$WORK/venv/bin:$PATH" ninja -C out-android >/dev/null
)
rm -rf "$OUT/android" "$OUT/debian"
mkdir -p "$OUT/android" "$OUT/debian/lib"
cp "$WORK/virglrenderer/out-android/vtest/virgl_test_server" \
    "$WORK/virglrenderer/out-android/server/virgl_render_server" \
    "$WORK/virglrenderer/out-android/src/libvirglrenderer.so" "$OUT/android/"

# --- debian: Mesa's Venus driver and vkfirst in a bookworm arm64 container
cp "$PROJECT_ROOT/src/venus/vkfirst.c" "$WORK/vkfirst.c"
podman run --rm --arch arm64 -v "$WORK:/work:Z" -v "$OUT/debian:/bundle:Z" \
    docker.io/library/debian:bookworm sh -euc '
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends build-essential pkg-config python3 \
        python3-venv bison flex libdrm-dev libx11-dev libx11-xcb-dev libxcb1-dev \
        libxcb-dri3-dev libxcb-present-dev libxcb-shm0-dev libxcb-randr0-dev libxcb-sync-dev \
        libxcb-xfixes0-dev libxshmfence-dev libxrandr-dev libexpat1-dev zlib1g-dev ninja-build \
        libwayland-dev wayland-protocols libwayland-bin libvulkan-dev libvulkan1 >/dev/null
    python3 -m venv /opt/venv && /opt/venv/bin/pip -q install meson mako pyyaml packaging
    export PATH=/opt/venv/bin:$PATH
    meson setup /tmp/mesa /work/mesa -Dbuildtype=release -Dprefix=/usr \
        -Dvulkan-drivers=virtio -Dgallium-drivers= -Dplatforms=x11,wayland \
        -Dglx=disabled -Degl=disabled -Dgbm=disabled -Dopengl=false -Dgles1=disabled \
        -Dgles2=disabled -Dllvm=disabled -Dvalgrind=disabled -Dlibunwind=disabled \
        -Dzstd=disabled -Dexpat=enabled -Dvulkan-layers= -Dtools= -Dbuild-tests=false \
        -Dvideo-codecs= >/dev/null
    ninja -C /tmp/mesa >/dev/null
    so=/tmp/mesa/src/virtio/vulkan/libvulkan_virtio.so
    cp $so /bundle/lib/
    for l in $(ldd $so | awk "/=>/ {print \$3}") /usr/lib/aarch64-linux-gnu/libvulkan.so.1; do
        case $l in */libc.so*|*/libm.so*|*/libpthread.so*|*/ld-linux*) ;; *) cp -L $l /bundle/lib/ ;; esac
    done
    gcc -O2 -Wall -o /bundle/vkfirst /work/vkfirst.c -ldl
'
cat >"$OUT/debian/venus_icd.json" <<'EOF'
{"ICD": {"api_version": "1.4.318", "library_arch": "64", "library_path": "/tmp/venus/lib/libvulkan_virtio.so"}, "file_format_version": "1.0.1"}
EOF
echo "Built $OUT (android: $(ls "$OUT/android" | tr '\n' ' '); debian: $(ls "$OUT/debian/lib" | wc -l) libraries + vkfirst)"
