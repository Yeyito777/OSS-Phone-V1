#!/system/bin/sh
# Runs one Exocortex process in the chroot, inside the private mount
# namespace exocortex-keeper.sh made for it (unshare -m):
#
#   exocortex-ns.sh daemon    the Exocortex daemon (run-daemon.sh)
#   exocortex-ns.sh notify    exocortex-notify, the moon-notifyd bridge
#   exocortex-ns.sh exec CMD  CMD in the same environment (installs, debugging):
#                             busybox unshare -m --propagation private \
#                                 sh exocortex-ns.sh exec bash -l
#
# The chroot's mounts here are its own: whatever Moon's session mounted under
# the rootfs (copied into this namespace by unshare) is dropped and /proc,
# /sys, /dev and /tmp are mounted afresh, so a Moon, Xorg or
# chroot-session teardown never unmounts them under the daemon. Moon's live
# sockets are reached through /proc/1/root (Android's mount namespace, where
# Moon's session mounts live), from /run symlinks that follow Moon restarts.
# The daemon's own socket is on /data (/opt/exocortex/config/runtime), the
# same file in every namespace, so Moon's apps reach it as usual.
set -eu

ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
STATE="$ROOT/var/lib/exocortex-keeper"
ROLE="${1:?daemon|notify|exec}"
LIVE=/proc/1/root

# The PID survives every exec below; the keeper tracks it.
case "$ROLE" in daemon|notify) echo $$ >"$STATE/$ROLE.pid" ;; esac
[ -s "$STATE/resolv.conf" ] || /system/bin/sh /data/adb/exocortex/exocortex-keeper.sh resolv

# 1. Drop the copies of Moon's session mounts, deepest first.
for target in $("$BB" awk -v root="$ROOT/" 'index($2, root) == 1 { print $2 }' /proc/self/mounts | "$BB" sort -r); do
    "$BB" umount -l "$target" 2>/dev/null || true
done

# 2. Our own.
# Recursive binds (with /dev/pts, Moon's /dev/freezer and the rest): this
# namespace is private, so nothing propagates back and nothing here is ever
# unmounted under anyone. /proc and /dev are essential; the daemon runs
# without /sys.
"$BB" mount -t proc proc "$ROOT/proc"
"$BB" mount -o rbind /dev "$ROOT/dev"
"$BB" mount -o rbind /sys "$ROOT/sys" || echo "exocortex-ns: no /sys" >&2
"$BB" mount -t tmpfs -o mode=1777,nosuid,nodev tmpfs "$ROOT/tmp"
"$BB" mount -t tmpfs -o mode=0755,nosuid,nodev tmpfs "$ROOT/run"
[ -e "$ROOT/etc/resolv.conf" ] || : >"$ROOT/etc/resolv.conf"
"$BB" mount -o bind "$STATE/resolv.conf" "$ROOT/etc/resolv.conf"

# 3. Moon's runtime, live. Its session /run (a tmpfs Moon's chroot session
# mounts) as a whole, and each socket directory where clients expect it.
"$BB" ln -s "$LIVE$ROOT/run" "$ROOT/run/moon-session"
for name in a26-shell moon-notify a26-wifi moon-wayland; do
    "$BB" ln -s "$LIVE$ROOT/run/$name" "$ROOT/run/$name"
done
for name in moon-audio moon-suspend moon-telephony moon-gpu; do
    "$BB" ln -s "$LIVE/data/local/tmp/$name" "$ROOT/run/$name"
done
"$BB" ln -s "$LIVE$ROOT/tmp/.X11-unix" "$ROOT/tmp/.X11-unix"
mkdir -p "$ROOT/run/exocortex"

ENV="HOME=/root USER=root LOGNAME=root SHELL=/bin/bash LANG=C.UTF-8 TERM=xterm-256color"
ENV="$ENV PATH=/usr/local/sbin:/usr/local/bin:/opt/bun/bin:/root/.local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
# The keeper is the daemon's service manager (Exocortex README,
# EXOCORTEX_SUPERVISOR). Claude Code refuses to bypass permissions as root
# unless told it is in a deliberate sandbox (IS_SANDBOX=1): this phone is the
# agent's own machine, root by design (docs/EXOCORTEX.md).
ENV="$ENV EXOCORTEX_SUPERVISOR=/usr/local/bin/exocortex-supervisor IS_SANDBOX=1 DISPLAY=:0"

case "$ROLE" in
    daemon)
        echo -200 >/proc/$$/oom_score_adj 2>/dev/null || true
        # shellcheck disable=SC2086
        exec "$BB" chroot "$ROOT" /usr/bin/env -i $ENV /opt/exocortex/scripts/setup/run-daemon.sh
        ;;
    notify)
        # shellcheck disable=SC2086
        exec "$BB" chroot "$ROOT" /usr/bin/env -i $ENV \
            EXOCORTEX_SOCKET=/opt/exocortex/config/runtime/exocortexd.sock \
            /opt/a26-system/bin/exocortex-notify
        ;;
    exec)
        shift
        # shellcheck disable=SC2086
        exec "$BB" chroot "$ROOT" /usr/bin/env -i $ENV "$@"
        ;;
    *) echo "usage: $0 daemon|notify|exec CMD..." >&2; exit 2 ;;
esac
