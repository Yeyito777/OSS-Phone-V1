# Page transitions in Moon's own apps

The System, Phone and Messages apps change pages with an iOS-style slide in
GPU mode, without redrawing their window every frame:

1. Just before drawing another page the app sends `transition push` (a
   deeper page) or `transition pop` (back) on Moon's control socket.
2. Moon snapshots the screen (a GPU copy of the X mirror) and answers
   `{"started": true}` once that copy is on its way to the compositor, so
   whatever the app draws next lands after it. The app then draws the new
   page as usual.
3. For 380 ms (ease-out) Moon composes the frame itself instead of showing
   the mirror alone: the status bar stays put, taken from the live mirror;
   below it a push slides the live app (the new page) in from the right
   over the snapshot, which drifts left by a third and darkens; a pop slides
   the snapshot away to the right over the live app. The live app is the
   compositor's `MG_MIRROR_ID` used as an image node, which the compositor
   redraws whenever Xorg's damage reaches the mirror; nothing in the
   compositor changed.

Touches keep going to the app throughout. Moon declines (`started: false`,
the page simply changes) when it has no GPU compositor, when the shell is
over the app (an alert, Control Centre, the switcher, a launch), for the
Browser, or if the snapshot takes longer than 120 ms. The app waits at most
for its control socket timeout (200 ms).
