/* Test stand-in for the stock init as fallback: proves it was reached, by which path, and shows mounts. */
#define _GNU_SOURCE
#include <stdio.h>
#include <unistd.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/stat.h>
int main(int argc,char**argv){
  char exe[256]={0}; readlink("/proc/self/exe",exe,sizeof exe-1);
  mkdir("/proc",0755); mount("proc","/proc","proc",0,NULL);
  readlink("/proc/self/exe",exe,sizeof exe-1);
  fprintf(stderr,"[fallback] STOCK INIT REACHED pid=%d argv0=%s exe=%s\n",getpid(),argc?argv[0]:"?",exe);
  FILE*f=fopen("/proc/self/mountinfo","r"); char l[512]; int n=0;
  while(f&&fgets(l,sizeof l,f)){ n++; fprintf(stderr,"[fallback] mount: %s",l);} 
  fprintf(stderr,"[fallback] mount count in oss-init's namespace = %d (expect 2: root + the proc this stub just mounted)\n",n);
  struct stat st; fprintf(stderr,"[fallback] original tree intact: /init.magisk:%s /init.stock:%s\n",
     stat("/init.magisk",&st)?"MISSING":"present", stat("/init.stock",&st)?"MISSING":"present");
  return 0;
}
