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

# Moon's X connection is deliberately long-lived and can cross the 16-bit X11
# request sequence boundary. Runtime reply/check waits on that connection are a
# shell-wide deadlock risk. Startup capability checks are allowed; the event
# loop and XTEST hot path must remain strictly one-way.
python3 - "$SOURCE/src" <<'PY'
import pathlib
import re
import sys

source_dir = pathlib.Path(sys.argv[1])
round_trip = re.compile(r"\.\s*(?:reply|check)\s*\(")
explicit_wait = re.compile(r"\b(?:wait_for_reply|poll_for_reply|check_for_error)\b")
violations = []
startup_keyboard_round_trips = {
    "let version = conn.xtest_get_version(2, 2)?.reply()?;",
    "let keyboard = conn.get_keyboard_mapping(min_keycode, count)?.reply()?;",
    "let modifiers = conn.get_modifier_mapping()?.reply()?;",
}
seen_startup_keyboard_round_trips = {line: 0 for line in startup_keyboard_round_trips}
for path in sorted(source_dir.glob("*.rs")):
    text = path.read_text()
    runtime_offset = 0
    if path.name == "main.rs":
        marker = "while !state.should_exit"
        runtime_offset = text.index(marker)
    offset = 0
    for number, raw_line in enumerate(text.splitlines(keepends=True), 1):
        line = raw_line.rstrip("\r\n")
        line_offset = offset
        offset += len(raw_line)
        if path.name == "main.rs" and line_offset < runtime_offset:
            continue
        normalized = line.strip()
        if path.name == "keyboard.rs" and normalized in startup_keyboard_round_trips:
            # These exact calls construct the immutable startup XTEST/XKB map.
            seen_startup_keyboard_round_trips[normalized] += 1
            continue
        if round_trip.search(line) or explicit_wait.search(line):
            violations.append(f"{path.name}:{number}:{line.strip()}")
for expression, count in seen_startup_keyboard_round_trips.items():
    if count != 1:
        violations.append(f"startup allowlist count={count}: {expression}")
if violations:
    raise SystemExit(
        "runtime X11 synchronous-operation guard failed:\n" + "\n".join(violations)
    )
print("runtime X11 round-trip guard: PASS")
PY
if grep -Fq 'thread::sleep' "$SOURCE/src/main.rs"; then
    echo 'event-loop regression: runtime thread::sleep reintroduced' >&2
    exit 31
fi
grep -Fq 'libc::poll(' "$SOURCE/src/main.rs" || {
    echo 'event-loop regression: blocking poll is missing' >&2
    exit 32
}
echo 'blocking event-loop guard: PASS'
sh -n "$PROJECT_ROOT/scripts/device/moon-boot-supervisor.sh"

mkdir -p "$IMAGE/bin" "$IMAGE/source"
install -m0755 "$SOURCE/target/$TARGET/release/a26-shell" "$IMAGE/bin/a26-shell"
install -m0755 "$SOURCE/target/$TARGET/release/a26-shellctl" "$IMAGE/bin/a26-shellctl"
install -m0755 "$SOURCE/target/$TARGET/release/a26-shellshot" "$IMAGE/bin/a26-shellshot"
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

(cd "$IMAGE" && sha256sum MANIFEST.txt bin/a26-shell bin/a26-shellctl bin/a26-shellshot \
    source/a26-shell-0.1.0.tar.gz > SHA256SUMS)
(cd "$IMAGE" && sha256sum -c SHA256SUMS)
