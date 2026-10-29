#!/usr/bin/env bash
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
SOURCE="$PROJECT_ROOT/src/a26-shell"
IMAGE="$PROJECT_ROOT/images/a26-shell-0.1.0"
TARGET=aarch64-unknown-linux-musl

# Homebrew's rustup is keg-only; prefer its proxies over a Homebrew cargo that
# has no cross targets.
if [[ "$(uname -s)" == Darwin ]] && command -v brew >/dev/null; then
    PATH="$(brew --prefix rustup)/bin:$PATH"
fi
# Without the musl cross-gcc pinned in .cargo/config.toml (e.g. on macOS), link
# through zig instead.
CARGO_BUILD=(cargo build)
if ! command -v aarch64-linux-musl-gcc >/dev/null; then
    CARGO_BUILD=(cargo zigbuild)
fi

rustup target add "$TARGET" >/dev/null
# Cargo only discovers .cargo/config.toml by walking up from its current
# directory (not from --manifest-path), so build from the source directory to
# select the musl cross-linker pinned there.
(
    cd "$SOURCE"
    cargo fmt --check
    if [[ "$(uname -s)" == Linux ]]; then
        cargo test --locked
    else
        # The shell is Linux-only (pidfd, evdev, freezer cgroups), so elsewhere
        # the test binaries are cross-built and run inside the phone's chroot.
        "$PROJECT_ROOT/scripts/a26-shell/device-test.sh" "${CARGO_BUILD[@]}"
    fi
    "${CARGO_BUILD[@]}" --locked --release --target "$TARGET"
)

"$PROJECT_ROOT/scripts/a26-shell/guards.sh"

mkdir -p "$IMAGE/bin" "$IMAGE/source" "$IMAGE/share/fonts"
# What About shows: the OSS Phone version and this build.
{
    echo "OSS Phone $(tr -d '[:space:]' <"$PROJECT_ROOT/VERSION")"
    echo "build $(git -C "$PROJECT_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)$(git -C "$PROJECT_ROOT" diff --quiet HEAD 2>/dev/null || echo +)"
    echo "built $(date -u +%Y-%m-%d)"
} >"$IMAGE/share/VERSION"
# Fallback fonts for text Inter lacks (emoji, Greek, Cyrillic, symbols).
"$PROJECT_ROOT/scripts/a26-shell/fetch-fonts.sh" "$PROJECT_ROOT/images/fonts"
while read -r name _; do
    [[ -z "$name" || "$name" == \#* ]] && continue
    install -m0644 "$PROJECT_ROOT/images/fonts/$name" "$IMAGE/share/fonts/$name"
done <"$PROJECT_ROOT/scripts/a26-shell/fonts.manifest"
install -m0755 "$SOURCE/target/$TARGET/release/a26-shell" "$IMAGE/bin/a26-shell"
install -m0755 "$SOURCE/target/$TARGET/release/a26-shellctl" "$IMAGE/bin/a26-shellctl"
install -m0755 "$SOURCE/target/$TARGET/release/a26-shellshot" "$IMAGE/bin/a26-shellshot"
install -m0755 "$SOURCE/target/$TARGET/release/a26-renderbench" "$IMAGE/bin/a26-renderbench"
install -m0755 "$SOURCE/target/$TARGET/release/a26-touchpath" "$IMAGE/bin/a26-touchpath"
install -m0755 "$SOURCE/target/$TARGET/release/moon-notifyd" "$IMAGE/bin/moon-notifyd"
install -m0755 "$SOURCE/target/$TARGET/release/moon-notifyctl" "$IMAGE/bin/moon-notifyctl"
tar --exclude=target -czf "$IMAGE/source/a26-shell-0.1.0.tar.gz" \
    -C "$PROJECT_ROOT/src" a26-shell

cat >"$IMAGE/MANIFEST.txt" <<EOF
name=a26-shell
version=0.1.0
target=$TARGET
source=src/a26-shell
display=:0
resolution=1080x2340
EOF

(cd "$IMAGE" && sha256sum MANIFEST.txt bin/a26-shell bin/a26-shellctl bin/a26-shellshot bin/a26-renderbench bin/a26-touchpath bin/moon-notifyd bin/moon-notifyctl \
    source/a26-shell-0.1.0.tar.gz > SHA256SUMS)
(cd "$IMAGE" && sha256sum -c SHA256SUMS)
