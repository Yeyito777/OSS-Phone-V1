# A good Browser on the phone: findings and proposal (2026-10-31)

The user restarted the Browser work: "Instead of Vim browser, maybe we can
get just Chrome ... like Chrome on phone, but with some changes so that you
can fully use it as if it were Vim browser." This note records what is wrong
with today's Browser and the proposed approach. Nothing here is built or
installed yet.

## Today's Browser (sandbox 6, installed build of 2026-07-26)

Tried with a copy of the launcher (`/opt/build/browser-sbx/vimbrowser-sbx`:
its own profile and log, DevTools on 9226, no audio), screenshots in
`/opt/build/browser-shots/`.

The engine is fine. Chromium 147 with a mobile user agent loads mobile sites
quickly (Wikipedia: load 1.05 s; Hacker News 0.5 s; GitHub 2.9 s; Reddit 5 s)
and the DevTools port is up 0.9 s after launch. What makes it bad is
everything around the engine:

1. **Touch is emulated.** Moon sends taps as XTEST mouse clicks. vimbrowser's
   XInput watcher turns one-finger drags into wheel scrolls and two fingers
   into page-zoom steps (`HandleA26TouchScroll`, `HandleA26PinchZoom`). So
   there is no fling (scrolling stops dead when the finger lifts), pinch
   reflows the page in 18 % steps, and there is no long-press, text selection
   or swipe-back. Pages see a mouse: `navigator.maxTouchPoints` 0,
   `(hover: hover)`, `(pointer: fine)`.
2. **Desktop rendering mode.** Classic scrollbars are always visible, and a
   page without a viewport tag is laid out 432 px wide instead of 980 px
   zoomed out, as phones do.
3. **The toolbar is a debug bar**: ←, →, a URL field that shows about 15
   characters, "Reload", and "Tabs n/m", which only cycles tabs. You can't
   open, close or see tabs, and there's no menu, suggestions, progress,
   find, history, copy link or downloads.
4. **None of vimbrowser's identity.** The phone runs stock CEF (no
   `vimbrowser` symbols in its `libcef.so`), so there's no shader (pages are
   white), and native hints are stubbed. The keybinds and command line need
   a keyboard, and the sidebar is hidden.
5. **Fonts:** the Debian root has only DejaVu, so emoji and CJK show as
   tofu (example.com).
6. **Cost:** rendering is software: ~77 % of a core at 47 updates/s while
   scrolling (`notes/gpu/BROWSER.md`).
7. It runs as root without Chromium's sandbox.

## Spikes (no build, through the sandbox Browser's DevTools)

- **Real Chromium touch works in today's windowed CEF:** DevTools
  `Input.dispatchTouchEvent` goes through Chromium's own gesture recognizer.
  A 440-px drag flung on to 930 px after the finger lifted. A two-finger
  spread gave a real pinch-zoom (visual viewport scale 4.0, `innerWidth`
  unchanged, no reflow). A long-press made neither a selection nor a menu;
  this needs checking with a real finger.
- **Mobile renderer mode works in stock CEF:**
  `Emulation.setDeviceMetricsOverride{mobile:true}` plus touch emulation
  hide the scrollbars and lay out a page without a viewport tag at 980 px,
  scale 0.44. The page then sees `maxTouchPoints` 5, `pointer: coarse` and
  `hover: none`, as in Chrome on Android.
- **Stock Blink dark mode** (`--blink-settings=forceDarkModeEnabled=true`)
  works, with no flash of white. It can stand in for vimbrowser's shader until
  the patched backend runs on arm64.

## Proposal

**Engine: Chromium through CEF, i.e. vimbrowser itself, made a phone
browser**: the layout of Chrome on Android, with vimbrowser inside. Same
profile format, IPC and `vimbrowser-cli` as on the desktop, so Exocortex
agents and Moon's `open-focus-tab` keep working.

- **Input:** the XInput watcher forwards every finger as a real touch (in-process
  DevTools `Input.dispatchTouchEvent`) instead of wheel/zoom emulation.
  Chromium then does fling, pinch, overscroll and DOM touch events. Moon
  stops sending XTEST taps into the Browser, a small additive Moon change, so
  a tap does not arrive twice.
- **Mobile mode** per tab (the emulation above, or natively in the patched
  build), with a "Desktop site" toggle.
- **UI**, following Chrome on Android's bottom-toolbar layout and
  vimbrowser's `docs/design.md` (whale palette, square corners):
  - a bottom bar with the address pill (domain, lock, progress), the tab
    count and a ⋮ menu; it hides while scrolling down;
  - an address sheet with suggestions from history and open tabs, and
    vimbrowser's `yt`/`gh`/`ai` engines with per-engine history;
  - a tab grid with thumbnails and vimbrowser folders as groups:
    new, close, undo close;
  - a menu with back/forward, reload, find in page, shader/dark,
    desktop site, zoom, copy URL/title/markdown link (`yy`/`yt`/`ym`),
    history and downloads;
  - long-press menus for links and images.
  The bar stays CEF Views. The sheets and the tab grid are an internal HTML
  overlay, which is quick to make look right and gets the same touch path.
