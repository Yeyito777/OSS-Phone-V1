#!/bin/sh
# Runs inside /data/local/a26-wayland with KMS free (a26-wayland-run.sh):
# the kmsbench matrix. Results: /root/exp-kms/results.txt
set -u
S="${1:-6}"
out=/root/exp-kms
mkdir -p "$out"
r="$out/results.txt"
: >"$r"
run() {
    echo "== kmsbench $*" | tee -a "$r"
    timeout 90 kmsbench "$@" >>"$r" 2>>"$out/stderr.txt"
    echo "rc=$?" >>"$r"
    tail -n 2 "$r"
}
{
    echo "cpu governors: $(cat /sys/devices/system/cpu/cpufreq/policy*/scaling_governor | tr '\n' ' ')"
    echo "uclamp.min of this group: $(cat /proc/self/cgroup | grep cpu | head -1)"
} >>"$r"
run probe
run bw
run gbm
# CPU compositor on the primary plane: bilinear-scaled card band, shadow + copy
# (weston's and Xorg ShadowFB's model), one thread.
for m in 0.5 0.75 1.07 1.5 2.0 2.53; do run flip --seconds "$S" --op scale --mpx "$m"; done
run flip --seconds "$S" --op over --mpx 1.07
run flip --seconds "$S" --op src --mpx 2.53
# Threads, cached heap buffers composed in place, no shadow.
for t in 2 4; do for m in 1.07 2.53; do run flip --seconds "$S" --op scale --mpx "$m" --threads "$t"; done; done
for m in 1.07 2.53; do run flip --seconds "$S" --op scale --mpx "$m" --buf system; done
run flip --seconds "$S" --op scale --mpx 2.53 --buf system --threads 4
for m in 1.07 2.53; do run flip --seconds "$S" --op scale --mpx "$m" --noshadow 1; done
run flip --seconds "$S" --op scale --mpx 1.07 --nbuf 3
# Composition by DPU planes only, then planes plus a CPU layer.
for c in 1 2 3 4 6; do run planes --seconds "$S" --cards "$c"; done
run planes --seconds "$S" --cards 3 --buf system
for m in 0.5 1.07; do run planes --seconds "$S" --cards 3 --bg-mpx "$m" --threads 2; done
run dpms --cycles 5
echo done
