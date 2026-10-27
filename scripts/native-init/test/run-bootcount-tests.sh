#!/usr/bin/env bash
# Bench for the boot-attempt counter in misc (oss-init v7, bootcount-misc.h).
#   scripts/native-init/test/run-bootcount-tests.sh [file|cli|device|all]
# (a) file:   oss-init as PID 1 of a user+pid+mount namespace, the container a
#             stub; misc = a 1 MiB file, sysfs = a fake tree (-DTEST_SYSFS).
#             Exercises discovery, freeze, bump, A/B, torn and foreign slots,
#             the skip decision, and promoted-mode escalation.
# (b) cli:    `oss-init --bootcount show|init|bump|clear` on file images.
# (c) device: a REAL block device (sudo): a GPT image with a 1 MiB partition
#             named "misc", attached with losetup -P, so sysfs has a real
#             PARTNAME=misc. Runs the CLI and the PRODUCTION helper path (real
#             sysfs mount in a private namespace, mknod, O_DIRECT, O_DSYNC) as
#             root in a private mount+pid namespace; the PMU clear is faked
#             (-DTEST_FAKE_CLEAR=0) so nothing is ever loaded into this kernel.
# Every case checks that no byte outside 0x40000-0x41fff changed (the images
# carry non-zero BCB, 0x8000 and 0x7f000 regions like the live phone).
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$HERE/.."
MT="python3 -I $HERE/misc-tool.py"
W=~/oss-kernel/.bctest; rm -rf "$W"; mkdir -p "$W"; cd "$W"
T="-static -O1 -w -DTEST_STDERR -DKMSG_READ=\"/kmsg.test\" -DLIVENESS_DEADLINE_MS=4000 -DBC_FIND_DEADLINE_MS=2500 -DTEST_FAKE_CLEAR=0 -I$SRC"
gcc $T -DTEST_SYSFS=\"/fakesys\" -DTEST_MISC_NODE=\"/misc.img\" "$SRC/oss-init.c" -o oi-file
gcc $T -DTEST_SYSFS=\"/fakesys\" -DTEST_MISC_NODE=\"/misc.img\" -DOSS_PROMOTED=1 "$SRC/oss-init.c" -o oi-file-prom
gcc $T "$SRC/oss-init.c" -o oi-dev
gcc -static -O1 -w "$SRC/oss-init.c" -o oi-cli
gcc -static -O1 -w "$HERE/stub-magisk.c" -o stub-magisk
gcc -static -O1 -w "$HERE/stub-stock.c" -o stub-stock
PASS=0; FAIL=0
ok()  { echo "  PASS $*"; PASS=$((PASS + 1)); }
bad() { echo "  FAIL $*"; FAIL=$((FAIL + 1)); }
expect() { # expect OUTPUT-FILE PATTERN...
  local f=$1; shift
  for p in "$@"; do grep -qF -- "$p" "$f" && ok "has: $p" || bad "missing: $p"; done
}
expect_not() { local f=$1; shift; for p in "$@"; do grep -qF -- "$p" "$f" && bad "unexpected: $p" || ok "absent: $p"; done; }

