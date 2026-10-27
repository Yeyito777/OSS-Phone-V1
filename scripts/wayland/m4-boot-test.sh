#!/bin/sh
# The merge gate for milestone 4 (xwayland-m4): three real boots through the
# supervisor, then everything put back. Run from the Mac with the phone on
# adb, in Moon, after `moon-phone.sh xwayland-install`. About 15 minutes.
#
#   scripts/wayland/m4-boot-test.sh [--keep]
#
#   0. Preflight: Moon ready, battery >= 40 %, Xwayland in the chroot, and
#      the phone's boot scripts and Moon are main's (by sha256, refuses
#      otherwise). Everything this changes is backed up on the phone and
#      pulled to build/wayland/m4-boot-test-TIME/.
#   1. Installs this branch's four changed boot scripts (the same paths and
#      modes as scripts/gpu/install.sh) and Moon (moon-phone.sh install).
#   2. Boot A, XWAYLAND=1: the session must be Moon's Xwayland (no Xorg,
#      Xwayland on :0, Moon answering).
#   3. Boot B, XWAYLAND=1 with XWAYLAND_BIN=/bin/false: Moon cannot start on
#      Xwayland; the supervisor must fall back to headless Xorg in the same
#      boot (Moon answering on Xorg, the fallback in boot.log).
#   4. Boot C, XWAYLAND unset: the GPU session with headless Xorg, as main.
#   5. Restore: the config, boot scripts and Moon from the backup (unless
#      --keep leaves this branch's scripts and Moon installed), and a last
#      boot that must come up in Moon with main's files (by sha256).
#
# Any failed check stops the run after restoring. If a boot does not come
# back at all, `adb reboot` falls back like any boot (Xorg, or Android at
# worst); the backup is on the phone in /data/local/tmp/m4-backup.
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
SERIAL="${A26_SERIAL:-R5CY323ES7X}"
KEEP=0
[ "${1:-}" != --keep ] || KEEP=1
OUT="$REPO/build/wayland/m4-boot-test-$(date +%Y%m%d-%H%M%S)"
mkdir -p "$OUT"
ROOT=/data/local/a26-linux
BACKUP=/data/local/tmp/m4-backup
adb() { command adb -s "$SERIAL" "$@"; }
su_sh() { adb shell "su -c '$*'"; }
log() { echo "[$(date +%H:%M:%S)] $*" | tee -a "$OUT/run.log"; }
die() { log "FAILED: $*"; restore; exit 1; }

# name  device path  mode
FILES="moon-boot-supervisor.sh:/data/adb/moon/moon-boot-supervisor.sh:0700
moon-gpu-session.sh:/data/adb/moon/moon-gpu-session.sh:0700
a26-xorg-takeover.sh:/data/local/tmp/a26-xorg-takeover.sh:0755
a26-xorg-persistent-session.sh:$ROOT/usr/local/sbin/a26-xorg-persistent-session:0755"

remote_sha() { su_sh "sha256sum $1 2>/dev/null" | cut -d' ' -f1; }

push_file() { # LOCAL DEVICE-PATH MODE
    adb push -q "$1" /data/local/tmp/m4-push >/dev/null
    su_sh "cp /data/local/tmp/m4-push $2.new && chown 0:0 $2.new && chmod $3 $2.new && mv -f $2.new $2 && rm -f /data/local/tmp/m4-push"
}

set_config() { # KEY VALUE (empty VALUE: remove the key)
    su_sh "sed -i \"/^$1=/d\" /data/adb/moon/config"
    [ -z "$2" ] || su_sh "echo $1=$2 >>/data/adb/moon/config"
}

wait_boot() { # after adb reboot: Moon ready, or fail after 240 s
    sleep 15
    adb wait-for-device
    for _ in $(seq 1 110); do
        [ "$(adb shell getprop moon.session.ready 2>/dev/null | tr -d '\r')" = 1 ] && return 0
        sleep 2
    done
    return 1
}

snapshot() { # LABEL: what the session is, and the logs
    su_sh "echo mode=\$(getprop moon.boot.mode) ready=\$(getprop moon.session.ready) zygote=\$(getprop init.svc.zygote) xorg=\$(pidof Xorg) xwayland=\$(pidof Xwayland) moon=\$(pidof a26-shell) compositor=\$(pidof moon-compositor)" | tee "$OUT/$1.state" | tee -a "$OUT/run.log"
    su_sh "cat $ROOT/root/xorg-session.status" >"$OUT/$1.xorg-session.status" 2>&1 || true
    su_sh "tail -n 150 /data/adb/moon/boot.log" >"$OUT/$1.boot.log" 2>&1 || true
    su_sh "cat /data/local/tmp/a26-xorg-control/takeover.log" >"$OUT/$1.takeover.log" 2>&1 || true
    su_sh "cat $ROOT/root/a26-shell.log" >"$OUT/$1.moon.log" 2>&1 || true
}

moon_ping() {
    su_sh "$ROOT/busybox.static timeout -s KILL 5 $ROOT/busybox.static chroot $ROOT /opt/a26-shell/bin/a26-shellctl ping" 2>/dev/null | grep -q '"ok":true'
}

