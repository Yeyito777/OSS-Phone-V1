#!/system/bin/sh
# Magisk service.d: start the Exocortex keeper at every boot, in Moon and
# Android mode (docs/EXOCORTEX.md). It detaches itself; this returns at once.
KEEPER=/data/adb/exocortex/exocortex-keeper.sh
[ -r "$KEEPER" ] || exit 0
# Android's init supervises it instead (the proposed exocortex.rc).
[ "$(getprop init.svc.exocortex)" != running ] || exit 0
/system/bin/sh "$KEEPER" boot </dev/null >/dev/null 2>&1
exit 0
