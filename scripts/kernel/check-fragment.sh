#!/usr/bin/env bash
# Static KMI screen for configs/oss-phone.fragment: an option is only
# acceptable if no header a module could include tests it, so prebuilt
# modules cannot have compiled against a different inline/struct view.
# (The authoritative check is still check-kmi.py plus compare-builds.sh on
# the built result.)
#
# An option that headers do test needs a "# kmi-reviewed: SYMBOL" line in its
# fragment next to the written justification; it is then listed as REVIEWED.
#
#   scripts/kernel/check-fragment.sh [SOURCE_DIR] [FRAGMENT...]
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

src="${1:-$KROOT/src/mirror}"
shift || true
fragments=("$@")
[[ ${#fragments[@]} -gt 0 ]] || fragments=("$KERNEL_SCRIPTS/configs/oss-phone.fragment" "$KERNEL_SCRIPTS/configs/oss-userns.fragment")
status=0
for fragment in "${fragments[@]}"; do
echo "== $(basename "$fragment")"
while read -r line; do
    sym="$(sed -E 's/^(# )?CONFIG_([A-Za-z0-9_]+).*/\2/' <<<"$line")"
    stock="$(grep -E "^(# )?CONFIG_${sym}[= ]" "$STOCK_CONFIG" || echo "(absent)")"
    hits="$(grep -rlw "CONFIG_$sym" "$src/include" "$src/arch/arm64/include" 2>/dev/null || true)"
    if [[ -n "$hits" ]] && grep -qxF "# kmi-reviewed: $sym" "$fragment"; then
        echo "REVIEWED $sym: tested in headers: $(tr '\n' ' ' <<<"$hits")"
    elif [[ -n "$hits" ]]; then
        echo "REJECT $sym: tested in headers: $(tr '\n' ' ' <<<"$hits")"
        status=1
    else
        echo "ok     $sym: header-free (stock: $stock -> $line)"
    fi
done < <(grep -E '^(CONFIG_|# CONFIG_)' "$fragment")
done
exit $status
