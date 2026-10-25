# OSS Phone kernel: reproducible build of the A266MUBU1AYB8 kernel

Builds the SM-A266M kernel (`5.10.223-android12-9-30597860-abA266MUBU1AYB8`,
Samsung GKI, Exynos 1280 / s5e8825) from source with the exact stock toolchain
and config, proves offline that the 306 prebuilt vendor modules still fit, and
drops the result into the current BOOT image. Nothing here flashes anything.
The kernel tree (≈2 GB) and toolchain stay on the build host (`whale`,
`$KROOT`, default `~/oss-kernel`); this directory only holds scripts and
configs.

```sh
# on whale, from a checkout of this branch
scripts/kernel/build-all.sh                       # mirror source (tonight)
scripts/kernel/build-all.sh ~/Downloads/SM-A266M_LA_15_Opensource.zip   # official source
```

About 12 minutes per kernel on 12 cores. Outputs: `$KROOT/out/<variant>-<source>/`
(Image, vmlinux, System.map, Module.symvers, .config, reports) and
`$KROOT/out/boot/` (unflashed BOOT images, `SHA256SUMS`).

## Inputs, all pinned (`env.sh`)

| Input | Pin | Why it is the right one |
| --- | --- | --- |
| Toolchain | AOSP `prebuilts/clang/host/linux-x86` commit `1006a161`, dir `clang-r416183`; `bin/clang-12` sha256 `d505bf52…71ca` | Prints exactly the stock `/proc/version` compiler: `Android (7211189, based on r416183) clang version 12.0.4 (… c935d99d…)`. The `clang-r416183b` that Samsung's `build_kernel.sh` names is 12.0.5 (7284624), *not* what built the shipped kernel. |
| Config | `configs/A266MUBU1AYB8.config` = the phone's `/proc/config.gz` | Byte-identical to the IKCONFIG extracted from the stock `boot.img` kernel. |
| Source (official) | `SM-A266M_LA_15_Opensource.zip`, opensource.samsung.com upload 13000, 278 700 980 bytes | Samsung's release for exactly A266MUBU1AYB8. Needs a human to accept the download terms (below). |
| Source (tonight) | `github.com/physwizz/A266m-V-LA` commit `d569a477`, tree `d779b6c1` | Public GPL re-upload of that release. Its saved defconfig differs from the phone's config only in `LOCALVERSION="-physwizz"` and the whitelist path; we use our own config. |
| Build environment | in-tree build in a `bwrap` sandbox with the tree at Samsung's path `/home/dpi/qb5_8814/workspace/P4_1716/android/kernel/s5e8825`, `TARGET_BUILD_VARIANT=user`, `LLVM=1 LLVM_IAS=1`, banner `dpi@21DND505`, `Fri Feb 21 20:40:50 KST 2025`, `.scmversion` | That path is baked into the stock config (`CONFIG_UNUSED_KSYMS_WHITELIST`). `TARGET_BUILD_VARIANT=user` selects production DEFEX/MZ/NGKSM code (without it DEFEX builds permissive). |

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
5. Copy the zip to whale and run
   `scripts/kernel/build-all.sh /path/to/SM-A266M_LA_15_Opensource.zip`.
   `fetch-source.sh samsung` checks the size, extracts `Kernel.tar.gz` to
   `$KROOT/src/samsung` and lists every file that differs from the mirror.

The same page offers Samsung's own `toolchain.tar.gz` under *Others →
Galaxy-A26-5g (SM-A266M)*; it is not needed (the AOSP clang above is the one
named in the banner), but downloading it would settle the toolchain question
beyond doubt.

## What the checks prove (results for the mirror build, 2026-10-25)

| Check | Script | Stock reproduction | OSS variant |
| --- | --- | --- | --- |
| All 7173 vmlinux exports: same names, CRCs, GPL-ness, namespaces as the stock Image | `stock-exports.py` | identical | identical |
| 306 vendor modules (304 vendor_boot + 2 vendor_dlkm): every import resolves, 13 508 vmlinux imports + 306 `module_layout` CRCs match, vermagic matches | `check-kmi.py` | 0 problems | 0 problems |
| Module signatures | `check-kmi.py` | `CONFIG_MODULE_SIG` off, no module signed, no `sig_enforce` on the phone | same |
| Functions (unique names, layout immediates masked) | `compare-functions.py` | 55 203 / 55 207 identical (99.99 %; the 4 others are section markers) | 99.79 % (DEFEX gone, PID_NS/devtmpfs added) |
| Rebuilt in-tree modules vs Samsung's prebuilt `.ko` (after `--strip-debug`) | `compare-builds.py` | 258 / 304 byte-identical; the other 46 differ only in ThinLTO `.llvm.<hash>` promotion names | — |
| Struct layouts (pahole over vmlinux DWARF) | manual, see below | — | no shared struct changed |
| Whole Image | `compare-builds.py` | same size (34 044 416), same banner, 28 % of bytes differ (layout shift) | — |
| BOOT pipeline | `build-all.sh` self-test | the stock kernel through `make-boot-image.sh` reproduces today's flashed BOOT image byte for byte (`3bf29162…`) | — |

Why the Image is not byte-identical although every function is: the published
tree is not exactly Samsung's build tree (its Kconfig lacks Samsung's internal
"Block kunit"/"File systems kunit" menus and `CONFIG_SEC_AUTO_INPUT`, so the
embedded config differs), and ThinLTO promotion names
(`.llvm.<module hash>`) differ, which reorders symbols. Rerun against the
official zip in the morning; `fetch-source.sh samsung` prints the differences.

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
