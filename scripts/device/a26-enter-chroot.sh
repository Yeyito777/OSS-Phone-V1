#!/system/bin/sh
set -eu

# Run this as genuine Android uid 0 (for example, from a trusted root shell).
# It mounts only below A26_ROOT, enters the chroot, and unmounts on return.  It
# deliberately does not stop Android services, change SELinux, load modules, or
# attempt to take over the display.

A26_ROOT="${A26_ROOT:-/data/local/a26-linux}"

find_busybox() {
    if [ -n "${A26_BUSYBOX:-}" ] && [ -x "$A26_BUSYBOX" ]; then
        printf '%s\n' "$A26_BUSYBOX"
        return
    fi
    for candidate in \
        /data/adb/magisk/busybox \
        /data/adb/ksu/bin/busybox \
        /data/local/tmp/busybox.static \
        "$A26_ROOT/busybox.static" \
        "$A26_ROOT/../busybox.static"
    do
        if [ -x "$candidate" ]; then printf '%s\n' "$candidate"; return; fi
    done
    if command -v busybox >/dev/null 2>&1; then command -v busybox; return; fi
    return 1
}

BB="$(find_busybox || true)"
[ -n "$BB" ] || {
    echo "No BusyBox found. Set A26_BUSYBOX=/absolute/path/to/busybox.static." >&2
    exit 1
}
[ "$($BB id -u)" = 0 ] || { echo "A genuine uid-0 shell is required." >&2; exit 1; }
# /bin/sh is an absolute /bin/busybox symlink and resolves only after chroot.
[ -x "$A26_ROOT/bin/busybox" ] || { echo "Not an Alpine rootfs: $A26_ROOT" >&2; exit 1; }

MOUNTS=""
mounted() {
    $BB awk -v target="$1" '$2 == target { found=1 } END { exit !found }' \
        /proc/mounts 2>/dev/null
}
record_mount() { MOUNTS="$1 $MOUNTS"; }

bind_mount() {
    source_path="$1"
    target_path="$2"
    [ -e "$source_path" ] || return 0
    if [ -d "$source_path" ]; then
        $BB mkdir -p "$target_path"
    else
        $BB mkdir -p "${target_path%/*}"
        [ -e "$target_path" ] || : >"$target_path"
    fi
    if ! mounted "$target_path"; then
        $BB mount -o bind "$source_path" "$target_path"
        record_mount "$target_path"
    fi
}

tmpfs_mount() {
    target_path="$1"
    mode="$2"
    $BB mkdir -p "$target_path"
    if ! mounted "$target_path"; then
        $BB mount -t tmpfs -o "mode=$mode,nosuid,nodev" tmpfs "$target_path"
        record_mount "$target_path"
    fi
}

cleanup() {
    status=$?
    trap - EXIT HUP INT TERM
    if [ -n "${RUNTIME_RESOLV_NEW:-}" ]; then
        $BB rm -f "$RUNTIME_RESOLV_NEW"
    fi
    for target_path in $MOUNTS; do
        if mounted "$target_path"; then
            $BB umount "$target_path" || {
                echo "Unmount failed (something may still be using it): $target_path" >&2
                status=1
            }
        fi
    done
    exit "$status"
}
trap cleanup EXIT HUP INT TERM

# Plain bind mounts are intentional.  The few nested mounts needed by a chroot
# are bound explicitly, avoiding shared recursive-mount propagation surprises.
bind_mount /proc "$A26_ROOT/proc"
bind_mount /sys "$A26_ROOT/sys"
bind_mount /dev "$A26_ROOT/dev"
bind_mount /dev/pts "$A26_ROOT/dev/pts"
# /dev is a plain bind, so its nested controller mounts must be included
# explicitly. Moon assigns each app before exec so every descendant inherits
# the same freezer group without a post-spawn race.
if [ -d /dev/freezer ]; then bind_mount /dev/freezer "$A26_ROOT/dev/freezer"; fi
if [ -e /dev/binderfs ]; then bind_mount /dev/binderfs "$A26_ROOT/dev/binderfs"; fi
tmpfs_mount "$A26_ROOT/run" 0755
tmpfs_mount "$A26_ROOT/tmp" 1777

