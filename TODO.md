1. [x] Replace per-app child slots with a lifecycle-aware application registry.
2. [x] Make swipe-up hide and background apps instead of terminating them.
3. [x] Assign each app process tree to an isolated freezer cgroup.
4. [x] Unfreeze and remap background apps for instant reopening.
5. [x] Add bounded background-execution leases for media and transfers.
6. [x] Connect Vimbrowser media playback state to Moon’s lease protocol.
7. [x] Add memory-pressure eviction of least-recently-used background apps.
8. [x] Replace Moon’s idle 8 ms polling with blocking event waits.
9. [x] Add a device-local suspend coordinator with safe lock, display, and app sequencing.
10. [x] Enter deep suspend on screen-off and wake safely from approved hardware sources.
11. [x] Add a central alarm and notification service with RTC wake scheduling (moon-notifyd, `docs/notifications-protocol.md`).
12. [x] Measure and document foreground, background, audio, and suspended battery use (`docs/battery.md`).