fakesys() { # fakesys DIR NAME SECTORS
  mkdir -p "$1/class/block/$2"
  printf 'MAJOR=259\nMINOR=5\nDEVNAME=%s\nDEVTYPE=partition\nPARTN=21\nPARTNAME=misc\n' "$2" >"$1/class/block/$2/uevent"
  echo "$3" >"$1/class/block/$2/size"
}
# ---------------- (a) file ----------------
mkcase() { # mkcase NAME BIN SYSFS MISC-ARGS...
  local n=$1 bin=$2 sys=$3; shift 3
  local t="$W/t-$n"; mkdir -p "$t"/{proc,sys,dev,mnt}
  cp "$bin" "$t/init"; cp stub-magisk "$t/init.magisk"; cp stub-stock "$t/init.stock"
  chmod 750 "$t/init" "$t/init.magisk" "$t/init.stock"
  echo hang >"$t/stub-mode"
  if [ "${NOTLIVE:-0}" = 1 ]; then printf 'boot\n' >"$t/kmsg.test"; else printf 'OSSPHONE_CONTAIN_LIVE\n' >"$t/kmsg.test"; fi
  $MT make "$t/misc.img" "$@"; cp "$t/misc.img" "$W/$n.ref"
  case "$sys" in
    present)   fakesys "$t/fakesys" sdz21 2048 ;;
    wrongsize) fakesys "$t/fakesys" sdz21 4096 ;;
    ambiguous) fakesys "$t/fakesys" sdz21 2048; fakesys "$t/fakesys" sdy7 2048 ;;
    late|none) mkdir -p "$t/fakesys/class/block" ;;
  esac
}
runcase() { # runcase NAME SYSFS
  local n=$1 t="$W/t-$1"
  echo "================ $n ================"
  [ "$2" = late ] && ( sleep 1.2; fakesys "$t/fakesys" sdz21 2048 ) &
  timeout -s KILL 7 unshare -Urmnpf sh -c "mount --bind $t $t && cd $t && exec chroot . /init" >"$W/$n.out" 2>&1
  wait
  grep -aE "oss-init|\[ctr\] /init" "$W/$n.out" | sed 's/^/    /' | cut -c1-150
  $MT check "$t/misc.img" "$W/$n.ref" >"$W/$n.misc"; local rc=$?
  sed 's/^/    misc: /' "$W/$n.misc"
  [ $rc = 0 ] && ok "nothing outside 0x40000-0x41fff changed" || bad "bytes outside the slots changed"
}
contained() { expect "$W/$1.out" "container is live" "/init.stock:absent"; expect_not "$W/$1.out" "skipping containment"; }
skipped()   { expect "$W/$1.out" "skipping containment" "booting magiskinit as /init" "/init.stock:PRESENT"; expect_not "$W/$1.out" "container is live"; }
if [ "${1:-all}" = all ] || [ "$1" = file ]; then
  mkcase fresh oi-file present;                       runcase fresh present
  expect "$W/fresh.out" "status=ok count=1 found=0 wrote=1 froze=1"; contained fresh
  expect "$W/fresh.misc" "slotA=VALID(seq=1,count=1) slotB=ZERO"
  mkcase second oi-file present --seed A:4:1;         runcase second present
  expect "$W/second.out" "status=ok count=2 found=0 wrote=1"; contained second
  expect "$W/second.misc" "slotA=VALID(seq=4,count=1) slotB=VALID(seq=5,count=2)"
  NOTLIVE=1 mkcase limit oi-file present --seed A:7:2;          runcase limit present
  expect "$W/limit.out" "status=ok count=3" "consecutive failed boots reached the limit"; skipped limit
  expect "$W/limit.misc" "slotB=VALID(seq=8,count=3)"
  NOTLIVE=1 mkcase capped oi-file present --seed B:9:65535;     runcase capped present
  expect "$W/capped.out" "status=ok count=65535"; skipped capped
  expect "$W/capped.misc" "slotA=VALID(seq=10,count=65535)"
  mkcase foreign oi-file present --seed A:3:1 --foreign B; runcase foreign present
  expect "$W/foreign.out" "status=foreign" "wrote=0"; contained foreign
  expect "$W/foreign.misc" "image=IDENTICAL"
  mkcase torn oi-file present --seed A:5:1 --torn B:6:2;  runcase torn present
  expect "$W/torn.out" "status=ok count=2 found=0 wrote=1"; contained torn
  expect "$W/torn.misc" "slotA=VALID(seq=5,count=1) slotB=VALID(seq=6,count=2)"
  mkcase damaged oi-file present --damaged A --damaged B; runcase damaged present
  expect "$W/damaged.out" "status=ok count=1 found=0 wrote=1"; contained damaged
  expect "$W/damaged.misc" "slotA=VALID(seq=1,count=1) slotB=DAMAGED"
  # limit reached only after the container is live: never interrupt a live container
  mkcase livelimit oi-file late --seed A:7:2;         runcase livelimit late
  expect "$W/livelimit.out" "status=ok count=3" "already live; not interrupting it" "container is live" "/init.stock:absent"
  expect_not "$W/livelimit.out" "booting magiskinit as /init"
  mkcase late oi-file late;                           runcase late late
  expect "$W/late.out" "status=ok count=1 found=0 wrote=1 froze=1"; contained late
  mkcase nosysfs oi-file none;                        runcase nosysfs none
  expect "$W/nosysfs.out" "status=nostore count=0 found=-1 wrote=0 froze=0"; contained nosysfs
  expect "$W/nosysfs.misc" "image=IDENTICAL"
  mkcase wrongsize oi-file wrongsize;                 runcase wrongsize wrongsize
  expect "$W/wrongsize.out" "status=nostore count=0 found=-3"; contained wrongsize
  expect "$W/wrongsize.misc" "image=IDENTICAL"
  mkcase ambiguous oi-file ambiguous;                 runcase ambiguous ambiguous
  expect "$W/ambiguous.out" "status=nostore count=0 found=-2"; contained ambiguous
  expect "$W/ambiguous.misc" "image=IDENTICAL"
  # promoted builds: no trustworthy count => escalate; a valid record => count normally
  NOTLIVE=1 mkcase pr-absent oi-file-prom present;              runcase pr-absent present
  expect "$W/pr-absent.out" "status=none count=0" "wrote=0" "promoted=1" "promoted boot without a trustworthy boot count"; skipped pr-absent
  expect "$W/pr-absent.misc" "image=IDENTICAL"
  mkcase pr-valid oi-file-prom present --seed A:3:0;  runcase pr-valid present
  expect "$W/pr-valid.out" "status=ok count=1 found=0 wrote=1" "promoted=1"; contained pr-valid
  NOTLIVE=1 mkcase pr-foreign oi-file-prom present --foreign A; runcase pr-foreign present
  expect "$W/pr-foreign.out" "status=foreign"; skipped pr-foreign
  expect "$W/pr-foreign.misc" "image=IDENTICAL"
  NOTLIVE=1 mkcase pr-damaged oi-file-prom present --damaged A; runcase pr-damaged present
  expect "$W/pr-damaged.out" "status=none"; skipped pr-damaged
  mkcase pr-nosysfs oi-file-prom none;                runcase pr-nosysfs none
  expect "$W/pr-nosysfs.out" "status=nostore count=0 found=-1"; contained pr-nosysfs