RESTORED=0
restore() {
    [ "$RESTORED" = 0 ] || return 0
    RESTORED=1
    if [ "$KEEP" = 1 ]; then log "restoring the config (--keep: this branch's scripts and Moon stay)"; else log "restoring the config, boot scripts and Moon"; fi
    su_sh "cp -p $BACKUP/config /data/adb/moon/config" || true
    if [ "$KEEP" = 0 ]; then
        echo "$FILES" | while IFS=: read -r name path mode; do
            su_sh "cp -p $BACKUP/$name $path.new && mv -f $path.new $path" || true
        done
        su_sh "cp -p $BACKUP/a26-shell $ROOT/opt/a26-shell/bin/a26-shell.new && mv -f $ROOT/opt/a26-shell/bin/a26-shell.new $ROOT/opt/a26-shell/bin/a26-shell && rm -f $ROOT/opt/a26-shell/bin/a26-shell.before-wayland $ROOT/opt/a26-system/bin/a26-system-wayland" || true
    fi
    log "last boot"
    adb reboot || true
    if wait_boot; then
        snapshot restored
        log "restored: Moon $(remote_sha $ROOT/opt/a26-shell/bin/a26-shell | cut -c1-12), supervisor $(remote_sha /data/adb/moon/moon-boot-supervisor.sh | cut -c1-12)"
    else
        log "the last boot did not report Moon ready; backup in $BACKUP"
    fi
}

# 0. Preflight.
[ "$(adb shell getprop moon.session.ready | tr -d '\r')" = 1 ] || { echo "Moon is not ready" >&2; exit 1; }
battery="$(adb shell cat /sys/class/power_supply/battery/capacity | tr -d '\r')"
[ "$battery" -ge 40 ] || { echo "battery $battery % (needs 40)" >&2; exit 1; }
su_sh "test -x $ROOT/usr/bin/Xwayland" || { echo "no Xwayland: moon-phone.sh xwayland-install" >&2; exit 1; }
git -C "$REPO" fetch -q origin main
echo "$FILES" | while IFS=: read -r name path mode; do
    want="$(git -C "$REPO" show "origin/main:scripts/device/$name" | shasum -a 256 | cut -d' ' -f1)"
    have="$(remote_sha "$path")"
    [ "$want" = "$have" ] || { echo "$path is not main's ($have); refusing" >&2; exit 1; }
done
log "preflight ok: battery $battery %, boot scripts are main's"
su_sh "rm -rf $BACKUP && mkdir -p $BACKUP && cp -p /data/adb/moon/config $BACKUP/config && cp -p $ROOT/opt/a26-shell/bin/a26-shell $BACKUP/a26-shell"
echo "$FILES" | while IFS=: read -r name path mode; do su_sh "cp -p $path $BACKUP/$name"; done
for f in config a26-shell; do su_sh "cat $BACKUP/$f" >"$OUT/backup-$f"; done
log "backed up to $BACKUP (and $OUT)"
snapshot before

# 1. Install.
echo "$FILES" | while IFS=: read -r name path mode; do
    sh -n "$REPO/scripts/device/$name"
    push_file "$REPO/scripts/device/$name" "$path" "$mode"
done
sh "$HERE/moon-phone.sh" install >>"$OUT/run.log" 2>&1
log "installed this branch's boot scripts and Moon $(remote_sha $ROOT/opt/a26-shell/bin/a26-shell | cut -c1-12)"

# 2. Boot A: Moon's Xwayland.
set_config XWAYLAND 1
set_config XWAYLAND_BIN ''
log "boot A: XWAYLAND=1"
adb reboot
wait_boot || die "boot A: Moon not ready"
snapshot A
grep -qx 'XSERVER=xwayland' "$OUT/A.xorg-session.status" || die "boot A: the session is not Xwayland's"
grep -q 'xwayland: display :0 ready' "$OUT/A.moon.log" || die "boot A: Moon did not start Xwayland"
grep -q 'xorg=$\|xorg= ' "$OUT/A.state" || die "boot A: Xorg is running"
moon_ping || die "boot A: Moon does not answer"
log "boot A ok: Moon on Xwayland, no Xorg"

# 3. Boot B: a broken Xwayland, the fallback to Xorg.
set_config XWAYLAND_BIN /bin/false
log "boot B: XWAYLAND=1, XWAYLAND_BIN=/bin/false"
adb reboot
wait_boot || die "boot B: Moon not ready (the fallback failed)"
snapshot B
grep -q 'falling back to headless Xorg' "$OUT/B.boot.log" || die "boot B: no fallback in boot.log"
grep -q 'xorg=[0-9]' "$OUT/B.state" || die "boot B: no Xorg after the fallback"
grep -q 'compositor=[0-9]' "$OUT/B.state" || die "boot B: the compositor is gone"
moon_ping || die "boot B: Moon does not answer on Xorg"
log "boot B ok: fell back to headless Xorg, Moon answers"

# 4. Boot C: XWAYLAND unset.
set_config XWAYLAND ''
set_config XWAYLAND_BIN ''
log "boot C: XWAYLAND unset"
adb reboot
wait_boot || die "boot C: Moon not ready"
snapshot C
grep -qx 'XSERVER=xwayland' "$OUT/C.xorg-session.status" && die "boot C: still Xwayland"
grep -q 'xorg=[0-9]' "$OUT/C.state" || die "boot C: no Xorg"
moon_ping || die "boot C: Moon does not answer"
log "boot C ok: the GPU session with headless Xorg, as main"

# 5. Restore.
restore
log "all checks passed; logs in $OUT"
