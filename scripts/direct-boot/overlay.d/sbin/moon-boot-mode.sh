#!/system/bin/sh
# Run by init (exec, so init waits) at post-fs-data, before zygote-start.
# Prints nothing; sets moon.boot.mode from the policy on /data:
#   moon     Moon owns this boot: the framework is never started
#   android  stock Android boot (and the legacy Magisk takeover stays off)
#   unset    no direct-boot policy installed: stock boot, legacy behaviour
# Anything unexpected (missing, slow, or failing policy) means Android.
POLICY=/data/adb/moon/moon-direct-policy.sh
LOG=/data/adb/moon/direct-boot.log

[ -r "$POLICY" ] || exit 0
decision="$(/system/bin/timeout 5 /system/bin/sh "$POLICY" 2>>"$LOG")"
case "$decision" in
    moon) /system/bin/setprop moon.boot.mode moon ;;
    legacy) ;;
    *) /system/bin/setprop moon.boot.mode android ;;
esac
exit 0
