#!/usr/bin/env python3
"""Offline module-compatibility check of a built kernel against prebuilt modules.

    check-kmi.py BUILD_OUT MODULE_DIR [MODULE_DIR ...] [--json REPORT]

BUILD_OUT is a build-kernel.sh output directory (Module.symvers, .config,
kernelrelease, modules/). Each prebuilt module is checked the way the kernel's
module loader would check it (kernel/module.c, 5.10):

  * vermagic: with CONFIG_MODVERSIONS only the part after the release string
    must match ("SMP preempt mod_unload modversions aarch64"); the release is
    compared too and reported.
  * every undefined symbol resolves to a vmlinux export of the new kernel or to
    an export of another prebuilt module;
  * every __versions CRC for a vmlinux symbol (including module_layout) equals
    the new kernel's Module.symvers CRC;
  * exports in a symbol namespace are imported via modinfo import_ns;
  * signatures: reports CONFIG_MODULE_SIG* and whether modules carry one.
"""
import json
import struct
import sys
from pathlib import Path

SIG_MAGIC = b"~Module signature appended~\n"


def elf_sections(data):
    if data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        raise ValueError("not a little-endian ELF64 file")
    e_shoff, = struct.unpack_from("<Q", data, 0x28)
    e_shentsize, e_shnum, e_shstrndx = struct.unpack_from("<HHH", data, 0x3A)
    raw = []
    for i in range(e_shnum):
        raw.append(struct.unpack_from("<IIQQQQIIQQ", data, e_shoff + i * e_shentsize))
    names_off = raw[e_shstrndx][4]
    out = []
    for sh in raw:
        name_end = data.index(b"\0", names_off + sh[0])
        out.append({
            "name": data[names_off + sh[0]:name_end].decode(),
            "type": sh[1], "offset": sh[4], "size": sh[5], "link": sh[6], "entsize": sh[9],
        })
    return out


def parse_module(path):
    data = path.read_bytes()
    secs = elf_sections(data)
    by_name = {s["name"]: s for s in secs}
    info = {}
    if ".modinfo" in by_name:
        s = by_name[".modinfo"]
        for item in data[s["offset"]:s["offset"] + s["size"]].split(b"\0"):
            if b"=" in item:
                k, v = item.decode(errors="replace").split("=", 1)
                info.setdefault(k, []).append(v)
    versions = {}
    if "__versions" in by_name:
        s = by_name["__versions"]
        for off in range(s["offset"], s["offset"] + s["size"], 64):
            crc, = struct.unpack_from("<Q", data, off)
            name = data[off + 8:off + 64].split(b"\0", 1)[0].decode()
            versions[name] = crc & 0xFFFFFFFF
    undefined, defined_ksym = set(), set()
    symtab = next(s for s in secs if s["type"] == 2)  # SHT_SYMTAB
    strtab = secs[symtab["link"]]
    for off in range(symtab["offset"], symtab["offset"] + symtab["size"], 24):
        st_name, st_info, _, st_shndx = struct.unpack_from("<IBBH", data, off)
        if not st_name:
            continue
        start = strtab["offset"] + st_name
        name = data[start:data.index(b"\0", start)].decode()
        bind = st_info >> 4
        if st_shndx == 0 and bind in (1, 2):  # UND, GLOBAL/WEAK
            undefined.add(name)
        if name.startswith("__ksymtab_") and st_shndx != 0:
            defined_ksym.add(name[len("__ksymtab_"):])
    return {
        "path": str(path), "info": info, "versions": versions,
        "undefined": undefined, "exports": defined_ksym,
        "signed": data.endswith(SIG_MAGIC),
    }


def load_symvers(path):
    symvers = {}
    for line in Path(path).read_text().splitlines():
        parts = line.split("\t")
        if len(parts) >= 4:
            crc, sym, mod, kind = parts[:4]
            ns = parts[4] if len(parts) > 4 else ""
            symvers[sym] = {"crc": int(crc, 16), "module": mod, "kind": kind, "ns": ns}
    return symvers