- **Carried over unchanged:** IPC/CLI, content blocking, tabs with stable
  IDs, folders, session recovery, undo close and find. The shader and native
  hints need the patched backend. Vim keys and the `:` line work when a
  keyboard is attached.
- **Fonts:** Noto Color Emoji and Noto CJK in the root.

**Build.** vimbrowser's main branch now refuses stock CEF: the A26 build
checks for the patched API (`vimbrowser_private_api.h`). There are two ways:

- (a) Add a small compatibility shim in my own branch, so the shell builds
  against the stock arm64 CEF that is already on the phone. That takes
  minutes per build, but the shader, hints and control inspection stay off.
- (b) Cross-build vimbrowser's patched Chromium/CEF for arm64 on whale, with
  the same `args.gn` as `Release_GN_x64` and `target_cpu="arm64"`. That
  needs the arm64 sysroot and is a cold build of an estimated 8–12 h on
  whale's Ryzen 5 3600XT, about 40 GB, in my own clone. After that it is
  incremental.

Recommended: (a) for phases 1–2, and (b) when the user agrees to occupy
whale (niced, overnight), for phase 3.

**Effort** (agent days, estimates):

| Phase | Work | Estimate |
|---|---|---|
| 0 | Own clone, A26 shell build (a) | 0.5 d |
| 1 | Touch forwarding, mobile mode, Moon tap flag, fonts, checks on the device | 2–3 d |
| 2 | Phone UI (bar, address sheet, tab grid, menu, long-press) | 4–6 d |
| 3 | Patched arm64 backend (b): shader, hints, parity checks | 1–2 d + the build |
| later | GPU (Venus) and Wayland zero-copy: `notes/gpu/BROWSER.md` | parked |

**Not chosen:**

- Chrome for Android needs Android's framework, which Moon replaces.
- Desktop Chrome or Chromium for arm64 runs in the same root without a
  build, but it has a desktop UI at 432 dp, the same faked input, and none
  of vimbrowser.
- A new Moon-native Rust shell over CEF off-screen rendering would match
  the Moon apps best, but it rewrites vimbrowser's 27k-line shell: 4+ weeks.
- A Wayland-native Browser waits for the parked Wayland work.

**Questions for the user:**

- The look: vimbrowser's whale design (flat, square) or Moon's iOS-dark
  style?
- Should tabs become Chrome-style CEF browsers? That brings Chrome's password
  manager and autofill, but vimbrowser's tabs are Alloy-style today.
- When may whale do the arm64 Chromium build?

## Step 1: real touch, phone page mode, fonts (2026-10-31)

Route (a), on stock CEF 147 arm64. vimbrowser's work is on branch
`phone-touch` of a clone on whale (`~/Workspace/research/vimbrowser-phone`,
`backend/` left out of the checkout); Moon's is in this branch.

- **Moon** (`src/a26-shell/src/touch_stream.rs`, `docs/app-services.md`):
  `touch stream` hands an app that asks every finger of the touches Moon
  would turn into XTEST taps, as `begin/move/end ID X Y MS` and `cancel ID`
  lines. MS is when Moon got the contact, so fling velocity follows the
  finger. Moon keeps its keyboard, overlays and the bottom 180 px. A
  subscriber that can't take a line is dropped, and apps that never ask are
  unchanged. `touch begin|move|end` inject contacts, so sandboxes can drive
  real multi-touch.
- **vimbrowser** (`src/a26_touch.cc`, `docs/a26-phone-shell.md`):
  - Page fingers go to Chromium through in-process DevTools
    `Input.dispatchTouchEvent`. A still 500 ms hold opens the context menu.
    Other taps in the window go back to Moon as `tap X Y`.
  - Every tab gets DevTools mobile and touch emulation plus Android client
    hints. Desktop site is per tab (IPC `desktop-site`, or the long-press
    menu).
  - `VIMBROWSER_STOCK_CEF` builds against the stock distribution.
    `scripts/a26/build-cross.sh` takes about 45 s from scratch on whale.
- **Fonts:** Noto Color Emoji and Noto CJK (`scripts/a26/fonts.sh`, an 82 MB
  overlay for the installed root).

Verified in sandbox 6, with Moon's `touch` injection standing in for fingers:

- Drags flung in proportion to their speed: a 700 px drag over about 0.4 s
  carried on to about 650 CSS px, and a quick flick to 2757.
- A spread pinched to scale 5 and back to 1, with `innerWidth` unchanged.
- A 900 ms hold on a link opened the menu with the link items and
  Desktop site. Tapping Desktop site gave a desktop user agent and client
  hints, and the desktop page loaded zoomed out to fit.
- A tap on a page field focused it and raised Moon's keyboard, and keys
  typed into it. Back in the row above the keyboard (stream, then Moon
  `tap`) went back. Back on the bottom edge (Moon's XTEST, unchanged) also
  went back.
- `navigator.maxTouchPoints` was 5, with a coarse pointer and overlay
  scrollbars. Client hints were mobile Android.
- Emoji, Chinese, Japanese and Korean rendered.

Not verified without a finger: real-device feel and latency, Moon's bottom
gestures beside the stream, and long-press text selection. Long-press opens
the context menu but does not select a word as Chrome on Android does.
