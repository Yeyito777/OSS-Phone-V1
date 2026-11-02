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

## Step 2: the phone UI (2026-11-02)

The user's answers:
- **Look:** Chrome on Android's layout and behaviour, in Moon's dark iOS
  style like the phone's other apps.
- **Chrome features:** yes to whatever makes it properly functional,
  including Chrome-style tabs for passwords and autofill, as long as
  `vimbrowser-cli`/IPC keep working.
- **Whale:** may build the patched Chromium for arm64 at idle priority. A
  subagent started it on 2026-11-02 at 00:44 EST in
  `~/Workspace/research/vimbrowser-chromium-arm64`.

**Architecture.** The UI is a local page (vimbrowser `assets/phone-ui`, with
Moon's Inter) in its own Alloy BrowserView and in-memory request context.
It sits in the window's bottom row, so the page's area never changes size.
It talks to vimbrowser by prefixed console messages, and vimbrowser answers
with `window.phone.*`.
- **Views that cover the page** (tab grid, menu, long-press sheet, address
  editor): the UI's view grows to the whole window and the page is hidden.
  Menus and sheets show a dimmed snapshot of the page behind them
  (`Page.captureScreenshot`). CEF can't give a windowed BrowserView a
  transparent background, so this is how a menu shows over the page.
- **Touch:** each surface, the page or the UI, gets real touch from Moon's
  stream. Taps in the bottom 180 px reach the bar as Moon's XTEST clicks.
- **Typing into the UI's field:** printable keys go in as DevTools
  `Input.insertText`. A CHAR sent back into the UI's own key pipeline is
  dropped, so the page fields' CHAR workaround doesn't work there.

**Increment 1** (handover `browser-vimbrowser-bc77309cdf91` and
`browser-phone-ui-8794d0b9e7d5.tar`, vimbrowser b0ebe951df):
- **Bar:** Back, the address pill (lock and host), a load progress line,
  the tab count and ⋮.
- **Tab switcher:** a two-column grid of thumbnails. Tap a card to switch,
  × or swipe to close; plus new tab and Reopen.
- **Address editor:** Safari-style, at the bottom above Moon's keyboard,
  with "Go to site" or "Search the web", Switch-to-tab and typed-history
  suggestions.
- **⋮ menu:** forward, reload or stop, copy link, new tab, reopen closed
  tab, and a Desktop site switch.
- **Long-press sheet:** Chrome on Android's link, image (with preview),
  selection and field items, plus a page menu.

Measured in sandbox 6 (phone at load ~20): the menu opens about 270 ms after
the tap and the tab grid about 340 ms after; most of that is the snapshot.
Fallback: without `Release/phone-ui` the binary keeps the old Views row.

**Increments 1.1 to 2.3** (all deployed, the last at vimbrowser e006c0f4d2):
- **1.1:** the UI moved into a window overlay, so its views open on the tap
  (25-55 ms from finger lift, measured through Moon).
- **2:** History, Bookmarks, Downloads, Find in page, and the edge back
  gesture.
- **2.1:** Settings (search engine, desktop default, clearing data) and pull
  to refresh.
- **2.2:** dark sites (`prefers-color-scheme` and auto dark); page alerts
  shown by the phone UI, since Chrome's own couldn't be answered by touch; a
  file picker over Downloads.
- **2.3:** word selection on hold; copies go to Moon's clipboard, where
  vimbrowser had shelled out to xclip, which the app root doesn't have; and
  incognito tabs.

**Passwords and autofill.** Chrome-style tabs can't be used: CEF allows only
one Chrome-style BrowserView per window ("Cannot add multiple Chrome style
BrowserViews"). But CEF's Alloy tabs already call
`TabHelpers::AttachTabHelpers`, so they have Chrome's password manager and
autofill clients. What they lack is the UI: the save bubble needs a Browser
window. The plan, on the patched arm64 backend, is to hook
`ManagePasswordsUIController` when it has no Browser and pass the pending
save to vimbrowser, which shows it in the phone UI and answers through a
new private API (save / never). Filling may then work through Chrome's own
autofill popup; that's to be tested once there is a saved password.

**Increments 2.4 and 2.5** (deployed, the last at vimbrowser 6e905fce3b):
- **2.4:** a page in fullscreen (a video) gets the whole window; camera and
  microphone requests are declined at once (the app root must never open
  `/dev/video*`); tab thumbnails survive a restart; vimbrowser's whale
  shader is a Settings switch, off on the phone; a tab whose renderer died
  reloads once; the address editor lists the search engine's suggestions.
- **2.5:** the bar steps aside while a page field has Moon's keyboard
  (Moon shrinks the window above it), as Chrome's bottom toolbar does. The
  back swipe does what Android's does: it hides the keyboard first, then
  leaves fullscreen, and only then goes back. Also "Link you copied" from
  Moon's clipboard (`clipboard get`), Close all tabs, and a fix: a Google
  results URL from History opens as a URL instead of being searched for.
- **Dropped:** holds and swipes on the bar (Chrome's long-press Back for
  history, and swiping along the toolbar to switch tabs). The bar lies in
  Moon's 180 px home-gesture zone, where the app gets taps only, a hold
  over 650 ms is dropped, and a sideways swipe is Moon's app switch.

**Codecs.** The stock CEF arm64 build has no H.264 or AAC
(`MediaSource.isTypeSupported('video/mp4; codecs="avc1.42E01E"')` is
false; VP8, VP9 and AV1 play), so MP4 video on Instagram, X and many news
sites doesn't play. The patched backend is built with
`proprietary_codecs=true` and `ffmpeg_branding="Chrome"`. Since its
runtime replaces libcef and its resources (about 300 MB), it goes over as
a whole Release tree with an install/rollback script, not as the usual
binary and phone-ui pair. `scripts/a26/cef-distrib-arm64.sh` builds the
CEF distribution the shell builds against from the backend's arm64 out
dir.

**Increments 2.6 to 2.8** (handed over, the last at vimbrowser 07586327d6):
- **2.6:** Chrome's new tab page on blank tabs: a search box and the 8
  sites visited most (often and lately, one per site); History search.
- **2.6.1:** the launcher always passes https://example.com, and vimbrowser
  added it to the restored session and made it the active tab at every
  start. Now, with --a26-shell and a session to restore, the start URL is
  dropped (links come over IPC).
- **2.7:** Chrome on Android's page fonts. Pages that name no font, and
  buttons, got desktop Chrome's Times (DejaVu Serif). The standard and
  sans-serif families are now Roboto, then DejaVu Sans. An overlay for the
  app root (`fonts-android.sh`, installed by `browser-fonts-install`) adds
  Roboto, Liberation (metric-compatible Arial, Helvetica, Times and Courier)
  and a fontconfig rule putting Roboto first for sans-serif and system-ui.
- **2.8:** taps reach cross-site iframes. DevTools touch (TouchEmulator)
  forwards its gestures only to the page's own renderer. In an
  out-of-process iframe the touches arrived but the tap's click never did:
  embedded players, Cloudflare Turnstile, sign-in and consent buttons.
  When a tap ends over a frame that is a target of its own
  (`DOM.getNodeForLocation` → `DOM.describeNode` frameId →
  `Target.getTargets`), vimbrowser clicks it with DevTools mouse input,
  which is routed properly. Scrolling in such frames already worked. Also:
  the editor offers a results page's query; intent: links go to their
  fallback; tel:, sms: and mailto: are copied.
- **2.9 and 2.10:** Download link / image, Translate (Google's page proxy),
  removing a page from History. 2.10 corrects 2.8. DevTools touch does give
  an out-of-process iframe the tap's click, but with the frame's offset
  applied twice, so the click falls outside the frame or on the wrong
  element, and 2.8's added mouse click made a second click. Now a sequence
  that begins over such a frame is probed at touch-down. A tap there ends
  in a touchcancel (no tap from the emulator) and gets only the mouse
  click. Touches inside such frames still carry the doubled offset (drags
  work, since only their deltas count); the patched backend could fix
  TouchEmulator's transform at the source.
- **PDFs:** Chrome's PDF viewer page has no viewport tag, so mobile
  emulation laid it out 980 px wide (viewer at 0.44 scale). A new-document
  script (which needs `Page.enable`) gives PDF pages `width=device-width`.
  The viewer also sets a zoom level of its own that does not zoom the page,
  so input now divides by the page's measured zoom (its devicePixelRatio
  over the display's), not by `GetZoomLevel`.
- **Seen, not done:** `<select>` lists are desktop dropdowns with 24 px rows
  (usable).

**The patched backend** (handed over 2026-11-02 as a whole Release tree,
`browser-release-deploy` with rollback). vimbrowser's own CEF 147 built for
arm64 on whale: proprietary codecs (H.264/AAC play), vimbrowser's native
API, and five patches of ours: the password prompt hook (save sheet in the
phone UI; Chrome autofills on page load as usual), the touch emulator taking
injected touches in root coordinates, and ChromeBrowserWidget observing each
ThemeService once. The last one matters because the backend is a
non-official build, where `DUMP_WILL_BE_NOTREACHED` is fatal: an in-memory
request context (the phone UI's, incognito's) shares its original's theme
service, and its second AddObserver aborted the shell at startup. The shell
builds against it with `A26_CEF_ROOT=third_party/cef-a26-arm64-vimbrowser`
(from `cef-distrib-arm64.sh`) in `build-a26-arm64-vimbrowser`; both CEF
kinds mount at /cef and keep old file times, so they must not share a build
directory. Saved passwords use Chrome's Linux fallback key (no keyring in
the app root).
