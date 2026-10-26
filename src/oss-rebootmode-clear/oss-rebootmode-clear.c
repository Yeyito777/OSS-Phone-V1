// SPDX-License-Identifier: GPL-2.0-only
/*
 * oss-rebootmode-clear: select a NORMAL boot for the next reset.
 *
 * Samsung's bootloader picks recovery from PMU alive-domain registers that
 * survive warm, panic and PMIC resets (notes/stage2/RECOVERY-PERSISTENCE-
 * ROOTCAUSE.md). A recovery-slot test image loads this first, so a later
 * panic or hang comes back to BOOT. It performs only register operations the
 * kernel's own reboot drivers perform, on the same regmap, taken from the
 * same device-tree nodes:
 *
 *   drivers/power/reset/exynos-reboot.c  exynos_reboot_parse():
 *       regmap_write(pmureg, reboot_cmd_offset, REBOOT_MODE_*)
 *     here REBOOT_MODE_NORMAL (0x00), and only if the register holds one of
 *     the modes that driver writes (0x01..0x05); anything else is left alone.
 *   drivers/samsung/sec_reboot.c  sec_reboot(), every non-panic reboot:
 *       sec_set_reboot_magic(SEC_REBOOT_NORMAL, SEC_REBOOT_END_OFFSET, 0xFF)
 *         = read magic_inform, replace bits 23:16 with 0x4E, write back
 *       regmap_write(pmureg, panic_inform, SEC_RESET_REASON_UNKNOWN)
 *
 * sec_set_reboot_magic() itself is exported by sec_reboot.ko, but that is a
 * vendor module loaded by Android's first-stage init, after oss-init runs;
 * the syscon regmap is built in and needs no module.
 *
 * Safe twice (idempotent; unchanged registers are not rewritten) and during
 * normal operation (the next clean reboot rewrites the sec informs anyway).
 * Refuses to write unless the device tree carries exactly the offsets
 * reviewed for SM-A266M (s5e8825). dry_run=1 only reads and logs.
 */
#define pr_fmt(fmt) "oss-rebootmode-clear: " fmt

#include <linux/err.h>
#include <linux/mfd/syscon.h>
#include <linux/module.h>
#include <linux/of.h>
#include <linux/regmap.h>

/* exynos-reboot.c */
#define REBOOT_MODE_NORMAL	0x00
#define REBOOT_MODE_LAST	0x05	/* FASTBOOTD; RECOVERY is 0x01 */
/* sec_reboot.c */
#define SEC_REBOOT_END_OFFSET	16
#define SEC_REBOOT_NORMAL	0x4E
#define SEC_REBOOT_MASK		(0xFFu << SEC_REBOOT_END_OFFSET)
#define SEC_RESET_REASON_UNKNOWN 0x12345670	/* SEC_RESET_REASON_PREFIX | 0x0 */

/* Live DT of SM-A266M A266MUBU1AYB8, syscon system-controller@11860000. */
#define EXPECT_REBOOT_CMD	0x810
#define EXPECT_MAGIC_INFORM	0x808
#define EXPECT_PANIC_INFORM	0x80c

static bool dry_run;
module_param(dry_run, bool, 0444);
MODULE_PARM_DESC(dry_run, "only read and log the registers");

static int lookup(const char *compatible, const char *prop, u32 expect,
		  struct regmap **map, u32 *offset)
{
	struct device_node *np = of_find_compatible_node(NULL, NULL, compatible);
	int ret = -ENODEV;

	if (!np) {
		pr_err("no %s node\n", compatible);
		return ret;
	}
	*map = syscon_regmap_lookup_by_phandle(np, "samsung,syscon-phandle");
	if (IS_ERR(*map)) {
		pr_err("%s: no syscon regmap (%ld)\n", compatible, PTR_ERR(*map));
		ret = PTR_ERR(*map);
	} else if (of_property_read_u32(np, prop, offset)) {
		pr_err("%s: no %s\n", compatible, prop);
	} else if (*offset != expect) {
		pr_err("%s: %s = %#x, reviewed value is %#x; refusing\n",
		       compatible, prop, *offset, expect);
	} else {
		ret = 0;
	}
	of_node_put(np);
	return ret;
}

static int __init oss_rebootmode_clear_init(void)
{
	struct regmap *pmu, *sec_pmu;
	u32 cmd_off, magic_off, panic_off;
	u32 cmd, magic, panic, want_magic;
	int ret;

	ret = lookup("samsung,exynos-reboot", "reboot-cmd-offset", EXPECT_REBOOT_CMD,
		     &pmu, &cmd_off);
	if (!ret)
		ret = lookup("samsung,sec-reboot", "magic-inform", EXPECT_MAGIC_INFORM,
			     &sec_pmu, &magic_off);
	if (!ret)
		ret = lookup("samsung,sec-reboot", "panic-inform", EXPECT_PANIC_INFORM,
			     &sec_pmu, &panic_off);
	if (ret)
		return ret;
	if (pmu != sec_pmu) {
		pr_err("exynos-reboot and sec-reboot use different regmaps; refusing\n");
		return -ENODEV;
	}

	ret = regmap_read(pmu, cmd_off, &cmd) ?:
	      regmap_read(pmu, magic_off, &magic) ?:
	      regmap_read(pmu, panic_off, &panic);
	if (ret) {
		pr_err("read failed (%d)\n", ret);
		return ret;
	}
	pr_info("before: reboot-cmd[%#x]=%#x magic-inform[%#x]=%#x panic-inform[%#x]=%#x%s\n",
		cmd_off, cmd, magic_off, magic, panic_off, panic,
		dry_run ? " (dry run)" : "");
	if (dry_run)
		return 0;

	if (cmd > REBOOT_MODE_NORMAL && cmd <= REBOOT_MODE_LAST)
		ret = regmap_write(pmu, cmd_off, REBOOT_MODE_NORMAL);
	else if (cmd != REBOOT_MODE_NORMAL)
		pr_warn("reboot-cmd %#x is not a reboot mode; left unchanged\n", cmd);

	want_magic = (magic & ~SEC_REBOOT_MASK) | (SEC_REBOOT_NORMAL << SEC_REBOOT_END_OFFSET);
	if (!ret && magic != want_magic)
		ret = regmap_write(pmu, magic_off, want_magic);

	if (!ret && panic != SEC_RESET_REASON_UNKNOWN)
		ret = regmap_write(pmu, panic_off, SEC_RESET_REASON_UNKNOWN);

	if (ret) {
		pr_err("write failed (%d)\n", ret);
		return ret;
	}
	regmap_read(pmu, cmd_off, &cmd);
	regmap_read(pmu, magic_off, &magic);
	regmap_read(pmu, panic_off, &panic);
	pr_info("after:  reboot-cmd[%#x]=%#x magic-inform[%#x]=%#x panic-inform[%#x]=%#x\n",
		cmd_off, cmd, magic_off, magic, panic_off, panic);
	return 0;
}

static void __exit oss_rebootmode_clear_exit(void)
{
}

module_init(oss_rebootmode_clear_init);
module_exit(oss_rebootmode_clear_exit);
MODULE_DESCRIPTION("Select a NORMAL boot in the Exynos PMU reboot registers");
MODULE_LICENSE("GPL v2");
