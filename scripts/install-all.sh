#!/usr/bin/env bash
# Provisions the attached phone with everything OSS-Phone-V1 installs on
# /data, from this repository (docs/INSTALL.md):
#
#   scripts/install-all.sh [--dry-run] [--a26-system DIR] [--reset-pin]
#                          [--skip-system] [--skip-exocortex] [--reboot]
#
# Steps, each idempotent (atomic replace; a second run changes nothing):
#   1. preflight: root, the Alpine rootfs, Magisk, the direct-boot ramdisk
#   2. the System app and native Wi-Fi backend (a26-system's installer)
#   3. Moon and moon-notifyd; the PIN config only if missing (or --reset-pin,
#      reading A26_SHELL_PIN from the Mac `secrets` store, never argv)
#   4. native audio
#   5. the autostart/direct-boot scripts, the GPU display path, the power
#      entry point, /data/adb/moon/config (created or migrated)
#   6. telephony
#   7. Exocortex: its keeper and daemon (scripts/exocortex/install.sh,
#      docs/EXOCORTEX.md); credentials are not touched here
#   8. verification: every installed file against its build, by sha256
#
# --dry-run builds everything and runs only 1 and 8: what would change, with
# nothing written to the phone. The boot image is never flashed here (see
# scripts/direct-boot/README.md); the Wi-Fi networks and the time zone keep
# whatever the phone has (System app; scripts/a26-shell/set-timezone.sh).
set -euo pipefail

PROJECT_ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
source "$PROJECT_ROOT/scripts/a26-shell/common.sh"

DRY_RUN=0
RESET_PIN=0
SKIP_SYSTEM=0
SKIP_EXOCORTEX=0
REBOOT=0
A26_SYSTEM="${A26_SYSTEM_DIR:-}"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --reset-pin) RESET_PIN=1 ;;
        --skip-system) SKIP_SYSTEM=1 ;;
        --skip-exocortex) SKIP_EXOCORTEX=1 ;;
        --reboot) REBOOT=1 ;;
        --a26-system) A26_SYSTEM="${2:?--a26-system needs a directory}"; shift ;;
        -h|--help) sed -n '2,26p' "$0"; exit 0 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done
if [[ -z "$A26_SYSTEM" ]]; then
    for candidate in "$PROJECT_ROOT/../a26-system" "$HOME/Desktop/a26-system"; do
        [[ -x "$candidate/scripts/install.sh" ]] && A26_SYSTEM="$(cd "$candidate" && pwd)" && break
    done
fi

SERIAL="$(a26_resolve_serial)"
ROOT=/data/local/a26-linux
PERSIST=/data/adb/moon
step() { printf '\n== %s\n' "$*"; }
su_run() { adb -s "$SERIAL" shell "/data/local/tmp/su -c '$1'" | tr -d '\r'; }

# --- 1. preflight -----------------------------------------------------------
step "preflight ($SERIAL)"
[[ "$(su_run "id -u")" == 0 ]] || { echo "no root through /data/local/tmp/su" >&2; exit 1; }
missing=0
for path in \
    "$ROOT/busybox.static" "$ROOT/etc/alpine-release" "$ROOT/usr/libexec/Xorg" \
    "$ROOT/usr/bin/python3" "$ROOT/etc/X11/xorg.conf.d" /data/adb/magisk
do
    if [[ "$(su_run "[ -e $path ] && echo yes")" != yes ]]; then
        echo "missing on the phone: $path" >&2
        missing=1
    fi
done
[[ "$missing" == 0 ]] || { echo "the reference rootfs/Magisk setup is incomplete (docs/INSTALL.md)" >&2; exit 1; }
echo "rootfs: Alpine $(su_run "cat $ROOT/etc/alpine-release")"
if [[ "$(su_run '[ -e /debug_ramdisk/moon-boot-mode.sh ] && echo yes')" == yes ]]; then
    echo "direct-boot ramdisk: installed"
else
    echo "direct-boot ramdisk: NOT installed (legacy post-boot takeover only; scripts/direct-boot/README.md)"
fi
config_present="$(su_run "[ -s $ROOT/etc/a26-shell/config.json ] && echo yes")"
echo "PIN config: ${config_present:-missing}"

