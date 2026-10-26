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

## On-device test (approved; 2026-10-26, BOOT 8c0461ee, Moon on the GPU tier)

1. Pushed: sha256 `a18606a8…4054` matched; BOOT `8c0461ee`, 306 modules.
2. `insmod … dry_run=1`, then `rmmod`:
   `before: reboot-cmd[0x810]=0x48 magic-inform[0x808]=0x4e4e0000 panic-inform[0x80c]=0x25b7ce71 (dry run)`
3. `insmod`, then `rmmod`:
   `before:` the same, then `reboot-cmd 0x48 is not a reboot mode; left unchanged`,
   then `after:  reboot-cmd[0x810]=0x48 magic-inform[0x808]=0x4e4e0000 panic-inform[0x80c]=0x12345670`.
   One write: panic-inform to UNKNOWN, the value `sec_reboot()` writes on
   the very next reboot.
4. `adb reboot`: normal boot to BOOT, Moon ready about 42 s later, OSS
   kernel, 306 modules, taint 1024 (4096 "out-of-tree" only until the
   reboot), BOOT unchanged.

What the live values show:
- In a normal boot, 0x810 holds **0x48**, not 0x00. Something other than
  exynos-reboot (presumably the bootloader) uses it, so the interlock that
  only clears 0x01..0x05 is needed.
- panic-inform holds a per-boot value (0x25b7ce71), not UNKNOWN, between
  reboots.
- magic-inform carries 0x4E in both bits 31:24 and 23:16.

Before relying on the module in the recovery-slot canary, read its
`before:` line from a recovery boot. It should show reboot-cmd 0x01 and
panic-inform 0x12345674. If 0x810 holds another value there, the module will
not change it and logs that instead.
