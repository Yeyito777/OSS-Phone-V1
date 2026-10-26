/* OSS-Phone canary recovery init. Replaces /init in a recovery image whose
 * kernel/dtb/dtbo are stock. It writes one marker to the kernel log, then
 * exits as PID 1 -> "Attempted to kill init" -> panic/reset. This reproduces
 * the exact stage-2 v2/v3 failure (PID-1 death) ON PURPOSE, inside a recovery
 * boot, to measure whether the device returns to BOOT on its own. It makes no
 * mounts, touches no partition, loads no module. */
#define _GNU_SOURCE
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <string.h>
#include <unistd.h>
int main(void){
  mkdir("/dev",0755);
  mknod("/dev/kmsg", S_IFCHR|0600, makedev(1,11));
  int fd=open("/dev/kmsg",O_WRONLY|O_CLOEXEC);
  if(fd>=0){
    const char m[]="<3>OSS-CANARY: recovery init reached PID 1; exiting now to force a PID-1 panic/reset (expected)\n";
    (void)!write(fd,m,sizeof(m)-1);
    close(fd);
  }
  /* Give the kmsg line a moment to flush to pstore before PID 1 dies. */
  sync();
  _exit(0);
}