# --- builds (also for --dry-run) ---------------------------------------------
# Without a local Android NDK / musl cross toolchain the native parts build
# on A26_BUILD_HOST (default whale), which has both.
step build
BUILD_HOST="${A26_BUILD_HOST:-whale}"
HOST_NDK='$HOME/Workspace/tools-cache/android-ndk-r27c'
"$PROJECT_ROOT/scripts/a26-shell/build.sh"
NDK_HOST="$BUILD_HOST" "$PROJECT_ROOT/scripts/a26-shell/build-telephony.sh"
if [[ -d "${NDK:-/nonexistent}/toolchains/llvm/prebuilt" ]]; then
    "$PROJECT_ROOT/scripts/a26-shell/build-audiod.sh"
    "$PROJECT_ROOT/scripts/gpu/build.sh"
else
    MOON_AUDIOD_BUILD_HOST="$BUILD_HOST" "$PROJECT_ROOT/scripts/a26-shell/build-audiod.sh"
    NDK="$HOST_NDK" MOON_GPU_BUILD_HOST="$BUILD_HOST" "$PROJECT_ROOT/scripts/gpu/build.sh"
fi
SYSTEM_PREBUILT=0
if [[ "$SKIP_SYSTEM" == 0 && -n "$A26_SYSTEM" ]]; then
    if command -v aarch64-linux-musl-gcc >/dev/null && command -v readelf >/dev/null; then
        "$A26_SYSTEM/scripts/build.sh"
    else
        remote=.cache/a26-system-build
        ssh "$BUILD_HOST" "mkdir -p $remote"
        # Content, not times: a source older than the host's last build (another
        # branch's) must still count as changed, or cargo keeps the stale binary.
        rsync -rlpD --checksum --delete --exclude target --exclude .git "$A26_SYSTEM/" "$BUILD_HOST:$remote/"
        ssh "$BUILD_HOST" "cd $remote && scripts/build.sh >/dev/null"
        mkdir -p "$A26_SYSTEM/target/aarch64-unknown-linux-musl/release"
        for name in a26-system a26-wifi-service a26-wifi-ctl a26-systemctl exocortex-notify; do
            rsync -a "$BUILD_HOST:$remote/target/aarch64-unknown-linux-musl/release/$name" \
                "$A26_SYSTEM/target/aarch64-unknown-linux-musl/release/$name"
        done
        for icon in system-app exocortex-app; do
            rsync -a "$BUILD_HOST:$remote/assets/$icon.bgrx" "$A26_SYSTEM/assets/$icon.bgrx"
        done
    fi
    SYSTEM_PREBUILT=1
fi

# local artifact -> phone path
IMAGE="$PROJECT_ROOT/images"
MANIFEST=()
for name in a26-shell a26-shellctl a26-shellshot a26-renderbench a26-touchpath moon-notifyd moon-notifyctl; do
    MANIFEST+=("$IMAGE/a26-shell-0.1.0/bin/$name:$ROOT/opt/a26-shell/bin/$name")
done
MANIFEST+=("$PROJECT_ROOT/scripts/device/moon-power:$ROOT/opt/a26-shell/bin/moon-power")
for name in moon-audiod moon-permissiond; do
    MANIFEST+=("$IMAGE/moon-audiod/$name:$ROOT/opt/a26-audio/$name")
done
for name in moon-boot-supervisor.sh moon-direct-policy.sh moon-post-ready.sh moon-audio-start.sh \
    moon-audio-stop.sh moon-audio-keeper.sh moon-gpu-session.sh moon-suspend-cycle.sh \
    moon-mode-stop.list moon-power.sh moon-boot-confirm.sh moon-telephony-start.sh \
    moon-telephony-stop.sh; do
    MANIFEST+=("$PROJECT_ROOT/scripts/device/$name:$PERSIST/$name")
