#!/usr/bin/env python3
"""Analyses a soak run pulled by scripts/a26-shell/soak.sh report: drift of
memory, CPU, threads and open files per Moon process, restarts, the idle
drain with the screen off and charging paused, the scheduled events, and
errors in the logs."""

import csv
import re
import sys
from collections import defaultdict
from pathlib import Path

HZ = 100  # USER_HZ on the phone's kernel


def slope(points):
    """Least-squares slope of (x, y) points."""
    n = len(points)
    if n < 2:
        return 0.0
    mx = sum(x for x, _ in points) / n
    my = sum(y for _, y in points) / n
    den = sum((x - mx) ** 2 for x, _ in points)
    return sum((x - mx) * (y - my) for x, y in points) / den if den else 0.0


def main(run: Path) -> int:
    samples = list(csv.DictReader(open(run / "samples.csv")))
    procs = list(csv.DictReader(open(run / "procs.csv")))
    events = list(csv.DictReader(open(run / "events.csv")))
    if not samples:
        print("no samples")
        return 1
    t0, t1 = int(samples[0]["t"]), int(samples[-1]["t"])
    hours = (t1 - t0) / 3600
    print(f"run {run.name}: {hours:.1f} h, {len(samples)} samples")
    falls = [s for s in samples if s["boot_mode"] not in ("moon", "")]
    if falls:
        print(f"!! boot mode left moon at {falls[0]['t']}: {falls[0]['boot_mode']}")

    print("\nprocess           pids  rss start->end (kB)  rss/h   cpu%   thr  fds   fd/h")
    by_proc = defaultdict(list)
    for row in procs:
        by_proc[row["proc"]].append(row)
    worst = []
    for name, rows in sorted(by_proc.items()):
        pids = sorted({r["pid"] for r in rows})
        # Drift per pid lineage: follow the longest-lived pid.
        main_pid = max(pids, key=lambda p: sum(1 for r in rows if r["pid"] == p))
        life = [r for r in rows if r["pid"] == main_pid]
        hrs = [(int(r["t"]) - t0) / 3600 for r in life]
        rss = [int(r["rss_kb"]) for r in life]
        fds = [int(r["fds"]) for r in life]
        ticks = [int(r["ticks"]) for r in life]
        span = (int(life[-1]["t"]) - int(life[0]["t"])) or 1
        cpu = 100 * (ticks[-1] - ticks[0]) / HZ / span
        rss_h = slope(list(zip(hrs, rss)))
        fd_h = slope(list(zip(hrs, fds)))
        print(f"{name:17} {len(pids):4}  {rss[0]:7} -> {rss[-1]:7}  {rss_h:+6.0f}  {cpu:5.2f}  "
              f"{life[-1]['threads']:>3}  {fds[-1]:3}  {fd_h:+5.1f}")
        if rss_h > 1024 or fd_h > 2:
            worst.append(f"{name}: rss {rss_h:+.0f} kB/h, fds {fd_h:+.1f}/h")
        if len(pids) > 1:
            worst.append(f"{name}: {len(pids)} pids (restarted)")

    # Idle drain: discharging, screen off (backlight 0), consecutive samples.
    idle = [s for s in samples
            if s["backlight"] == "0" and s["status"] in ("Discharging", "Not charging")]
    if len(idle) >= 30:
        points = [((int(s["t"]) - t0) / 3600, float(s["capacity"])) for s in idle]
        current = sorted(abs(int(s["current_ua"] or 0)) for s in idle)
        print(f"\nidle drain (screen off, not charging, {len(idle)} min): "
              f"{-slope(points):.2f} %/h, median current {current[len(current) // 2] / 1000:.0f} mA")
    else:
        print("\nidle drain: not measured (charging, or too few screen-off minutes)")
    # CPU busy % and wakeups (context switches, interrupts) per second, by
    # screen state, from consecutive samples.
    if "cpu_busy" in samples[0]:
        rates = {"screen off": [], "screen on": []}
        for a, b in zip(samples, samples[1:]):
            try:
                dt = int(b["t"]) - int(a["t"])
                total = int(b["cpu_total"]) - int(a["cpu_total"])
                busy = int(b["cpu_busy"]) - int(a["cpu_busy"])
                ctxt = (int(b["ctxt"]) - int(a["ctxt"])) / dt
                intr = (int(b["intr"]) - int(a["intr"])) / dt
            except (ValueError, ZeroDivisionError, KeyError):
                continue
            key = "screen off" if a["backlight"] == "0" and b["backlight"] == "0" else "screen on"
            if total > 0:
                rates[key].append((100 * busy / total, ctxt, intr))
        for key, values in rates.items():
            if values:
                n = len(values)
                print(f"{key}: CPU busy {sum(v[0] for v in values) / n:.2f} % (all cores), "
                      f"{sum(v[1] for v in values) / n:.0f} context switches/s, "
                      f"{sum(v[2] for v in values) / n:.0f} interrupts/s ({n} min)")
        d = [int(s["d_state"]) for s in samples if s.get("d_state", "").isdigit()]
        if d:
            print(f"tasks in D state (counted by the load average): {min(d)}-{max(d)}")
    mem = [int(s["mem_available_kb"] or 0) for s in samples]
    print(f"MemAvailable: {mem[0] // 1024} -> {mem[-1] // 1024} MB "
          f"({slope([((int(s['t']) - t0) / 3600, int(s['mem_available_kb'] or 0)) for s in samples]) / 1024:+.1f} MB/h)")

    print("\nevents:")
    tally = defaultdict(lambda: [0, 0])
    for e in events:
        tally[e["event"]][0 if e["result"] == "ok" else 1] += 1
    for name, (ok, bad) in sorted(tally.items()):
        print(f"  {name:13} ok {ok:3}  not ok {bad:3}")
    for e in events:
        if e["result"] not in ("ok", "skip"):
            print(f"  !! {e['t']} {e['event']} {e['result']}: {e['detail']}")
            worst.append(f"event {e['event']} {e['result']}")

    patterns = {
        "kernel": re.compile(r"Kernel panic|Oops|BUG:|lowmemorykiller|Out of memory|Killed process|hung_task", re.I),
        "moon": re.compile(r"panicked|error|failed|exited rc|restoring Android|fallback", re.I),
        "crash": re.compile(r"FATAL EXCEPTION|Fatal signal|tombstone", re.I),
    }
    print("\nlog scan:")
    for label, path, pattern in (("dmesg", "dmesg.txt", patterns["kernel"]),
                                 ("moon logs", "moon-logs.txt", patterns["moon"]),
                                 ("logcat", "logcat.txt", patterns["crash"])):
        file = run / path
        if not file.exists():
            continue
        hits = [line.rstrip() for line in file.open(errors="replace") if pattern.search(line)]
        print(f"  {label}: {len(hits)} matching lines")
        for line in hits[-5:]:
            print(f"    {line[:160]}")

    print("\nverdict:", "attention: " + "; ".join(worst) if worst else "no drift, restarts or failed events")
    return 0


if __name__ == "__main__":
    sys.exit(main(Path(sys.argv[1])))