fi
# ---------------- (b) cli ----------------
cli() { OSS_BC_SYSROOT="$W/cli/fakesys" OSS_BC_NODE="$W/cli/misc.img" ./oi-cli --bootcount "$1"; }
if [ "${1:-all}" = all ] || [ "$1" = cli ]; then
  echo "================ cli ================"
  mkdir -p "$W/cli"; fakesys "$W/cli/fakesys" sdz21 2048
  $MT make "$W/cli/misc.img"; cp "$W/cli/misc.img" "$W/cli.ref"
  { cli show; cli clear; cli init; cli init; cli bump; cli bump; cli bump; cli show; cli clear; cli clear; cli show; } >"$W/cli.out" 2>&1
  sed 's/^/    /' "$W/cli.out"
  expect "$W/cli.out" "status=none slot=- seq=0 count=0 wrote=0" \
    "status=ok slot=A seq=1 count=0 wrote=1" "status=ok slot=A seq=1 count=0 wrote=0" \
    "status=ok slot=B seq=2 count=1 wrote=1" "status=ok slot=A seq=3 count=2 wrote=1" \
    "status=ok slot=B seq=4 count=3 wrote=1" "status=ok slot=A seq=5 count=0 wrote=1" \
    "status=ok slot=A seq=5 count=0 wrote=0"
  $MT check "$W/cli/misc.img" "$W/cli.ref" >/dev/null && ok "cli: nothing outside the slots changed" || bad "cli: outside changed"
  $MT make "$W/cli/misc.img" --seed A:4294967295:1; cli bump >"$W/cli-wrap.out"; cli show >>"$W/cli-wrap.out"
  sed 's/^/    /' "$W/cli-wrap.out"
  expect "$W/cli-wrap.out" "status=ok slot=B seq=0 count=2 wrote=1" "status=ok slot=B seq=0 count=2 wrote=0"
  $MT make "$W/cli/misc.img" --foreign B; cp "$W/cli/misc.img" "$W/cli-f.ref"; cli bump >"$W/cli-f.out"; rc=$?
  sed 's/^/    /' "$W/cli-f.out"
  [ $rc = 4 ] && ok "cli: foreign slot refused (exit 4)" || bad "cli: foreign exit $rc"
  $MT check "$W/cli/misc.img" "$W/cli-f.ref" | grep -q IDENTICAL && ok "cli: foreign image untouched" || bad "cli: foreign image modified"