# The AudioTrack bridge must be authorized before system_server is suspended,
# so its FIFO lives outside this session's later-mounted /run tmpfs. Bind the
# stable device-local runtime into Alpine after /run exists. The mount is
# recorded here and therefore removed in the correct order during restoration.
if [ -d /data/local/tmp/moon-audio ]; then
    bind_mount /data/local/tmp/moon-audio "$A26_ROOT/run/moon-audio"
fi
# Deep suspend is supervised from Android's mount namespace because Xorg must be
# replaced after Samsung's DSI output has been disabled. Expose only its private
# state directory to Moon; the privileged helper itself remains outside chroot.
if [ -d /data/local/tmp/moon-suspend ]; then
    bind_mount /data/local/tmp/moon-suspend "$A26_ROOT/run/moon-suspend"
fi
# moon-telephonyd runs on the Android side (it talks to rild over hwbinder and
# execs the bionic call-audio helper); its socket lives in here.
$BB mkdir -p /data/local/tmp/moon-telephony
bind_mount /data/local/tmp/moon-telephony "$A26_ROOT/run/moon-telephony"
# GPU mode: moon-compositor (Android side) listens on a socket in here.
if [ -d /data/local/tmp/moon-gpu ]; then
    bind_mount /data/local/tmp/moon-gpu "$A26_ROOT/run/moon-gpu"
fi

# Android commonly has no useful /etc/resolv.conf.  Prefer any actual host
# nameserver, then legacy net.dns properties, then explicit public fallbacks.
RUNTIME_RESOLV="$A26_ROOT/run/a26-resolv.conf"
RUNTIME_RESOLV_NEW="$RUNTIME_RESOLV.new.$$"
: >"$RUNTIME_RESOLV_NEW"
if [ -n "${A26_DNS:-}" ]; then
    for ns in $A26_DNS; do echo "nameserver $ns" >>"$RUNTIME_RESOLV_NEW"; done
elif [ -r /etc/resolv.conf ]; then
    $BB awk '/^[[:space:]]*nameserver[[:space:]]+/ { print; found=1 } END { exit !found }' \
        /etc/resolv.conf >"$RUNTIME_RESOLV_NEW" 2>/dev/null || :
fi
if ! $BB grep -q '^nameserver ' "$RUNTIME_RESOLV_NEW"; then
    for prop in net.dns1 net.dns2; do
        ns="$(/system/bin/getprop "$prop" 2>/dev/null || true)"
        [ -n "$ns" ] && echo "nameserver $ns" >>"$RUNTIME_RESOLV_NEW"
    done
fi
if ! $BB grep -q '^nameserver ' "$RUNTIME_RESOLV_NEW"; then
    echo 'nameserver 1.1.1.1' >>"$RUNTIME_RESOLV_NEW"
    echo 'nameserver 8.8.8.8' >>"$RUNTIME_RESOLV_NEW"
fi
echo 'options timeout:2 attempts:2' >>"$RUNTIME_RESOLV_NEW"
$BB chmod 0644 "$RUNTIME_RESOLV_NEW"
$BB mv -f "$RUNTIME_RESOLV_NEW" "$RUNTIME_RESOLV"
bind_mount "$RUNTIME_RESOLV" "$A26_ROOT/etc/resolv.conf"

TERM_VALUE="${TERM:-xterm-256color}"
if [ "$#" -gt 0 ]; then
    $BB chroot "$A26_ROOT" /usr/bin/env -i \
        HOME=/root USER=root LOGNAME=root SHELL=/bin/bash TERM="$TERM_VALUE" \
        PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
        "$@"
else
    $BB chroot "$A26_ROOT" /usr/bin/env -i \
        HOME=/root USER=root LOGNAME=root SHELL=/bin/bash TERM="$TERM_VALUE" \
        PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
        /bin/bash --login
fi
