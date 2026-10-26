#!/usr/bin/env bash
# Downloads Moon's fallback fonts (fonts.manifest, pinned by SHA-256) into
# DIR (default images/fonts), reusing files that already match. Moon and the
# System app map them from /opt/a26-shell/share/fonts on the phone; see the
# header of src/a26-shell/src/text.rs.
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)"
DIR="${1:-$PROJECT_ROOT/images/fonts}"
mkdir -p "$DIR"

sha256() {
    if command -v sha256sum >/dev/null; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

while read -r name hash url; do
    [[ -z "$name" || "$name" == \#* ]] && continue
    if [[ -f "$DIR/$name" && "$(sha256 "$DIR/$name")" == "$hash" ]]; then
        continue
    fi
    curl -fsSL --retry 3 -o "$DIR/$name.part" "$url"
    if [[ "$(sha256 "$DIR/$name.part")" != "$hash" ]]; then
        echo "hash mismatch for $name from $url" >&2
        rm -f "$DIR/$name.part"
        exit 1
    fi
    mv "$DIR/$name.part" "$DIR/$name"
done <"$PROJECT_ROOT/scripts/a26-shell/fonts.manifest"
