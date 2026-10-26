# OSS Phone kernel: reproducible build of the A266MUBU1AYB8 kernel

Builds the SM-A266M kernel (`5.10.223-android12-9-30597860-abA266MUBU1AYB8`,
Samsung GKI, Exynos 1280 / s5e8825) from source with the exact stock toolchain
and config, proves offline that the 306 prebuilt vendor modules still fit, and
drops the result into the current BOOT image. Nothing here flashes anything.
The kernel tree (≈2 GB) and toolchain stay on the build host (`whale`,
`$KROOT`, default `~/oss-kernel`); this directory only holds scripts and
configs.

```sh
# on whale, from a checkout of this repo
scripts/kernel/build-all.sh          # Samsung's zip from $KROOT/dl/, else the mirror
scripts/kernel/build-all.sh /path/to/SM-A266M_LA_15_Opensource.zip
```

About 12 minutes per kernel on 12 cores. Outputs: `$KROOT/out/<variant>-<source>/`
(Image, vmlinux, System.map, Module.symvers, .config, reports) and
`$KROOT/out/boot/` (unflashed BOOT images, `SHA256SUMS`).

## Inputs, all pinned (`env.sh`)

| Input | Pin | Why it is the right one |
| --- | --- | --- |
| Toolchain | AOSP `prebuilts/clang/host/linux-x86` commit `1006a161`, dir `clang-r416183`; `bin/clang-12` sha256 `d505bf52…71ca` | Prints exactly the stock `/proc/version` compiler: `Android (7211189, based on r416183) clang version 12.0.4 (… c935d99d…)`. The `clang-r416183b` that Samsung's `build_kernel.sh` names is 12.0.5 (7284624), *not* what built the shipped kernel. |
| Config | `configs/A266MUBU1AYB8.config` = the phone's `/proc/config.gz` | Byte-identical to the IKCONFIG extracted from the stock `boot.img` kernel. |
| Source | Samsung's `SM-A266M_LA_15_Opensource.zip`, opensource.samsung.com upload 13000 (attach 38622): 278 700 980 bytes, sha256 `6b993454a1ddd97fd36a94b0ee61bbe97165db5c22674f82010aece86210453f`; its `Kernel.tar.gz` sha256 `f9ca20b4…d357`; extracted-tree content hash `7ce9859e…0f38` | Samsung's release for exactly A266MUBU1AYB8, downloaded 2026-10-26 after the user accepted the terms. `fetch-source.sh samsung` checks all three hashes. Needs a person to download (below); kept at `$KROOT/dl/` on whale. |
| Source (fallback) | `github.com/physwizz/A266m-V-LA` commit `d569a477`, tree `d779b6c1` | Public GPL re-upload of the same release. Verified: all 81 582 files byte-identical to Samsung's `Kernel.tar.gz` except the saved defconfig (`LOCALVERSION="-physwizz"`, `LOCALVERSION_AUTO` off), which the build never uses; it builds bit-identical kernels. Used only where the zip is absent. |
| Build environment | in-tree build in a `bwrap` sandbox with the tree at Samsung's path `/home/dpi/qb5_8814/workspace/P4_1716/android/kernel/s5e8825` and the toolchain at `/home/dpi/qb5_8814/workspace/P4_1716/android/prebuilts/clang/host/linux-x86/clang-r416183`, `TARGET_BUILD_VARIANT=user`, `LLVM=1 LLVM_IAS=1`, banner `dpi@21DND505`, `Fri Feb 21 20:40:50 KST 2025`, `.scmversion` | The source path is baked into the stock config (`CONFIG_UNUSED_KSYMS_WHITELIST`). The toolchain path ends up in debug info (compiler `stdarg.h`), hence in ThinLTO hashes and build IDs; it was found by rebuilding `kperfmon.ko` until it matched Samsung's prebuilt byte for byte, build ID included, and makes the output independent of `$KROOT`. `TARGET_BUILD_VARIANT=user` selects production DEFEX/MZ/NGKSM code (without it DEFEX builds permissive). |

### Getting Samsung's official source (needs a person, ~2 minutes)

The download is behind a Cloudflare check and a terms click-through, which
this project does not automate.

1. Open <https://opensource.samsung.com/uploadSearch?searchValue=SM-A266M>.
2. In the row **SM-A266M · A266MUBU1AYB8**, click the download icon in the
   *Source* column.
3. Tick only **SM-A266M_LA_15_Opensource.zip** (265.79 MB) and click
   **Download selected file(s)**.
