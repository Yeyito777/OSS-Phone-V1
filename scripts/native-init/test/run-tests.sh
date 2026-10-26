#!/usr/bin/env bash
# oss-init v4 test bench (research host with binfmt qemu-aarch64 'F', strace,
# lz4, cpio, unpack_bootimg). oss-init is built natively with -DTEST_STDERR and
# runs as PID 1 of a fresh user+pid+mount+net namespace, chrooted (on a bind
# mount, as rootfs is a mount on the phone) into the REAL merged recovery-slot
# ramdisk. Stub cases: live, layout-crash (SwitchRoot-style mounts + MS_MOVE, then
# crash), hang, late-reboot, contained-reboot. The real magiskinit / stock init
# case is documented in notes/stage2/V4-CONTAINMENT-VALIDATION.md.
#   scripts/native-init/test/run-tests.sh [layout-crash|hang|late-reboot|contained-reboot|all]
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
R=~/Workspace/research/linux-galaxy-a26; MB=$R/tools/magiskboot-x86_64
W=~/oss-kernel/.v4test; mkdir -p $W; cd $W
SRC=$HERE/../oss-init.c
# x86 oss-init test builds (kmsg reader -> /kmsg.test, 3 s deadline)
gcc -static -O1 -w -DTEST_STDERR -DKMSG_READ='"/kmsg.test"' -DLIVENESS_DEADLINE_MS=3000 $SRC -o oss-init-test
gcc -static -O1 -w $HERE/stub-magisk.c -o stub-magisk
gcc -static -O1 -w $HERE/stub-stock.c -o stub-stock
# pristine merged ramdisk (v4 load order) from the real fragments, once
if [ ! -d base ]; then
  mkdir base; cd base
  for f in ~/oss-kernel/.recslot/vb/vendor_ramdisk00 ~/oss-kernel/.recslot/vb/vendor_ramdisk01; do lz4 -dc $f 2>/dev/null | cpio -idmu 2>/dev/null; done
  U=$(mktemp -d); unpack_bootimg --boot_img ~/oss-kernel/out/boot/boot-oss-mirror-A266MUBU1AYB8.img --out $U >/dev/null
  lz4 -dc $U/ramdisk 2>/dev/null > $U/r.cpio; cpio -idmu < $U/r.cpio 2>/dev/null
  chmod -R u+rwX .; chmod 755 .backup
  for f in init.xz .magisk .rmlist; do $MB cpio $U/r.cpio "extract .backup/$f .backup/$f" >/dev/null 2>&1; done
  chmod 644 .backup/.magisk .backup/.rmlist
  cp init ../real-magiskinit; xz -dc .backup/init.xz > ../real-stockinit; chmod 755 ../real-stockinit
  cd ..
fi
mk() { # mk NAME MAGISK STOCK MODE KMSGLINE
  rm -rf t-$1; cp -a base t-$1
  cp oss-init-test t-$1/init; cp $2 t-$1/init.magisk; cp $3 t-$1/init.stock
  chmod 750 t-$1/init t-$1/init.magisk t-$1/init.stock
  echo "$4" > t-$1/stub-mode; printf '%s\n' "$5" > t-$1/kmsg.test
  mkdir -p t-$1/proc t-$1/sys t-$1/dev t-$1/mnt
}
run() { # run NAME
  echo "================ $1 ================"
  timeout -s KILL ${2:-30} unshare -Urmnpf sh -c "mount --bind $W/t-$1 $W/t-$1 && cd $W/t-$1 && exec chroot . /init" 2>&1 | grep -aE "oss-init|oss-init test|\[ctr\]|\[fallback\]" | head -40
  echo "exit status (128+signal if killed): ${PIPESTATUS[0]}"
}
case "${1:-all}" in
  layout-crash|all)
    mk layout-crash stub-magisk stub-stock crash "boot log line"
    run layout-crash ;;
esac
case "${1:-all}" in
  hang|all)
    mk hang stub-magisk stub-stock hang "boot log line"
    run hang ;;
esac
case "${1:-all}" in
  late-reboot|all)
    mk late-reboot stub-magisk stub-stock exit "sec-reboot sec-reboot: register restart handler successfully"
    run late-reboot ;;
esac
case "${1:-all}" in
  contained-reboot|all)
    mk contained-reboot stub-magisk stub-stock reboot "sec-reboot sec-reboot: register restart handler successfully"
    run contained-reboot ;;
esac
case "${1:-all}" in
  live|all)
    # Liveness already logged, container then idles: must be supervised with
    # no deadline kill and no fallback; the bench timeout (8 s) ends it.
    mk live stub-magisk stub-stock hang "OSSPHONE_CONTAIN_LIVE"
    run live 8 ;;
esac