done
MANIFEST+=(
    "$PROJECT_ROOT/scripts/device/90-moon-autostart.sh:/data/adb/service.d/90-moon-autostart.sh"
    "$PROJECT_ROOT/scripts/device/a26-enter-chroot.sh:$ROOT/a26-enter-chroot.sh"
    "$PROJECT_ROOT/scripts/device/a26-xorg-takeover.sh:/data/local/tmp/a26-xorg-takeover.sh"
    "$PROJECT_ROOT/scripts/device/a26-xorg-safety-watchdog.sh:/data/local/tmp/a26-xorg-safety-watchdog.sh"
    "$PROJECT_ROOT/scripts/device/a26-android-graphics-restore.sh:/data/local/tmp/a26-android-graphics-restore.sh"
    "$PROJECT_ROOT/scripts/device/a26-xorg-persistent-session.sh:$ROOT/usr/local/sbin/a26-xorg-persistent-session"
    "$IMAGE/moon-gpu/moon-compositor:$ROOT/opt/moon-gpu/moon-compositor"
    "$PROJECT_ROOT/scripts/gpu/xorg/20-a26-gpu.conf:$ROOT/etc/X11/xorg-gpu.conf.d/20-a26-gpu.conf"
)
for name in moon-telephonyd moon-telephonyctl moon-call-audio mobile-data; do
    MANIFEST+=("$IMAGE/moon-telephony/$name:$ROOT/opt/moon-telephony/$name")
done
if [[ "$SKIP_EXOCORTEX" == 0 ]]; then
    EXO="$PROJECT_ROOT/scripts/device/exocortex"
    MANIFEST+=(
        "$EXO/exocortex-keeper.sh:/data/adb/exocortex/exocortex-keeper.sh"
        "$EXO/exocortex-ns.sh:/data/adb/exocortex/exocortex-ns.sh"
        "$EXO/95-exocortex.sh:/data/adb/service.d/95-exocortex.sh"
        "$EXO/exocortex-supervisor:$ROOT/usr/local/bin/exocortex-supervisor"
        "$EXO/exocortexd:$ROOT/usr/local/bin/exocortexd"
        "$EXO/android-sh:$ROOT/usr/local/bin/android-sh"
    )
fi
if [[ "$SKIP_SYSTEM" == 0 && -n "$A26_SYSTEM" ]]; then
    # The System, Phone, Messages and Exocortex apps (one binary), the Wi-Fi
    # backend, and the Exocortex notification bridge.
    for name in a26-system a26-wifi-service a26-wifi-ctl a26-systemctl exocortex-notify; do
        MANIFEST+=("$A26_SYSTEM/target/aarch64-unknown-linux-musl/release/$name:$ROOT/opt/a26-system/bin/$name")
    done
    MANIFEST+=("$A26_SYSTEM/assets/exocortex-app.bgrx:$ROOT/opt/a26-system/share/exocortex-app.bgrx")
    for name in a26-wifi-start a26-wifi-stop a26-udhcpc.script; do
        MANIFEST+=("$A26_SYSTEM/scripts/phone/$name:$ROOT/opt/a26-system/libexec/$name")
    done
fi

# Prints "same|DIFF|MISSING path" per manifest entry; returns the count of
# entries that differ.
verify() {
    local remote_paths=() entry
    for entry in "${MANIFEST[@]}"; do remote_paths+=("${entry#*:}"); done
    local phone
    phone="$(su_run "sha256sum ${remote_paths[*]} 2>/dev/null")"
    local differ=0 local_path remote_path want have
    for entry in "${MANIFEST[@]}"; do
        local_path="${entry%%:*}"
        remote_path="${entry#*:}"
        want="$(shasum -a 256 "$local_path" | cut -d' ' -f1)"
        have="$(awk -v p="$remote_path" '$2 == p {print $1}' <<<"$phone")"
        if [[ -z "$have" ]]; then
            echo "MISSING $remote_path"
            differ=$((differ + 1))
        elif [[ "$have" != "$want" ]]; then
            echo "DIFF    $remote_path"
            differ=$((differ + 1))
        else
            echo "same    $remote_path"
        fi
    done
    local config
    config="$(su_run "cat $PERSIST/config 2>/dev/null")"
    # DIRECT_BOOT, GPU_COMPOSITOR and the like are the owner's choices; only
    # keys that install-autostart.sh migrates away count as out of date.
    for key in LOW_BATTERY_FALLBACK MIN_START_BATTERY=20; do
        grep -q "^$key" <<<"$config" && { echo "CONFIG  $PERSIST/config has the old $key"; differ=$((differ + 1)); }
    done
    return "$differ"
}

