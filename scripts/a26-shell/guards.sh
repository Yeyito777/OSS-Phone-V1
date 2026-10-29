#!/usr/bin/env bash
# Source guards build.sh runs on every build (also run alone by agents'
# checks): no synchronous X11 round trips or sleeps in Moon's event loop, and
# the boot supervisor still parses.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
SOURCE="$PROJECT_ROOT/src/a26-shell"

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
    if path.name == "clipboard.rs":
        # The clipboard thread's own X connection waits as X clients do; it
        # never touches the event loop's connection.
        continue
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