4. Read the legal notice; optionally pick a purpose (e.g. *Study the source
   code*) and click **Agree**.
5. Put the zip at `$KROOT/dl/` on whale (it is there now) and run
   `scripts/kernel/build-all.sh`. `fetch-source.sh samsung` verifies the zip,
   `Kernel.tar.gz` and extracted-tree hashes, extracts to `$KROOT/src/samsung`
   and lists every file that differs from the mirror.

The zip also holds `Platform.tar.gz` (Samsung's userspace GPL parts) and
`README_Kernel.txt`, whose instructions name `clang-r416183b` and
`s5e8825-a26xsub_defconfig`; neither is what built the shipped kernel (see
the toolchain and config rows). The extracted tree is read-only (444/555);
only write bits differ from the mirror, and the build copies the tree first.

The same page offers Samsung's own `toolchain.tar.gz` under *Others →
Galaxy-A26-5g (SM-A266M)*; it is not needed (the AOSP clang above is the one
named in the banner), but downloading it would settle the toolchain question
beyond doubt.

## What the checks prove (official source, 2026-10-26; the mirror gives identical results)

| Check | Script | Stock reproduction | OSS variant |
| --- | --- | --- | --- |
| All 7173 vmlinux exports: same names, CRCs, GPL-ness, namespaces as the stock Image | `stock-exports.py` | identical | identical |
| 306 vendor modules (304 vendor_boot + 2 vendor_dlkm): every import resolves, 13 508 vmlinux imports + 306 `module_layout` CRCs match, vermagic matches | `check-kmi.py` | 0 problems | 0 problems |
| Module signatures | `check-kmi.py` | `CONFIG_MODULE_SIG` off, no module signed, no `sig_enforce` on the phone | same |
| Functions (unique names, layout immediates masked) | `compare-functions.py` | 55 203 / 55 207 identical (99.99 %; the 4 others are section markers) | 99.79 % (DEFEX gone, PID_NS/devtmpfs added) |
| Rebuilt in-tree modules vs Samsung's prebuilt `.ko` (after `--strip-debug`) | `compare-builds.py` | 274 / 304 byte-identical; the other 30 have identical code and differ in ThinLTO `.llvm.<hash>` promotion names | the same 274 still byte-identical with PID_NS/devtmpfs (and with USER_NS): the header-tested options change no module code |
| Struct layouts (pahole over vmlinux DWARF) | manual, see below | — | no shared struct changed |
| Whole Image | `compare-builds.py` | same size (34 044 416), same banner, same embedded config, 28 % of bytes differ (function order, see below) | — |
| BOOT pipeline | `build-all.sh` self-test | the stock kernel through `make-boot-image.sh` reproduces today's flashed BOOT image byte for byte (`3bf29162…`) | — |

Why the Image is not byte-identical although every function is: ThinLTO
names promoted symbols `.llvm.<(h0<<32)|h1 of the object's bitcode hash>`,
and only 96 of 609 vmlinux objects (680 of all objects, counting modules) hash
as in Samsung's build. That changes symbol order and so every address. What
is known about the cause:

- It is not in the published source: Samsung's official tarball is
  file-for-file the mirror, and builds to the identical Image.
- The official tree confirms Samsung's *build* tree had more than it ships:
  no Kconfig defines `CONFIG_SEC_AUTO_INPUT` (only the saved defconfig names
  it) and the empty "Block kunit" / "File systems kunit" menus in the stock
  config come from Kconfig `menu`s the release lacks. These are all
  `# … is not set`/`=m` for an absent driver, so they change no compiled code;
  the reproduction embeds the phone's config text verbatim anyway.
- Not line numbers: all 11 387 `__bug_table` file:line entries of vmlinux (and
  those of the non-identical modules) equal Samsung's.
- Not the environment: kperfmon.ko and 273 other modules (54 of them
  multi-object) match Samsung's byte for byte *including the build ID*, which
  lld computes over the full debug info. Three non-identical modules
  (mali_kbase, ufs-exynos-core, usb_notify_layer) even have Samsung's
  `srcversion`, which modpost computes from the sources with comments and
  whitespace removed.
- Not a few headers: 83 mismatching objects (e.g. `drivers/base/core.o`) use
  only headers that byte-identical objects also use.

So Samsung's build tree differs from its release in non-semantic ways spread
through many files (most consistent with comment/whitespace edits that move
columns or declaration lines in debug info but no code, BUG lines or
srcversion). The published source cannot reproduce the Image byte for byte;
it reproduces every function, export, CRC and 274 modules exactly.

### Reproducibility and results

Two independent runs (fresh toolchain download, fresh clone, different
`$KROOT`) give bit-identical outputs, and the official zip and the mirror give
the same outputs. Results of `build-all.sh`:

| Variant | Image sha256 | BOOT image sha256 |
| --- | --- | --- |
| stock | `72bb8f835539bcc405ae0e9a8f3303098bfe9974f04edfd2e98c9f28b439ee63` | `28f315051de91b205344bf184f3c401f69c445097a1397194843a9b9ed9b30f1` |
| oss | `6a3dd36a32df426d44b171201866d1c277756fb6b10e7b253ce49386496db4d5` | `8c0461eef2ca07b3d35199149907b40ff5db07fc10b4b92567ef1e3443b15f73` |
| oss-userns | `92f02d17b32d3ca8c36c7f8d64802fe7550e33a87b96f291b8971fb94b5bfd56` | `061b69234f41e1d5544b906ee72884ae56f907744a433fe4041c2b948348e6ac` |

(Image hashes are before Magisk's hexpatches; BOOT images are built on
`boot-moon-direct-A266MUBU1AYB8.img`, `3bf29162…`.)

## The OSS variant (`configs/oss-phone.fragment`)

Rule: an option may change only if no header a module can include tests it
(`check-fragment.sh`), or it carries a written `# kmi-reviewed:` justification;
and the built result must keep every export and CRC (`stock-exports.py`,
`check-kmi.py`).

| Change | Value for OSS Phone | KMI evidence |
| --- | --- | --- |
| `# CONFIG_SECURITY_DEFEX is not set` | Removes Samsung's exec/open/write hooks that deny or kill "unauthorised" root processes (our chroot runs as root); stock Magisk only byte-patches one DEFEX check | header-free, no exports |
| `CONFIG_LOG_BUF_SHIFT=20` | 1 MiB dmesg instead of 256 KiB, so early boot logs survive until the direct-boot handoff | header-free |
| `CONFIG_KFENCE_SAMPLE_INTERVAL=0` | KFENCE stays built in but idle: with static keys its 500 ms timer patches kernel text on all CPUs twice a second; re-enable with `kfence.sample_interval=500` | header-free |
| `CONFIG_PID_NS=y` | Stage 2: our init as PID 1 with Android's init contained in its own PID namespace | `pid_namespace.h` inlines only; no vendor module uses them; no struct change |
| `CONFIG_DEVTMPFS=y` (not auto-mounted) | Stage 2: kernel-maintained `/dev` that our init can mount | `device.h` prototypes used only by built-in drivers/base |

`configs/oss-userns.fragment` (variant `oss-userns`) adds `CONFIG_USER_NS=y`
for rootless containers. Prebuilt modules were compiled with
`current_user_ns() == &init_user_ns` and identity uid mapping; none imports a
user-namespace API, so they behave as before for everything in the initial
user namespace. Any process can then create user namespaces (5.10 has no LSM
hook for it): cap with `sysctl user.max_user_namespaces=0` until needed. A
privileged (root) LXC-style Android container needs PID_NS, not USER_NS.

Samsung hardening checked against these: KDP_NS protects only mount-namespace
`vfsmount`s (which Android already creates); KDP_CRED copies creds into
protected memory in `commit_creds()` and already refcounts `cred->user_ns`
(`drivers/uh/kdp.c`), so `create_user_ns()` on a prepared cred is supported;
RKP/uH are unchanged (same config).

Not changed, because they would break the frozen KMI: anything that changes
`task_struct`/`struct module` (SCHEDSTATS, SCHED_DEBUG, FTRACE, SYSVIPC,
POSIX_MQUEUE), DEBUG_LIST (exports `__list_add_valid`), HZ, PAGE_OWNER (header
inlines), FIVE/PROCA (task integrity fields, headers).

## BOOT images (`make-boot-image.sh`)

The built Image replaces only the kernel of the current direct-boot BOOT image
(Magisk v30.7 + `scripts/direct-boot` overlay, sha256 `3bf29162…`). Magisk's
Samsung kernel hexpatches are re-applied as in the base (PROCA always, DEFEX
when present), the AVB hash footer is re-added with the project key and salt,
and the script verifies ramdisk and header are byte-identical to the base.
If the phone's BOOT has changed since, pass the new image as the third
argument (or `BASE_BOOT_IMG=` to `build-all.sh`).
