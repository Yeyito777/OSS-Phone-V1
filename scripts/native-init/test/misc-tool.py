#!/usr/bin/env python3
"""Make and check 1 MiB misc images for the bootcount bench.

  misc-tool.py make IMG [--seed S:SEQ:COUNT] [--damaged S] [--foreign S] [--torn S:SEQ:COUNT] [--plain]
  misc-tool.py check IMG REF      # slot summary; every byte outside 0x40000-0x41fff == REF
  misc-tool.py show IMG
S is A or B. Unless --plain, the image mimics the live map: non-zero BCB
(block 0), 0x8000 (AOSP system area) and 0x7f000 (Samsung metadata), so the
check proves those are never touched."""
import struct, sys, zlib

SIZE, A, B, BLK = 1 << 20, 0x40000, 0x41000, 4096
OFF = {"A": A, "B": B}

def rec(seq, count):
    head = b"OSSBOOTC" + struct.pack("<HHIIII", 1, 32, seq, count, 3, 0)
    return head + struct.pack("<I", zlib.crc32(head))

def classify(b):
    if not any(b): return "ZERO"
    if b[:8] != b"OSSBOOTC" or any(b[32:]): return "FOREIGN"
    ver, size = struct.unpack_from("<HH", b, 8)
    if ver != 1 or size != 32 or zlib.crc32(b[:0x1c]) != struct.unpack_from("<I", b, 0x1c)[0]:
        return "DAMAGED"
    seq, count = struct.unpack_from("<II", b, 0x0c)
    return f"VALID(seq={seq},count={count})"

def make(path, args):
    img = bytearray(SIZE)
    if "--plain" not in args:
        img[0:64] = b"BCB-must-never-be-touched-by-the-bootcount-writer".ljust(64, b"!")
        img[0x8000:0x8040] = b"AOSP-system-area".ljust(64, b"#")
        img[0x7f000:0x7f340] = (b"SAMSUNG-META" * 70)[:0x340]
    i = 0
    while i < len(args):
        a = args[i]
        if a == "--seed":
            s, seq, cnt = args[i + 1].split(":"); o = OFF[s]
            img[o:o + 32] = rec(int(seq), int(cnt)); i += 2
        elif a == "--damaged":
            o = OFF[args[i + 1]]; r = bytearray(rec(5, 1)); r[0x1c] ^= 0xff
            img[o:o + 32] = r; i += 2
        elif a == "--foreign":
            o = OFF[args[i + 1]]; img[o:o + 16] = b"SOMEONE-ELSE-USE"; i += 2
        elif a == "--torn":   # first 16 bytes of a new record landed, the rest did not
            s, seq, cnt = args[i + 1].split(":"); o = OFF[s]
            img[o:o + 16] = rec(int(seq), int(cnt))[:16]; i += 2
        else:
            i += 1
    open(path, "wb").write(img)

def show(img):
    return f"slotA={classify(img[A:A + BLK])} slotB={classify(img[B:B + BLK])}"

def main():
    cmd, path = sys.argv[1], sys.argv[2]
    if cmd == "make":
        make(path, sys.argv[3:]); return
    img = open(path, "rb").read()
    assert len(img) == SIZE, f"size {len(img)}"
    if cmd == "show":
        print(show(img)); return
    ref = open(sys.argv[3], "rb").read()
    outside = img[:A] == ref[:A] and img[B + BLK:] == ref[B + BLK:]
    print(show(img), "outside=UNCHANGED" if outside else "outside=CHANGED!!",
          "image=IDENTICAL" if img == ref else "image=modified")
    sys.exit(0 if outside else 1)

main()
