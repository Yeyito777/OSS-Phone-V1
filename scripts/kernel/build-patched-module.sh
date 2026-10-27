#!/usr/bin/env bash
# Rebuild one in-tree (vendor) module with a patch, against a kernel built by
# build-kernel.sh, in a throw-away copy of its build tree, then check it like
# the vendor modules and compare it with Samsung's prebuilt.
#
#   scripts/kernel/build-patched-module.sh PATCH MODULE_KO_PATH [KERNEL_BUILD]
#   e.g. scripts/kernel/build-patched-module.sh \
#            scripts/kernel/patches/mali-kbase-pid-namespace.patch \
#            drivers/gpu/arm/bv_r32p1/mali_kbase.ko
#
# KERNEL_BUILD defaults to oss-samsung (the installed OSS kernel, BOOT
# 8c0461ee). Output: $KROOT/out/modules/<patch name>/<module>.ko
# (debug-stripped, as the vendor modules ship), kmi-report.txt, SHA256SUMS,
# function-diff.txt (functions whose code differs from Samsung's prebuilt).
set -euo pipefail
. "$(dirname "${BASH_SOURCE[0]}")/env.sh"

patch_file="$(cd "$(dirname "${1:?patch required}")" && pwd)/$(basename "$1")"
ko_path="${2:?module .ko path inside the tree required}"
kname="${3:-oss-samsung}"
ktree="$KROOT/build/$kname/src"
kout="$KROOT/out/$kname"
[[ -f "$ktree/Module.symvers" ]] || { echo "No built kernel $kname" >&2; exit 1; }
name="$(basename "$patch_file" .patch)"
work="$KROOT/build/$kname-$name"
out="$KROOT/out/modules/$name"
mod="$(basename "$ko_path")"
stock_ko="$KROOT/stock/vendor_ramdisk01/lib/modules/$mod"
[[ -f "$stock_ko" ]] || { echo "No Samsung prebuilt $mod" >&2; exit 1; }

rm -rf "$work" "$out"
mkdir -p "$out"
echo "Copying $ktree -> $work/src"
mkdir -p "$work"
cp -a "$ktree" "$work/src"
(cd "$work/src" && patch -p1 --no-backup-if-mismatch <"$patch_file")
# In-tree single-target modpost reads only vmlinux.symvers; in this throw-away
# copy give it every export of the kernel *and* the modules (vendor modules
# import from each other, e.g. mali_kbase from cmupmucal), with their CRCs.
# (minus the rebuilt module's own exports).
awk -F'\t' -v m="${ko_path%.ko}" '$3 != m' "$kout/Module.symvers" >"$work/src/vmlinux.symvers"
touch -r "$kout/Module.symvers" "$work/src/vmlinux.symvers"

make_args=(
    ARCH=arm64 LLVM=1 LLVM_IAS=1
    CROSS_COMPILE=aarch64-linux-gnu- CROSS_COMPILE_COMPAT=arm-linux-gnueabi-
    PLATFORM_VERSION=12 ANDROID_MAJOR_VERSION=s TARGET_SOC=s5e8825
    TARGET_BUILD_VARIANT=user
    KBUILD_BUILD_USER=oss-phone KBUILD_BUILD_HOST=oss-phone-v1
    "KBUILD_BUILD_TIMESTAMP=Sun Oct 25 00:00:00 UTC 2026" KBUILD_BUILD_VERSION=1
)
bwrap --ro-bind /usr /usr --ro-bind /etc /etc \
    --symlink usr/lib /lib --symlink usr/lib /lib64 --symlink usr/bin /bin --symlink usr/bin /sbin \
    --proc /proc --dev /dev --tmpfs /tmp \
    --ro-bind "$CLANG" "$SAMSUNG_CLANG_PATH" \
    --bind "$work/src" "$SAMSUNG_SRC_PATH" --chdir "$SAMSUNG_SRC_PATH" \
    --unshare-net --clearenv \
    --setenv PATH "$SAMSUNG_CLANG_PATH/bin:/usr/bin" --setenv HOME /tmp --setenv LC_ALL C --setenv TZ UTC \
    make "${make_args[@]}" -j"$(nproc)" "$ko_path" >"$out/build.log" 2>&1 \
    || { tail -30 "$out/build.log" >&2; exit 1; }

cp "$work/src/$ko_path" "$out/$mod"
"$CLANG/bin/llvm-strip" --strip-debug "$out/$mod"
# All 306 vendor modules with the patched one substituted (later dirs win).
mkdir -p "$out/check"
cp "$out/$mod" "$out/check/"
python3 -I "$KERNEL_SCRIPTS/check-kmi.py" "$kout" "$KROOT/stock/vendor_ramdisk01/lib/modules" \
    "$KROOT/stock/vendor_dlkm" "$out/check" | tee "$out/kmi-report.txt"
rm -rf "$out/check"

# Code comparison with Samsung's prebuilt, per function. ThinLTO renames
# promoted locals (.llvm.<hash>) and the layout differs, so compare
# disassembly with those names, addresses and branch targets normalised.
python3 -I - "$CLANG/bin/llvm-objdump" "$stock_ko" "$out/$mod" "$out/function-diff.txt" <<'PY'
import re, subprocess, sys
objdump, prebuilt, patched, report = sys.argv[1:5]
head = re.compile(r"^[0-9a-f]+ <(.+)>:$")
def funcs(path):
    text = subprocess.run([objdump, "-d", "-r", "--no-show-raw-insn", path],
                          capture_output=True, text=True, check=True).stdout
    out, cur = {}, None
    for line in text.splitlines():
        m = head.match(line)
        if m:
            cur = re.sub(r"\.llvm\.[0-9]+", "", m.group(1))
            out[cur] = []
        elif cur is not None and "\t" in line:
            ins = line.split(":", 1)[1].strip()
            ins = re.sub(r"\.llvm\.[0-9]+", "", ins)
            ins = re.sub(r"<[^>]*>", "", ins)
            ins = re.sub(r"0x[0-9a-f]+|#-?[0-9]+|\b[0-9a-f]{6,}\b", "N", ins)
            out[cur].append(ins)
    return out
a, b = funcs(prebuilt), funcs(patched)
diff = sorted(n for n in a.keys() & b.keys() if a[n] != b[n])
with open(report, "w") as f:
    f.write(f"functions: prebuilt {len(a)}, patched {len(b)}, common {len(a.keys() & b.keys())}; code differs: {len(diff)}\n")
    for n in diff:
        f.write(f"  differs: {n}\n")
    for n in sorted(a.keys() - b.keys()):
        f.write(f"  only in prebuilt: {n}\n")
    for n in sorted(b.keys() - a.keys()):
        f.write(f"  only in patched: {n}\n")
print(open(report).read(), end="")
PY
(cd "$out" && sha256sum "$mod" >SHA256SUMS && cat SHA256SUMS)
rm -rf "$work"
