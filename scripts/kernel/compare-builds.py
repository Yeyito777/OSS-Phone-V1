#!/usr/bin/env python3
"""How close is a rebuilt kernel to the stock one?

    compare-builds.py STOCK_IMAGE BUILD_OUT STOCK_MODULE_DIR [--kallsyms FILE] [--other BUILD_OUT]
                      [--strip LLVM_STRIP]

Reports: Image size/hash/byte differences, banner, embedded config (ikconfig),
vmlinux symbol names vs the phone's /proc/kallsyms (names only; kptr_restrict
hides addresses), and every rebuilt in-tree module against the prebuilt module
of the same name (byte-identical? same srcversion, i.e. same source MD5?).
Prebuilt modules ship debug-stripped, so with --strip the rebuilt ones are
compared after `llvm-strip --strip-debug`, exactly as the Android build does.
With --other, also diffs the vmlinux exports (Module.symvers) of two builds.
"""
import gzip
import hashlib
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
check_kmi = __import__("check-kmi")


def ikconfig(blob):
    start = blob.find(b"IKCFG_ST")
    end = blob.find(b"IKCFG_ED")
    if start < 0 or end < 0:
        return None
    return gzip.decompress(blob[start + 8:end])


def banner(blob):
    m = re.search(rb"Linux version [^\n\0]+", blob)
    return m.group(0).decode(errors="replace") if m else None


def main(argv):
    opts = {}
    for flag in ("--kallsyms", "--other", "--strip"):
        if flag in argv:
            i = argv.index(flag)
            opts[flag] = argv[i + 1]
            del argv[i:i + 2]
    stock_image, build, stock_mods = Path(argv[0]), Path(argv[1]), Path(argv[2])
    a = stock_image.read_bytes()
    b = (build / "Image").read_bytes()
    print(f"stock Image: {len(a)} bytes sha256 {hashlib.sha256(a).hexdigest()}")
    print(f"built Image: {len(b)} bytes sha256 {hashlib.sha256(b).hexdigest()}")
    n = min(len(a), len(b))
    same = sum(1 for i in range(0, n, 4096) if a[i:i + 4096] == b[i:i + 4096])
    diff_bytes = sum(1 for x, y in zip(a, b) if x != y) + abs(len(a) - len(b))
    print(f"identical: {a == b}; differing bytes: {diff_bytes} "
          f"({100.0 * diff_bytes / max(len(a), len(b)):.3f}%); identical 4 KiB pages: {same}/{(n + 4095) // 4096}")
    first = next((i for i in range(n) if a[i] != b[i]), None)
    print(f"first difference at offset: {hex(first) if first is not None else 'none'}")
    print(f"stock banner: {banner(a)}")
    print(f"built banner: {banner(b)}")
    print(f"banner identical: {banner(a) == banner(b)}")
    ca, cb = ikconfig(a), ikconfig(b)
    print(f"embedded config identical: {ca == cb}")
    if ca != cb and ca and cb:
        la, lb = set(ca.decode().splitlines()), set(cb.decode().splitlines())
        for line in sorted(la - lb)[:20]:
            print(f"  only stock: {line}")
        for line in sorted(lb - la)[:20]:
            print(f"  only built: {line}")

    if "--kallsyms" in opts:
        dev = []
        for line in Path(opts["--kallsyms"]).read_text().splitlines():
            parts = line.split()
            if len(parts) == 3:  # vmlinux symbols only; module symbols carry [mod]
                dev.append((parts[1], parts[2]))
        built = []
        for line in (build / "System.map").read_text().splitlines():
            parts = line.split()
            if len(parts) == 3:
                built.append((parts[1], parts[2]))
        sd, sb = set(n for _, n in dev), set(n for _, n in built)
        print(f"kallsyms: phone {len(dev)} vmlinux symbols, build System.map {len(built)}; "
              f"names only on phone {len(sd - sb)}, only in build {len(sb - sd)}")
        for s in sorted(sd - sb)[:15]:
            print(f"  only on phone: {s}")
        for s in sorted(sb - sd)[:15]:
            print(f"  only in build: {s}")
        dev_names = [n for _, n in dev]
        built_names = [n for _, n in built if n in sd]
        common = [n for n in dev_names if n in sb]
        print(f"kallsyms order of common names identical: {common == built_names}")

    stock = {}
    for ko in stock_mods.rglob("*.ko"):
        stock[ko.name] = ko
    exact = same_src = other_src = unknown = 0
    differ_names = []
    rebuilt = sorted((build / "modules").glob("*.ko"))
    tmp = Path(tempfile.mkdtemp())
    try:
        for ko in rebuilt:
            if ko.name not in stock:
                continue
            if "--strip" in opts:
                shutil.copy(ko, tmp / ko.name)
                subprocess.run([opts["--strip"], "--strip-debug", str(tmp / ko.name)], check=True)
                ko = tmp / ko.name
            if stock[ko.name].read_bytes() == ko.read_bytes():
                exact += 1
                continue
            va = check_kmi.parse_module(stock[ko.name])["info"].get("srcversion", [None])[0]
            vb = check_kmi.parse_module(ko)["info"].get("srcversion", [None])[0]
            if va is None or vb is None:
                unknown += 1
                differ_names.append(ko.name)
            elif va == vb:
                same_src += 1
                differ_names.append(ko.name + " (same srcversion)")
            else:
                other_src += 1
                differ_names.append(ko.name + " (srcversion differs)")
    finally:
        shutil.rmtree(tmp)
    matched = exact + same_src + other_src + unknown
    print(f"modules: {len(stock)} prebuilt, {len(rebuilt)} rebuilt, {matched} matched by name: "
          f"{exact} byte-identical; of the rest {same_src} have the same srcversion, "
          f"{other_src} a different srcversion, {unknown} carry none")
    for name in differ_names[:60]:
        print(f"  not identical: {name}")
    missing = sorted(set(stock) - {k.name for k in rebuilt})
    print(f"prebuilt modules with no rebuilt counterpart: {len(missing)}")
    for name in missing[:40]:
        print(f"  not built from this tree: {name}")

    if "--other" in opts:
        x = check_kmi.load_symvers(build / "Module.symvers")
        y = check_kmi.load_symvers(Path(opts["--other"]) / "Module.symvers")
        xv = {k: v for k, v in x.items() if v["module"] == "vmlinux"}
        yv = {k: v for k, v in y.items() if v["module"] == "vmlinux"}
        changed = sorted(k for k in xv.keys() & yv.keys() if xv[k]["crc"] != yv[k]["crc"])
        print(f"vmlinux exports vs {opts['--other']}: {len(xv)} vs {len(yv)}; "
              f"only here {len(xv.keys() - yv.keys())}, only there {len(yv.keys() - xv.keys())}, "
              f"CRC changed {len(changed)}")
        for k in sorted(xv.keys() ^ yv.keys())[:20]:
            print(f"  export set differs: {k}")
        for k in changed[:20]:
            print(f"  CRC changed: {k}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
