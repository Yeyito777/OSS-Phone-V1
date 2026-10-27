#!/system/bin/sh
# Device side of scripts/a26-shell/light-sweep.sh (root, Moon mode). Reads
# "X Y SIZE" lines from $1; for each, Moon shows the light-probe frame (black,
# one white square; SIZE 0 is all black), the light sensor is switched off and
# on so the hub logs fresh readings, and "X,Y,SIZE,LUX" is printed with the
# last of them.
ROOT=/data/local/a26-linux
BB="$ROOT/busybox.static"
CTL="$BB chroot $ROOT /opt/a26-shell/bin/a26-shellctl"
# The enable node returns more than was written, which shell printf/echo
# write loops abort on: one os.write each.
SWITCH='import os
for v in (b"50\n", b"51\n"):
    f = os.open("/sys/class/sensors/ssp_sensor/enable", os.O_WRONLY)
    os.write(f, v)
    os.close(f)'

count() { dmesg | grep -c "Light Sensor : lux="; }

while read -r x y size; do
    $CTL screen on >/dev/null
    $CTL light-probe "$x" "$y" "$size" >/dev/null
    sleep 0.25
    before=$(count)
    $BB chroot $ROOT /usr/bin/python3 -c "$SWITCH"
    for _ in 1 2 3 4 5 6 7 8 9 10; do
        sleep 0.1
        [ $(($(count) - before)) -ge 3 ] && break
    done
    lux=$(dmesg | grep "Light Sensor : lux=" | tail -1 | sed 's/.*lux=\([0-9]*\).*/\1/')
    echo "$x,$y,$size,$lux"
done <"$1"
