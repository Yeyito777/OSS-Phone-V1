# oss-rebootmode-clear.ko

Selects a NORMAL boot in the Exynos PMU alive-domain reboot registers, so a
recovery-slot test image that panics or hangs comes back to BOOT instead of
looping in recovery (`notes/stage2/RECOVERY-PERSISTENCE-ROOTCAUSE.md`, branch
`stage2-native-init`). oss-init and the canary `finit_module()` it first.

```sh
# on whale; KERNEL_BUILD defaults to oss-samsung = the installed BOOT 8c0461ee
scripts/kernel/build-module.sh src/oss-rebootmode-clear
# -> $KROOT/out/modules/oss-rebootmode-clear/oss-rebootmode-clear.ko
```

Parameters: `dry_run=1` reads and logs only. Messages go to kmsg with the
prefix `oss-rebootmode-clear:` (`before:` and `after:` lines).

## What it does, and where each operation comes from

All accesses use the `samsung,syscon-phandle` regmap of the `exynos-reboot` and
`sec-reboot` DT nodes. On this phone both point to phandle 0x94,
`system-controller@11860000` (`"samsung,s5e8825-pmu", "syscon"`, reg
`0x11860000 +0x10000`, the `pmu_alive` block). The module refuses unless
both nodes give the same regmap. The regmap is the built-in syscon core's
MMIO regmap, created on first lookup and shared by every user (exynos-pmu-if,
exynos-reboot, sec_reboot), so it works before any vendor module is loaded.
`sec_set_reboot_magic()` is not used because it lives in `sec_reboot.ko`,
which first-stage init loads after oss-init.

| Register (DT property, live value) | Read | Write (only if the value differs) | Same operation in the kernel |
| --- | --- | --- | --- |
| `reboot-cmd-offset` = 0x810 (PMU SYSIP_DAT0) | yes | `0x00` REBOOT_MODE_NORMAL, **only if it holds 0x01..0x05** (a mode exynos-reboot writes); any other value is left alone and logged | `exynos_reboot_parse()`: `regmap_write(pmureg, reboot_cmd_offset, REBOOT_MODE_*)` on charge/bootloader/recovery/sfactory reboots |
| `magic-inform` = 0x808 (PMU INFORM2) | yes | bits 23:16 := `0x4E` SEC_REBOOT_NORMAL, other bits kept | `sec_reboot()` → `sec_set_reboot_magic(SEC_REBOOT_NORMAL, 16, 0xFF)` (read, mask, write) on **every** reboot, panic included |
| `panic-inform` = 0x80c (PMU INFORM3) | yes | `0x12345670` SEC_RESET_REASON_UNKNOWN | `sec_reboot()` on every reboot without a special command; `sec_power_off()` before its fallback reset |

Interlock: the module writes nothing unless the three DT offsets are exactly
0x810, 0x808 and 0x80c. So it cannot touch another register on another
device or DT.

Other kernel users of these registers (searched in the official source):
- `sec_debug_base.c` writes panic-inform (upload cause) inside its panic
  notifier.
- `rtc-s2mpu13.c` (`rtc_s2mpu13` is loaded here) reads panic-inform once in
  probe. It only tests the value for nonzero, and overwrites the result with
  `pon_alarm_get_lpcharge()` before using it. RECOVERY 0x12345674 and
  UNKNOWN 0x12345670 are both nonzero.
- Nothing else uses these offsets, and the live DT holds exactly one of each
  property.

Idempotent: a second run finds the values already set and writes nothing.
Loaded again while loaded it fails with `EEXIST`. During normal operation the
expected values already hold (no writes). If they don't, the writes equal
what the next clean reboot writes anyway, apart from 0x810, where it writes
the NORMAL mode value.

## KMI evidence (`build-module.sh` → `check-kmi.py`)

- vermagic `5.10.223-android12-9-30597860-abA266MUBU1AYB8 SMP preempt
  mod_unload modversions aarch64`.
- 9 imports, all from vmlinux, none from vendor modules: `syscon_regmap_lookup_by_phandle`,
  `regmap_read`, `regmap_write`, `of_find_compatible_node`,
  `of_property_read_variable_u32_array`, `printk`, `param_ops_bool`,
  `__stack_chk_fail`, `__stack_chk_guard`. Their CRCs and `module_layout`
  (0x7c24b32d) equal both the installed OSS kernel's Module.symvers and the
  export table recovered from Samsung's shipped A266MUBU1AYB8 Image:
  0 problems.
- Signing: `CONFIG_MODULE_SIG` off. No LoadPin, no lockdown LSM,
  `MODULE_UNLOAD=y`. RKP registers the module's text with uH as it does for
  each vendor module.

## Proposed on-device test (needs explicit approval; not run)

It touches nothing but the three registers above, through the kernel's own
regmap, plus a module load/unload.

1. `adb push oss-rebootmode-clear.ko /data/local/tmp/` and check its sha256.
2. Dry run: `su -c 'insmod /data/local/tmp/oss-rebootmode-clear.ko dry_run=1; dmesg | grep oss-rebootmode-clear'`.
   Only three 32-bit MMIO reads (0x11860808/0c/10). 0x808 is read on every
   reboot and 0x80c at every RTC probe, through this same regmap.
   Then `su -c 'rmmod oss_rebootmode_clear'`.
3. Real run: the same insmod without `dry_run`, read the `before:`/`after:`
   lines, `rmmod`. On a normally booted phone the expected `before:` is
   reboot-cmd 0x0, magic bits 23:16 = 0x4E, panic-inform 0x12345670, giving
   **no writes**. Any write it does make is listed in `after:`.
4. `adb reboot`: it must boot BOOT normally (Moon, 306 modules).

Risk, honestly:
- **Lowest:** the dry run. It does only reads the kernel already performs.
- **Writes in step 3:** if any happen, they are values the normal reboot path
  writes. 0x810 is the exception: it is set to NORMAL only from a recovery or
  other mode value, which a normally booted phone should not hold.
- **Worst plausible case:** an oops while loading. The cmdline has
  `oops=panic`, so that means a panic and reboot. In normal mode the
  registers already select BOOT, the direct-boot policy gives one Android
  boot after a panic, and RESCUE.md applies.
- **Not covered by this test:** the real purpose, clearing a *recovery*
  selection. That belongs to the stage-2 canary, which will log `before:`
  reboot-cmd 0x1 / panic-inform 0x12345674.
