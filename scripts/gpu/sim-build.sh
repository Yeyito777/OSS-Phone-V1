#!/bin/sh
# Builds moon-compositor-sim (src/moon-gpu/sim.c): moon-compositor's own
# code on a host Linux, with the phone's KMS and GPU simulated, for testing
# Moon against the real compositor logic in a sandbox or on Xvfb. Needs a C
# compiler and the EGL/GLES headers and libraries (mesa).
#   scripts/gpu/sim-build.sh OUT_DIR
set -eu
here=$(cd "$(dirname "$0")/../../src/moon-gpu" && pwd)
out=${1:?output directory required}
mkdir -p "$out"
${CC:-cc} -O2 -Wall -Wextra -Wno-unused-parameter -std=c11 -o "$out/moon-compositor-sim" \
    "$here/compositor.c" "$here/render.c" "$here/cpu.c" "$here/sim.c" \
    -lEGL -lGLESv2 -lm -lpthread
echo "Built $out/moon-compositor-sim"
