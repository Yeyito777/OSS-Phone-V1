#!/system/bin/sh
# Keeps Moon's native audio (moon-permissiond + moon-audiod) running without
# ever holding up Moon. The supervisor starts this detached alongside the Xorg
# takeover and never waits for it: Moon comes up without sound if audio is
# slow, and gains it when this succeeds. Each start attempt is bounded; a
# failed or dead daemon is retried with backoff for as long as Moon owns the
# display (framework-suspended marker). The Android restore path stops it
# through moon-audio-stop.sh before the framework starts.
set -u

ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
PERSIST=/data/adb/moon
RUNTIME=/data/local/tmp/moon-audio
MARKER=/data/local/tmp/a26-xorg-control/framework-suspended
LOG="$PERSIST/boot.log"
AUDIOD="$ROOT/opt/a26-audio/moon-audiod"
START_TIMEOUT=40

mkdir -p "$RUNTIME"
exec </dev/null >>"$LOG" 2>&1
echo $$ >"$RUNTIME/keeper.pid"

log() {
    echo "moon-audio: $* [$(date --iso-8601=seconds 2>/dev/null || date) up=$(cut -d' ' -f1 /proc/uptime)]"
}

audiod_running() {
    pid="$(cat "$RUNTIME/bridge.pid" 2>/dev/null || true)"
    case "$pid" in ''|*[!0-9]*) return 1 ;; esac
    [ "$(readlink "/proc/$pid/exe" 2>/dev/null)" = "$AUDIOD" ]
}

# The permission stand-in may only register once the framework is gone: wait
# for the takeover to mark the session and for zygote to be stopped.
for _ in $($BB seq 1 600); do
    [ -e "$MARKER" ] && [ "$(getprop init.svc.zygote)" != running ] && break
    "$BB" sleep 0.2
done
[ -e "$MARKER" ] || { log 'Moon never took the display; not starting audio'; exit 0; }

# /data/adb/moon/volume mirrors Moon's live volume file (also kept on /data),
# so the value that seeds a missing live file is never stale.
sync_volume() {
    live="$(cat "$RUNTIME/volume" 2>/dev/null || true)"
    case "$live" in ''|*[!0-9]*) return ;; esac
    [ "$live" -le 100 ] || return
    [ "$(cat "$PERSIST/volume" 2>/dev/null)" = "$live" ] && return
    printf '%s\n' "$live" >"$PERSIST/volume.new.$$" &&
        chmod 0600 "$PERSIST/volume.new.$$" &&
        mv -f "$PERSIST/volume.new.$$" "$PERSIST/volume"
}

attempt=0
delay=2
while [ -e "$MARKER" ] && [ "$(getprop moon.boot.mode)" != android ] &&
    [ "$(getprop moon.boot.mode)" != shutdown ]; do
    sync_volume
    if audiod_running; then
        "$BB" sleep 15
        continue
    fi
    attempt=$((attempt + 1))
    if "$BB" timeout -s KILL "$START_TIMEOUT" /system/bin/sh "$PERSIST/moon-audio-start.sh"; then
        log "ready pid=$(cat "$RUNTIME/bridge.pid" 2>/dev/null) attempt=$attempt"
        delay=2
    else
        log "start attempt $attempt failed or exceeded ${START_TIMEOUT}s; retrying in ${delay}s"
        "$BB" sleep "$delay"
        [ "$delay" -ge 60 ] || delay=$((delay * 2))
    fi
done
log 'Moon no longer owns the display; keeper exiting'
rm -f "$RUNTIME/keeper.pid"
exit 0