if [[ "$DRY_RUN" == 1 ]]; then
    step "dry run: what would change (nothing is written)"
    if verify; then
        echo "up to date: an install would change nothing"
    else
        echo "$? item(s) would change"
    fi
    [[ -n "$A26_SYSTEM" || "$SKIP_SYSTEM" == 1 ]] ||
        echo "note: a26-system not found (--a26-system DIR); the System app was not checked"
    [[ -n "$config_present" || "$RESET_PIN" == 1 ]] ||
        echo "note: no PIN config on the phone; an install would create one from A26_SHELL_PIN"
    exit 0
fi

# --- 2. System app and Wi-Fi backend -----------------------------------------
if [[ "$SKIP_SYSTEM" == 0 ]]; then
    step "System app and Wi-Fi backend (a26-system)"
    [[ -n "$A26_SYSTEM" ]] || { echo "a26-system checkout not found: --a26-system DIR or --skip-system" >&2; exit 1; }
    A26_SYSTEM_PREBUILT="$SYSTEM_PREBUILT" "$A26_SYSTEM/scripts/install.sh"
fi

# --- 3. Moon -----------------------------------------------------------------
step "Moon"
if [[ -z "$config_present" || "$RESET_PIN" == 1 ]]; then
    echo "creating the PIN config from the secrets store (A26_SHELL_PIN)"
    secrets run --allow A26_SHELL_PIN -- "$PROJECT_ROOT/scripts/a26-shell/install.sh"
fi
"$PROJECT_ROOT/scripts/a26-shell/deploy.sh"

# --- 4-6. audio, autostart (GPU, power, config), telephony ---------------------
step "native audio"
"$PROJECT_ROOT/scripts/a26-shell/install-audio.sh"
step "autostart, direct-boot policy, GPU display path, power entry point"
"$PROJECT_ROOT/scripts/a26-shell/install-autostart.sh"
step telephony
"$PROJECT_ROOT/scripts/a26-shell/install-telephony.sh"

# --- 7. Exocortex ------------------------------------------------------------
# The keeper and an updated daemon; the OpenAI key and whale's authorization
# are separate, one-time steps (docs/EXOCORTEX.md).
if [[ "$SKIP_EXOCORTEX" == 0 ]]; then
    step "Exocortex"
    "$PROJECT_ROOT/scripts/exocortex/install.sh"
fi

# --- 8. verification ---------------------------------------------------------
step verification
if verify; then
    echo "every installed file matches its build"
else
    echo "verification found $? difference(s)" >&2
    exit 1
fi
if [[ "$REBOOT" == 1 ]]; then
    # moon-power stops Moon's services and reboots normally; adb drops when it
    # does, which is the success signal. Only a phone that never goes away
    # gets a plain adb reboot.
    old_boot="$(su_run 'cat /proc/sys/kernel/random/boot_id')"
    su_run "$ROOT/busybox.static chroot $ROOT /opt/a26-shell/bin/moon-power reboot" || true
    gone=0
    for _ in $(seq 1 90); do
        adb -s "$SERIAL" get-state >/dev/null 2>&1 || { gone=1; break; }
        sleep 1
    done
    if [[ "$gone" == 0 ]]; then
        echo "moon-power did not reboot the phone within 90 s; adb reboot" >&2
        adb -s "$SERIAL" reboot
    fi
    echo "rebooting; waiting for Moon"
    ready=0
    for _ in $(seq 1 60); do
        sleep 5
        adb -s "$SERIAL" get-state >/dev/null 2>&1 || continue
        [[ "$(su_run 'cat /proc/sys/kernel/random/boot_id' 2>/dev/null)" != "$old_boot" ]] || continue
        # Direct boots report moon.session.ready; a legacy takeover does not,
        # so Moon's control socket counts too.
        if [[ "$(su_run "getprop moon.session.ready; [ -S $ROOT/run/a26-shell/control.sock ] && echo socket" 2>/dev/null)" == *[1t]* ]]; then
            ready=1
            break
        fi
    done
    if [[ "$ready" == 1 ]]; then
        echo "Moon is up after the reboot"
    else
        echo "the phone did not report Moon ready within 5 minutes of the reboot" >&2
        exit 1
    fi
else
    echo "installed; changes take effect at the next boot (or: --reboot)"
fi
