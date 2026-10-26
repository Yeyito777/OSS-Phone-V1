#!/usr/bin/env bash
# Put a pristine kernel tree under $KROOT/src/ (never inside this repo).
#
#   scripts/kernel/fetch-source.sh mirror
#       Public GPL mirror pinned by commit and tree hash -> $KROOT/src/mirror
#   scripts/kernel/fetch-source.sh samsung [/path/to/SM-A266M_LA_15_Opensource.zip]
#       Samsung's official release (downloaded by a person, see README.md;
#       default $SAMSUNG_ZIP), verified by zip, Kernel.tar.gz and extracted
#       tree hashes -> $KROOT/src/samsung; then compared with the mirror.
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

mode="${1:?usage: fetch-source.sh mirror | samsung ZIP}"
mkdir -p "$KROOT/src"

case "$mode" in
mirror)
    git_dir="$KROOT/src/mirror.git"
    if [[ ! -d "$git_dir" ]]; then
        git clone --bare "$MIRROR_URL" "$git_dir"
    fi
    git -C "$git_dir" cat-file -e "$MIRROR_COMMIT^{commit}" 2>/dev/null \
        || git -C "$git_dir" fetch origin "$MIRROR_COMMIT"
    tree="$(git -C "$git_dir" rev-parse "$MIRROR_COMMIT^{tree}")"
    [[ "$tree" == "$MIRROR_TREE" ]] || { echo "Mirror tree mismatch: $tree" >&2; exit 1; }
    out="$KROOT/src/mirror"
    if [[ ! -f "$out/.tree-$MIRROR_TREE" ]]; then
        rm -rf "$out.tmp"
        mkdir "$out.tmp"
        git -C "$git_dir" archive "$MIRROR_COMMIT" | tar -x -C "$out.tmp"
        touch "$out.tmp/.tree-$MIRROR_TREE"
        rm -rf "$out"
        mv "$out.tmp" "$out"
    fi
    echo "Source: $out (mirror commit $MIRROR_COMMIT, tree $MIRROR_TREE)"
    ;;
samsung)
    zip="${2:-$SAMSUNG_ZIP}"
    size="$(stat -c %s "$zip")"
    [[ "$size" -eq "$SAMSUNG_ZIP_SIZE" ]] \
        || { echo "Unexpected size $size (Samsung lists $SAMSUNG_ZIP_SIZE)" >&2; exit 1; }
    work="$KROOT/src/samsung-zip"
    out="$KROOT/src/samsung"
    rm -rf "$work" "$out.tmp"
    mkdir -p "$work" "$out.tmp"
    printf '%s  %s\n' "$SAMSUNG_ZIP_SHA256" "$zip" | sha256sum -c --quiet - \
        || { echo "$zip is not the pinned $SAMSUNG_ZIP_NAME" >&2; exit 1; }
    unzip -q "$zip" Kernel.tar.gz -d "$work"
    printf '%s  %s\n' "$SAMSUNG_KERNEL_TAR_SHA256" "$work/Kernel.tar.gz" | sha256sum -c --quiet -
    tar -xzf "$work/Kernel.tar.gz" -C "$out.tmp"
    # Some releases wrap the tree in one directory; unwrap it.
    if [[ ! -f "$out.tmp/Makefile" ]] && [[ "$(ls "$out.tmp" | wc -l)" -eq 1 ]]; then
        inner="$out.tmp/$(ls "$out.tmp")"
        mv "$inner" "$out.tmp.inner" && rmdir "$out.tmp" && mv "$out.tmp.inner" "$out.tmp"
    fi
    content="$(cd "$out.tmp" && find . -type f -print0 | LC_ALL=C sort -z | xargs -0 sha256sum | sha256sum | cut -d' ' -f1)"
    [[ "$content" == "$SAMSUNG_TREE_CONTENT_SHA256" ]] \
        || { echo "Extracted tree content hash mismatch: $content" >&2; exit 1; }
    rm -rf "$out" "$work"
    mv "$out.tmp" "$out"
    echo "Source: $out (Samsung $SAMSUNG_ZIP_NAME, verified)"
    if [[ -d "$KROOT/src/mirror" ]]; then
        echo "Files differing from the mirror (expected: only the saved defconfig):"
        diff -rq -x .git -x '.tree-*' "$out" "$KROOT/src/mirror" || true
    fi
    ;;
*)
    echo "unknown mode: $mode" >&2
    exit 1
    ;;
esac