fi
# ---------------- (c) device ----------------
if [ "${1:-all}" = all ] || [ "$1" = device ]; then
  echo "================ device (losetup) ================"
  if ! sudo -n true 2>/dev/null; then echo "  SKIP: no sudo"; else
    if grep -lx PARTNAME=misc /sys/class/block/*/uevent 2>/dev/null | grep -q .; then
      echo "  SKIP: this host already has a PARTNAME=misc partition"; else
      truncate -s 8M disk.img
      sgdisk -o -n1:2048:+2M -c1:boot -n2:0:+1M -c2:misc disk.img >/dev/null
      LOOP=$(sudo losetup -fP --show disk.img); PART=${LOOP}p2
      for i in 1 2 3 4 5 6 7 8 9 10; do [ -b "$PART" ] && break; sleep 0.3; done
      echo "  $LOOP, misc = $PART ($(cat /sys/class/block/$(basename $PART)/dev), $(cat /sys/class/block/$(basename $PART)/size) sectors)"
      if [ ! -b "$PART" ] || ! grep -qx PARTNAME=misc "/sys/class/block/$(basename "$PART")/uevent"; then
        bad "device: $PART is not a block device with PARTNAME=misc"; sudo losetup -d "$LOOP"
      else
      $MT make dev.ref; sudo dd if=dev.ref of="$PART" bs=4096 conv=fsync status=none
      { sudo ./oi-cli --bootcount show; sudo ./oi-cli --bootcount init; sudo ./oi-cli --bootcount bump; sudo ./oi-cli --bootcount bump; } >dev-cli.out 2>&1
      sed 's/^/    /' dev-cli.out
      expect dev-cli.out "status=none slot=- seq=0" "status=ok slot=A seq=1 count=0 wrote=1" "status=ok slot=B seq=2 count=1 wrote=1" "status=ok slot=A seq=3 count=2 wrote=1 slotA=1 slotB=1 direct=1"
      # The production helper path, as root, in a private mount+pid namespace.
      t="$W/t-dev"; mkdir -p "$t"/{proc,sys,dev,mnt}
      cp oi-dev "$t/init"; cp stub-magisk "$t/init.magisk"; cp stub-stock "$t/init.stock"
      chmod 750 "$t/init" "$t/init.magisk" "$t/init.stock"
      echo hang >"$t/stub-mode"; printf 'boot\n' >"$t/kmsg.test"   # not live yet: a count of 3 skips
      : >"$t/dev/kmsg"   # a plain file: oss-init logs into it, never into this host's kernel log
      sudo timeout -s KILL 8 unshare -mpf --kill-child=SIGKILL --propagation private \
        sh -c "mount --bind $t $t && cd $t && exec chroot . /init" >dev-helper.out 2>&1
      grep -aE "oss-init|\[ctr\] /init" dev-helper.out | sed 's/^/    /' | cut -c1-150
      expect dev-helper.out "status=ok count=3 found=0 wrote=1 froze=1" "consecutive failed boots reached the limit" "/init.stock:PRESENT"
      sudo dd if="$PART" of=dev.after bs=4096 status=none
      $MT check dev.after dev.ref | sed 's/^/    misc: /'
      $MT check dev.after dev.ref >/dev/null && ok "device: nothing outside 0x40000-0x41fff changed" || bad "device: outside changed"
      grep -q "slotB=VALID(seq=4,count=3)" <($MT show dev.after) && ok "device: helper wrote slot B seq 4 count 3" || bad "device: unexpected slots"
      sudo ./oi-cli --bootcount clear | sed 's/^/    /'
      sudo losetup -d "$LOOP"
      fi
    fi
  fi
fi
echo "================ bootcount bench: $PASS passed, $FAIL failed ================"
[ $FAIL = 0 ]
