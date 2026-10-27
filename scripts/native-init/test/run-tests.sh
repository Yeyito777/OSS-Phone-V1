#!/usr/bin/env bash
# Off-device test bench for the recovery-slot /init programs (research host with
# binfmt qemu-aarch64 'F', lz4, cpio, unpack_bootimg, gcc, strace).
#
# Each case runs one init as PID 1 of a fresh user+pid+mount+net namespace,
# chrooted (on a bind mount, as rootfs is a mount on the phone) into the REAL
# merged recovery-slot ramdisk. aarch64 programs (magiskinit, stock init) run
# through binfmt. A user namespace can never load a module, so:
#   - the containment cases use -DTEST_FAKE_CLEAR=0 (pretend the clear loaded);
#   - the *-noclear cases call the REAL finit_module on the REAL
#     oss-rebootmode-clear.ko and get EPERM: a genuine failed load.
#   scripts/native-init/test/run-tests.sh [case|all]
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$HERE/.."
R=~/Workspace/research/linux-galaxy-a26; MB=$R/tools/magiskboot-x86_64
KO="${A26_REBOOTMODE_CLEAR_KO:-$HOME/oss-kernel/out/modules/oss-rebootmode-clear/oss-rebootmode-clear.ko}"
W=~/oss-kernel/.v4test; mkdir -p $W; cd $W
T="-static -O1 -w -DTEST_STDERR -DKMSG_READ=\"/kmsg.test\" -DLIVENESS_DEADLINE_MS=3000 -I$SRC"
gcc $T -DTEST_FAKE_CLEAR=0 $SRC/oss-init.c -o oss-init-fake
gcc $T -DTEST_FAKE_CLEAR=0 -DTEST_BOOTCOUNT=3 $SRC/oss-init.c -o oss-init-bc3
gcc $T -DTEST_FAKE_CLEAR=0 -DTEST_BOOTCOUNT=1 $SRC/oss-init.c -o oss-init-bc1
gcc $T $SRC/oss-init.c -o oss-init-real
gcc $T -DTEST_FAKE_CLEAR=0 $SRC/oss-pid1.c -o oss-pid1-fake
gcc $T $SRC/oss-pid1.c -o oss-pid1-real
gcc $T -DTEST_FAKE_CLEAR=0 -DRECOVERY_INIT=\"/init.stock\" $SRC/canary/canary-init.c -o canary-fake
gcc $T -DRECOVERY_INIT=\"/init.stock\" $SRC/canary/canary-init.c -o canary-real
gcc -static -O1 -w $HERE/stub-magisk.c -o stub-magisk
gcc -static -O1 -w $HERE/stub-stock.c -o stub-stock
if [ ! -d base ]; then   # pristine merged ramdisk (v4 load order), once
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
mk() { # mk NAME INIT MAGISK STOCK MODE KMSGLINE
  rm -rf t-$1; cp -a base t-$1
  cp $2 t-$1/init; cp $3 t-$1/init.magisk; cp $4 t-$1/init.stock; cp "$KO" t-$1/oss-rebootmode-clear.ko
  chmod 750 t-$1/init t-$1/init.magisk t-$1/init.stock
  echo "$5" > t-$1/stub-mode; printf '%s\n' "$6" > t-$1/kmsg.test
  mkdir -p t-$1/proc t-$1/sys t-$1/dev t-$1/mnt
}
run() { # run NAME [timeout]
  echo "================ $1 ================"
  timeout -s KILL ${2:-30} unshare -Urmnpf sh -c "mount --bind $W/t-$1 $W/t-$1 && cd $W/t-$1 && exec chroot . /init" 2>&1 |
    grep -aE "oss-init|oss-pid1|OSS-CANARY|\[ctr\]|\[fallback\]" | head -40
  echo "exit status (128+signal if killed): ${PIPESTATUS[0]}"
}
want() { [ "${1}" = all ] || [ "${1}" = "$2" ]; }
C="${1:-all}"
# --- oss-init containment (clear faked as loaded) ---
want $C live             && { mk live oss-init-fake stub-magisk stub-stock hang "OSSPHONE_CONTAIN_LIVE"; run live 8; }
want $C layout-crash     && { mk layout-crash oss-init-fake stub-magisk stub-stock crash "boot"; run layout-crash; }
want $C hang             && { mk hang oss-init-fake stub-magisk stub-stock hang "boot"; run hang; }
want $C late-reboot      && { mk late-reboot oss-init-fake stub-magisk stub-stock exit "sec-reboot sec-reboot: register restart handler successfully"; run late-reboot; }
want $C contained-reboot && { mk contained-reboot oss-init-fake stub-magisk stub-stock reboot "sec-reboot sec-reboot: register restart handler successfully"; run contained-reboot; }
want $C contained-poweroff && { mk contained-poweroff oss-init-fake stub-magisk stub-stock poweroff "sec-reboot sec-reboot: register restart handler successfully"; run contained-poweroff; }
want $C bootcount-skip   && { mk bootcount-skip oss-init-bc3 stub-magisk stub-stock hang "boot"; run bootcount-skip; }
want $C bootcount-ok     && { mk bootcount-ok oss-init-bc1 stub-magisk stub-stock hang "OSSPHONE_CONTAIN_LIVE"; run bootcount-ok 8; }
# --- the clear did NOT load (real finit_module -> EPERM) ---
want $C oss-init-noclear && { mk oss-init-noclear oss-init-real stub-magisk stub-stock crash "boot"; run oss-init-noclear; }
want $C oss-pid1-noclear && { mk oss-pid1-noclear oss-pid1-real stub-magisk stub-stock crash "boot"; run oss-pid1-noclear; }
want $C canary-noclear   && { mk canary-noclear canary-real stub-magisk stub-stock crash "boot"; run canary-noclear; }
# --- clear loaded: plain pre-init hands PID 1 to the REAL magiskinit as /init; canary exits ---
want $C oss-pid1-real-magisk && { mk oss-pid1-real-magisk oss-pid1-fake real-magiskinit stub-stock crash "boot"; run oss-pid1-real-magisk 60; }
want $C canary-clear     && { mk canary-clear canary-fake stub-magisk stub-stock crash "boot"; run canary-clear; }
exit 0