def main(argv):
    report_path = None
    if "--json" in argv:
        i = argv.index("--json")
        report_path = argv[i + 1]
        del argv[i:i + 2]
    if len(argv) < 2:
        print(__doc__)
        return 2
    build = Path(argv[0])
    symvers = load_symvers(build / "Module.symvers")
    vmlinux = {k: v for k, v in symvers.items() if v["module"] == "vmlinux"}
    release = (build / "kernelrelease").read_text().strip()
    config = (build / ".config").read_text()
    new_magic_tail = "SMP preempt mod_unload modversions aarch64"
    in_tree = sorted((build / "modules").glob("*.ko"))
    if in_tree:
        new_magic = parse_module(in_tree[0])["info"]["vermagic"][0].strip()
        new_magic_tail = new_magic.split(" ", 1)[1]
    else:
        new_magic = f"{release} {new_magic_tail}"

    modules = {}
    for d in argv[1:]:
        for ko in sorted(Path(d).rglob("*.ko")):
            m = parse_module(ko)
            name = m["info"].get("name", [ko.stem])[0]
            modules[name] = m
    vendor_exports = {}
    for name, m in modules.items():
        for sym in m["exports"]:
            vendor_exports[sym] = name

    problems = []
    stats = {"modules": len(modules), "vmlinux_imports": 0, "crc_checked": 0,
             "vendor_imports": 0, "signed_modules": 0, "release_mismatch": 0}
    used_vmlinux = set()
    for name, m in sorted(modules.items()):
        magic = m["info"].get("vermagic", [""])[0].strip()
        rel, _, tail = magic.partition(" ")
        if tail != new_magic_tail:
            problems.append(f"{name}: vermagic tail '{tail}' != '{new_magic_tail}'")
        if rel != release:
            stats["release_mismatch"] += 1
        if m["signed"]:
            stats["signed_modules"] += 1
        imports_ns = set(m["info"].get("import_ns", []))
        for sym in sorted(m["undefined"]):
            if sym in vmlinux:
                stats["vmlinux_imports"] += 1
                used_vmlinux.add(sym)
                exp = vmlinux[sym]
                if sym not in m["versions"]:
                    problems.append(f"{name}: {sym} has no __versions entry")
                elif m["versions"][sym] != exp["crc"]:
                    problems.append(f"{name}: CRC mismatch {sym} module=0x{m['versions'][sym]:08x} "
                                    f"kernel=0x{exp['crc']:08x}")
                else:
                    stats["crc_checked"] += 1
                if exp["ns"] and exp["ns"] not in imports_ns:
                    problems.append(f"{name}: {sym} is in namespace {exp['ns']} not imported")
                if exp["kind"].endswith("_GPL") and m["info"].get("license", [""])[0] not in (
                        "GPL", "GPL v2", "GPL and additional rights", "Dual BSD/GPL", "Dual MIT/GPL", "Dual MPL/GPL"):
                    problems.append(f"{name}: GPL-only {sym} used by license {m['info'].get('license')}")
            elif sym in vendor_exports:
                stats["vendor_imports"] += 1
            else:
                problems.append(f"{name}: unresolved symbol {sym}")
        lay = m["versions"].get("module_layout")
        if lay is None:
            problems.append(f"{name}: no module_layout version")
        elif "module_layout" in vmlinux and lay != vmlinux["module_layout"]["crc"]:
            problems.append(f"{name}: module_layout CRC 0x{lay:08x} != 0x{vmlinux['module_layout']['crc']:08x}")
        elif "module_layout" in vmlinux:
            stats["crc_checked"] += 1

    sig = {k: ("y" if f"\n{k}=y" in config else "n")
           for k in ("CONFIG_MODULE_SIG", "CONFIG_MODULE_SIG_FORCE", "CONFIG_MODULE_SIG_PROTECT")}
    summary = {
        "build": str(build), "kernelrelease": release, "vermagic": new_magic,
        "vmlinux_exports": len(vmlinux), "distinct_vmlinux_symbols_used": len(used_vmlinux),
        "module_layout_crc": f"0x{vmlinux['module_layout']['crc']:08x}" if "module_layout" in vmlinux else None,
        "signature_config": sig, **stats, "problems": problems,
    }
    for k, v in summary.items():
        if k != "problems":
            print(f"{k}: {v}")
    print(f"problems: {len(problems)}")
    for p in problems[:200]:
        print("  " + p)
    if report_path:
        Path(report_path).write_text(json.dumps(summary, indent=2) + "\n")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
