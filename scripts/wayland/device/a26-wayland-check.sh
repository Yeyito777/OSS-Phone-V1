#!/system/bin/sh
# Experiment only: is the phone in the handed-off state (no Xorg, no Android
# display stack, Moon's init session still alive so no fallback fires)?
ok=1
say() { echo "check: $*"; }
[ "$(getprop init.svc.moon-session)" = running ] || { say "moon-session is $(getprop init.svc.moon-session), fallback may fire"; ok=0; }
[ "$(getprop moon.boot.mode)" = moon ] || { say "moon.boot.mode=$(getprop moon.boot.mode)"; ok=0; }
[ "$(getprop init.svc.zygote)" != running ] || { say "zygote is running"; ok=0; }
for svc in surfaceflinger vendor.hwcomposer-2-4 bootanim; do
    case "$(getprop init.svc.$svc)" in running|restarting) say "$svc is $(getprop init.svc.$svc)"; ok=0 ;; esac
done
[ -z "$(pidof surfaceflinger)" ] || { say "surfaceflinger pid $(pidof surfaceflinger)"; ok=0; }
[ -z "$(pidof Xorg)" ] || { say "Xorg is running ($(pidof Xorg))"; ok=0; }
sup="$(getprop moon.supervisor.pid)"
state="$(cut -d' ' -f3 /proc/$sup/stat 2>/dev/null)"
[ "$state" = T ] || { say "supervisor $sup state '$state' (expected T, stopped)"; ok=0; }
[ "$ok" = 1 ] && say "ok: Xorg gone, Android display stack stopped, moon-session running, supervisor frozen"
[ "$ok" = 1 ]
