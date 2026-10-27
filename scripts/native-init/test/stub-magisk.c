/* Test stand-in for magiskinit inside the container. MODE via argv0 suffix file /stub-mode. */
#define _GNU_SOURCE
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <dirent.h>
#include <sys/stat.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/syscall.h>
#include <linux/reboot.h>
static void lsroot(void){
  DIR*d=opendir("/"); struct dirent*e; fprintf(stderr,"[ctr] / =");
  while((e=readdir(d))) if(e->d_name[0]!='.'||strlen(e->d_name)>2) fprintf(stderr," %s",e->d_name);
  fprintf(stderr,"\n"); closedir(d);
  struct stat st; fprintf(stderr,"[ctr] /init.magisk:%s /init.stock:%s /.backup/init.xz:%s /mnt:%s pid=%d\n",
    stat("/init.magisk",&st)?"absent":"PRESENT", stat("/init.stock",&st)?"absent":"PRESENT",
    stat("/.backup/init.xz",&st)?"absent":"present", stat("/mnt",&st)?"absent":"present", getpid());
}
int main(void){
  char mode[32]={0}; FILE*f=fopen("/stub-mode","r"); if(f){ if(fgets(mode,sizeof mode,f)) mode[strcspn(mode,"\n")]=0; fclose(f);}
  lsroot();
  if(!strcmp(mode,"crash")){
    /* SwitchRoot-style: mounts and an MS_MOVE inside the container ns, then die. */
    mkdir("/sysx",0755); mkdir("/devx",0755);
    int a=mount("tmpfs","/devx","tmpfs",0,NULL), b=mount("tmpfs","/sysx","tmpfs",0,NULL);
    mkdir("/sysx/devx",0755);
    int c=mount("/devx","/sysx/devx",NULL,MS_MOVE,NULL);
    fprintf(stderr,"[ctr] mounts tmpfs=%d,%d move=%d; crashing\n",a,b,c);
    abort();
  }
  if(!strcmp(mode,"hang")){ fprintf(stderr,"[ctr] hanging\n"); for(;;) pause(); }
  if(!strcmp(mode,"reboot")){ fprintf(stderr,"[ctr] contained Android requests reboot\n"); syscall(SYS_reboot,LINUX_REBOOT_MAGIC1,LINUX_REBOOT_MAGIC2,LINUX_REBOOT_CMD_RESTART,NULL); }
  if(!strcmp(mode,"poweroff")){ fprintf(stderr,"[ctr] contained Android requests power-off\n"); syscall(SYS_reboot,LINUX_REBOOT_MAGIC1,LINUX_REBOOT_MAGIC2,LINUX_REBOOT_CMD_POWER_OFF,NULL); }
  if(!strcmp(mode,"exit")){ fprintf(stderr,"[ctr] late failure: exit 42 (a real failure, not 1=reboot/2=poweroff)\n"); _exit(42); }
  fprintf(stderr,"[ctr] unknown mode %s\n",mode); _exit(2);
}
