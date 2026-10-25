#!/usr/bin/env python3
"""Per-function closeness of two kernels, independent of layout shifts.

    compare-functions.py STOCK_IMAGE STOCK_NM BUILT_IMAGE BUILT_SYSTEM_MAP

Symbol sizes come from the distance to the next symbol (stock: vmlinux-to-elf
nm of the stock Image; build: System.map). For each text symbol whose
(ThinLTO-suffix-stripped) name is unique in both kernels it reports whether
the size matches and whether the bytes match once branch/adrp/ldr-literal
immediates (which encode layout) are masked out.
"""
import re
import struct
import sys
from pathlib import Path

NORM = re.compile(r"\.llvm\.\d+$")


def load(nm_path):
    rows = []
    for line in Path(nm_path).read_text().splitlines():
        p = line.split()
        if len(p) == 3 and not p[2].startswith("$") and p[1] not in "aAN":
            rows.append((int(p[0], 16), p[1], NORM.sub("", p[2])))
    rows.sort()
    base = next(a for a, _, n in rows if n == "_text")
    out, seen = {}, {}
    for (a, t, n), nxt in zip(rows, rows[1:]):
        if t not in "tT":
            continue
        size = nxt[0] - a
        seen[n] = seen.get(n, 0) + 1
        out[n] = (a - base, size)
    return {n: v for n, v in out.items() if seen[n] == 1 and v[1] > 0}


def masked(code):
    words = struct.unpack(f"<{len(code) // 4}I", code[:len(code) // 4 * 4])
    res = []
    for w in words:
        if (w & 0x7C000000) == 0x14000000:      # B / BL imm26
            w &= 0xFC000000
        elif (w & 0x9F000000) in (0x90000000, 0x10000000):  # ADRP / ADR
            w &= 0x9F00001F
        elif (w & 0xFF000010) == 0x54000000:    # B.cond imm19
            w &= 0xFF00001F
        elif (w & 0x7E000000) in (0x34000000, 0x36000000):  # CBZ/CBNZ/TBZ/TBNZ
            w &= 0xFF00001F if (w & 0x7E000000) == 0x34000000 else 0xFFF8001F
        elif (w & 0x3B000000) == 0x18000000:    # LDR literal
            w &= 0xFF00001F
        elif (w & 0x3B000000) == 0x39000000:    # LDR/STR unsigned imm (lo12 of ADRP pairs)
            w &= 0xFFC003FF
        elif (w & 0x7F800000) == 0x11000000:    # ADD imm (lo12 of ADRP pairs)
            w &= 0xFFC003FF
        res.append(w)
    return res


def main(argv):
    si, sn, bi, bm = argv
    simg, bimg = Path(si).read_bytes(), Path(bi).read_bytes()
    s, b = load(sn), load(bm)
    common = sorted(s.keys() & b.keys())
    same_size = same_code = 0
    # vmlinux-to-elf types every stock symbol t/T, so restrict to names that
    # are text in the build and unique in both.
    stock_bytes = sum(s[n][1] for n in common)
    same_code_bytes = 0
    differ = []
    for n in common:
        (so, ss), (bo, bs) = s[n], b[n]
        if ss != bs:
            differ.append((ss, n, bs))
            continue
        same_size += 1
        if masked(simg[so:so + ss]) == masked(bimg[bo:bo + bs]):
            same_code += 1
            same_code_bytes += ss
    print(f"unique text symbols: stock {len(s)}, build {len(b)}, common {len(common)}")
    print(f"same size: {same_size} ({100.0 * same_size / len(common):.2f}%)")
    print(f"same code modulo layout immediates: {same_code} ({100.0 * same_code / len(common):.2f}%), "
          f"{same_code_bytes} of {stock_bytes} bytes of those functions ({100.0 * same_code_bytes / stock_bytes:.2f}%)")
    print(f"build text symbols without a unique stock counterpart: {len(b.keys() - s.keys())}")
    print("largest size differences (stock size, name, built size):")
    for ss, n, bs in sorted(differ, key=lambda x: -abs(x[0] - x[2]))[:25]:
        print(f"  {ss:7d} {n} {bs}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
