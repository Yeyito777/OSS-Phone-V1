/* BCB-free recovery entry. Calls reboot(2) with "recovery" directly, bypassing
 * Android init's DoReboot (which writes "boot-recovery" into the misc BCB and
 * would make recovery sticky). The Samsung kernel restart handlers
 * (exynos-reboot, sec_reboot) record the request only in a PMU reason register,
 * which is one-shot: a later panic/watchdog/manual reset sets a different
 * reason, so the bootloader then boots BOOT, not recovery. Run as root. */
#define _GNU_SOURCE
#include <unistd.h>
#include <sys/syscall.h>
#include <linux/reboot.h>
#include <stdio.h>
int main(void){
  sync();
  long r=syscall(SYS_reboot, LINUX_REBOOT_MAGIC1, LINUX_REBOOT_MAGIC2,
                 LINUX_REBOOT_CMD_RESTART2, "recovery");
  dprintf(2,"reboot(recovery) returned %ld (should not return)\n", r);
  return 1;
}
