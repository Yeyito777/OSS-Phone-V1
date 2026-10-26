#!/usr/bin/env python3
"""Recover the stock kernel's export table (name, CRC, GPL-ness, namespace).

    stock-exports.py STOCK_IMAGE STOCK_SYMBOLS [--compare Module.symvers]

STOCK_SYMBOLS is `llvm-nm -n` output for the ELF that vmlinux-to-elf rebuilds
from the stock Image's embedded kallsyms (the phone hides addresses), which
gives __start/__stop of __ksymtab{,_gpl} and __kcrctab{,_gpl}. Entries are
5.10 arm64 PREL32 `struct kernel_symbol {int value, name, namespace;}`
and u32 CRCs in the same order. With --compare, reports every export whose
presence, CRC, GPL-ness or namespace differs from a built Module.symvers.
"""
import struct
import sys
from pathlib import Path


def main(argv):
    compare = None
    if "--compare" in argv:
        i = argv.index("--compare")
        compare = argv[i + 1]
        del argv[i:i + 2]
    image = Path(argv[0]).read_bytes()
    syms = {}
    for line in Path(argv[1]).read_text().splitlines():
        parts = line.split()
        if len(parts) == 3:
            syms.setdefault(parts[2], int(parts[0], 16))
    base = syms["_text"]

    def cstr(vaddr):
        off = vaddr - base
        return image[off:image.index(b"\0", off)].decode()

    exports = {}
    for gpl in ("", "_gpl"):
        ks, ke = syms[f"__start___ksymtab{gpl}"], syms[f"__stop___ksymtab{gpl}"]
        cs = syms[f"__start___kcrctab{gpl}"]
        n = (ke - ks) // 12
        for i in range(n):
            ent = ks + 12 * i
            _, name_off, ns_off = struct.unpack_from("<iii", image, ent - base)
            name = cstr(ent + 4 + name_off)
            ns = cstr(ent + 8 + ns_off) if ns_off else ""
            crc, = struct.unpack_from("<I", image, cs + 4 * i - base)
            exports[name] = {"crc": crc, "kind": "EXPORT_SYMBOL" + gpl.upper(), "ns": ns}
    print(f"stock exports: {len(exports)}")
    if not compare:
        for name, e in sorted(exports.items()):
            print(f"0x{e['crc']:08x}\t{name}\tvmlinux\t{e['kind']}\t{e['ns']}")
        return 0
    built = {}
    for line in Path(compare).read_text().splitlines():
        p = line.split("\t")
        if len(p) >= 4 and p[2] == "vmlinux":
            built[p[1]] = {"crc": int(p[0], 16), "kind": p[3], "ns": p[4] if len(p) > 4 else ""}
    only_stock = sorted(exports.keys() - built.keys())
    only_built = sorted(built.keys() - exports.keys())
    diff = sorted(k for k in exports.keys() & built.keys() if exports[k] != built[k])
    print(f"built exports: {len(built)}; only in stock: {len(only_stock)}; only in build: {len(only_built)}; "
          f"CRC/kind/namespace differs: {len(diff)}")
    for k in only_stock[:30]:
        print(f"  only in stock: {k}")
    for k in only_built[:30]:
        print(f"  only in build: {k}")
    for k in diff[:30]:
        print(f"  differs: {k} stock={exports[k]} built={built[k]}")
    return 1 if (only_stock or only_built or diff) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
