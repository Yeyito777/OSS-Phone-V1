# Moon as a Wayland compositor: decision, plan and status

**Status: parked (2026-10-27) by the user's priority, a good basic OS
first.** Everything below is on main and opt-in. With no switches set, the
session is today's: moon-compositor and headless Xorg. Companion
documents: `notes/wayland/DESIGN.md` (the measurements that led here) and
`notes/gpu/DESIGN.md` (moon-compositor, the display path every milestone
builds on).

## Where to resume

**What is on main**

| Piece | State | Switch |
|---|---|---|
| M1/M2: native Wayland apps | Verified on the phone. Shown through X proxy windows, or directly in GPU mode | `WAYLAND_APPS=1` in `/data/adb/moon/config` (apps listed in `MOON_WAYLAND_APPS`, default System) |
| M3: Moon reads the touchscreen | Verified off-device through the FIFO replay and under Xwayland on the phone; real fingers not tried | `MOON_TOUCH=evdev` (always on under Xwayland) |
| M4a: Xwayland instead of headless Xorg | 9 h overnight on the phone. Merge gate passed: a real boot on Xwayland, the fallback to Xorg in the same boot, a boot back without it | `XWAYLAND=1` (GPU path only, Xwayland in the chroot from `moon-phone.sh xwayland-install`) |
| dmabuf-backed `wl_shm` pools | Full-screen demo at 96 to 103 fps, against 60 with memfd; the buffer goes on a display plane | Automatic for clients that allocate from `/dev/dma_heap/system` |
| The Exocortex app on Wayland (below) | Built and measured in sandboxes against moon-compositor-sim; real fingers next | `a26-shellctl settings set wayland_apps exocortex` (no reboot, no restart) |

The phone's chroot keeps Xwayland 24.1.14 and libtirpc. They do nothing
until `XWAYLAND=1` is set.

**Open, in order of value**
1. **Real fingers on Xwayland.** The user unlocks, scrolls, uses the home
   swipe and the switcher, and types in apps. Injected evdev touches
   already work. Run `moon-phone.sh xwayland-start`, then `moon-phone.sh
   reboot` to come back.
2. **Make Xwayland the default?** Before deciding, the remaining risk
   points:
   - Xwayland has no DPMS, so it needs the GPU path; the software
     fallback is Xorg.
   - No deep suspend under Xwayland: its helper replaces Xorg.
   - Run `scripts/wayland/m4-boot-test.sh` again before flipping the
     default.
3. **The slower paint in dma-heap buffers.** The demo paints in 3.5 ms
   against 2.07 ms on the memfd path. The mapping is cached (heapbench) and
   the flush is not the cause. Suspects: bandwidth contention with the
   plane scanout, or CPU frequency. It decides whether full-screen native
   apps reach 120 Hz.
4. **M4b, rootless Xwayland.** Each X window would be its own surface.
   Only worth it with an Xwayland whose shm pixmaps are dma-heap buffers
   (one copy, as now with the mirror); with memfd pixmaps it would add an
   upload per frame. Turning Moon's overlays into compositor layers
   (`notes/gpu/DESIGN.md`) removes X-side compositing more cheaply.
5. **The Browser as a native Wayland app** (vimbrowser's window code).
   Parked with all Browser work.

**Tools**
- On the phone (`scripts/wayland/moon-phone.sh`): `install`, `start
  demo|system [debug] [box] [dmabuf] [evdev]`, `xwayland-install`,
  `xwayland-start`, `heapbench`, `restore`, `reboot`.
- Device checks: `scripts/wayland/wake-test.sh` and
  `scripts/wayland/m4-boot-test.sh`.
- Off-device on whale: `scripts/wayland/dev-run.sh`, `xwayland-run.sh`
  (Xwayland in a container, touch from `touch-replay.py`) and
  `scripts/gpu/mock-run.sh` with `mock-compositor.py` (`--textures`,
  `--render-ms`, `--upload-ms-per-mpx`, `--powered-off`).
- `MOON_PROFILE=1` (or `MOON_WAYLAND_DEBUG=1`) logs each direct frame's
  timing.

## The Exocortex app straight to the panel (branch direct-moon, 2026-10-31)

The Exocortex app (a26-system `--app exocortex`, branch direct-app) is a
Wayland client of Moon when Moon names it. Its frames reach moon-compositor
without X's copies, touches come from Moon, and it draws in step with the
panel.

**Switching it on.** `settings set wayland_apps exocortex` (Moon's
settings, `docs/settings.md`; a list of Moon's own apps, or `none`). Moon
binds its server in `/run/moon-wayland` when the setting names an app, at
start or when it changes; the app takes Wayland at its next start. No
supervisor change: `WAYLAND_APPS` / `MOON_WAYLAND_DIR` / `MOON_WAYLAND_APPS`
still work and override it. `state.apps[].wayland` says which run on it.

**Coming back by itself.**
- The app goes on in an X window, with everything it holds, when Wayland
  cannot connect or its connection fails later (Moon restarted, a protocol
  error).
- An app that dies on Wayland (a crash, not a close) is started again at
  once on X, and stays on X until `wayland_apps` is set again
  (`apps.rs`).
- A buffer the compositor refuses sends the app back through its X proxy
  (M2, as before).

**The app's side** (`src/exocortex/wayland.rs`): one full-screen xdg
toplevel at the preferred scale; dma-heap buffers with 64-byte rows (memfd
when there is no heap), two that alternate and a third only while both are
held. The app still draws into its canvas; a frame copies into a free buffer
what changed since that buffer was last written (its own damage plus the
frames it missed). A frame is drawn only once the last one's frame callback
came (Moon answers it at the flip that showed it), so frames follow the
panel; a callback that never comes holds nothing back after 100 ms. Keys
come as wl_keyboard keys of Moon's US map (the keyboard is still asked for
over the control socket, with every purpose, as on X: X typed ASCII only
too).

**Moon's side:**
- **Touch:** a touch Moon forwards to a Wayland app goes straight to its
  surface as wl_touch (`WaylandApps::app_pointer`), not through XTEST, the
  X server and back to the proxy window.
- **Leaving direct display** (a snapshot, the switcher, a key callout that
  rises over the app, a page transition, `screen sync`): the proxy gets the app's current buffer
  first, and the panel keeps the last direct frame until the mirror has it
  (`GpuBackend::leave_direct`), so the proxy's stale pixels never show. A
  page transition snapshots the app's current frame.
- **One full-width layer per row:** with an opaque full-width buffer in
  front, the mirror is scanned out only where the app does not cover it
  (the status strip, Moon's keyboard). Two full-width layers a row is at the
  DPU's limit at 120 Hz (it underruns at about 2-3).
- **Frame callbacks:** a commit that no direct frame showed still gets its
  callback on the X path's grid.
- **Screenshots:** `a26-shellshot` asks Moon `screen sync` first.
- **Profiling:** `a26-shellctl profile on` also turns on the direct
  frames' timing summary (`wayland: direct timing`) without a restart.

**Measured** (sandboxes on the phone, moon-compositor-sim with its probe,
uinput touches through Xorg's evdev driver, the exo-bench scenarios and
replay; the same app binary on X and on Wayland, 3 rounds interleaved;
direct-moon bc2b414 / direct-app 2273034; `/opt/build/direct-tools` on the
phone, `exo-bench-wl/exo-bench.py --probe`):

| | X | Wayland |
| --- | --- | --- |
| Touch report to the panel, p50 / p95: EXO scroll | 24.5 / 49.7 ms | 12.6 / 23.0 ms |
| the same: list scroll | 28.2 / 54.4 ms | 13.5 / 22.5 ms |
| the same: EXO, ten flings into older history | 42.3 / 69.0 ms | 13.3 / 23.5 ms |
| Frames on the panel while scrolling, gap p50 / p95: EXO | 16.7 / 41.7 ms | 8.3 / 16.7 ms |
| the same: list | 16.7 / 50.0 ms | 8.3 / 33.3 ms |
| Display path work per frame (every thread of the app, Xorg and Moon): EXO, list, stream, sheet | 8.9, 8.9, 7.4, 10.2 M instructions | 3.0, 3.1, 3.0, 5.3 M |
| Xorg's work over the EXO scroll | 3302 M | 25 M |
| EXO's open: tap to its first frame | 69.7 M instructions, 42 ms | 53.4 M, 29 ms |
| Typing: finger down to the frame handed over, p50 / p95 | 8.3 / 17.9 ms | 5.9 / 11.6 ms |

The app's own thread does more per frame (EXO scroll 1.26 to 2.28 M
instructions): it now copies into the buffer what Xorg copied three times
before, plus Moon's mirror copy. A top-row key's callout rises over the app
and is an X window, so while it shows the app goes back through X: typing
is the one scenario where the display path does not get cheaper (+1 %; +6 %
before the other keys' callouts kept direct display, 7fc451a).

**The simulator's probes** (`src/moon-gpu/sim.c`): `MOON_SIM_PROBE=PATH:X,Y`
logs each refresh whose pixel at (X, Y) changed; the app's
`EXOCORTEX_APP_PROBE` writes a frame count and the finger row into its
top-left pixel, which is (0, 124) on the panel on both paths.
`MOON_SIM_DUMP=PREFIX` writes the next refresh as a PPM on SIGUSR1 (it
holds the scanout thread, so it is for what is shown, not for timing).

**Next:**
- the top-row key callout as a patch of the mirror over a proxy kept fresh
  only under it (or a Moon layer), so all of typing stays direct;
- the client's own cache clean (Moon's `DMA_BUF_IOCTL_SYNC` per commit,
  about 0.5 ms on its loop);
- real fingers on the phone (plan in the hand-over).

## Decision: Moon speaks Wayland, moon-compositor stays the display

There are three ways to make Moon a Wayland compositor. We take the third.

1. **Teach moon-compositor Wayland.** Rejected. It's a bionic C daemon,
   outside the chroot, that knows frames, not windows. Focus, input, the
   keyboard and app lifecycle would have to move into it or be mirrored
   across the socket.
2. **A smithay compositor that absorbs moon-compositor.** Rejected:
   - The Mali blob is bionic-only and has no GBM or EGL platform, so
     smithay's GL renderer and `DrmCompositor` can't use it.
   - The GPU tier would have to be rewritten as a bionic Rust process.
   - The working, measured, merged C compositor and its fallback chain
     would be thrown away.
3. **Moon is the Wayland server; moon-compositor stays the only DRM
   master.** Chosen:
   - Moon already owns what a Wayland server decides: windows, focus,
     touch routing, the keyboard, app lifecycle, freezer and leases.
   - moon-compositor keeps owning what reaches the panel: planes first,
     then GPU, then CPU, the `HDR_FD`/`DQE_FD = -1` rule, `ACTIVE` screen
     power, and the in-place fallback to Xorg.
   - The protocol between them already carries what client buffers need:
     memfd surfaces copied on damage, and dmabuf surfaces sampled in place
     and eligible for planes. v3 adds the byte offset a `wl_shm` buffer
     has inside its pool.

The server is written on `wayland-server` (pure-Rust backend, statically
linked like the rest of Moon), not on smithay:
- Moon needs about a dozen protocols, and none of smithay's renderers or
  backends.
- smithay's keyboard pulls in libxkbcommon. Moon only sends a fixed keymap
  (`assets/xkb/us.xkb`) and lets clients parse it.
- smithay's handler-trait and calloop structure fits badly with Moon's
  one-way, never-blocking loop.

Xwayland later doesn't need smithay either. Moon is already an X11 window
manager, so it can manage Xwayland's rootless windows with the code it has
(milestone 4).

## Milestones (each behind a switch; the phone never loses its UI)

| | What | Switch | Fallback |
|---|---|---|---|
| **M1** (built) | Wayland server in Moon. Native apps run full-screen with touch, Moon's keyboard (text-input-v3) and frame pacing. Every Wayland surface is shown through an **X proxy window** that Xorg fills from the client's shared memory, so everything Moon does with X11 apps works unchanged, on both display paths. Legacy apps stay on (headless) Xorg. | `WAYLAND_APPS=1` (supervisor) / `MOON_WAYLAND_DIR` | Switch off: exactly today's Moon |
| **M2** | Direct client buffers in GPU mode, with zero X copies:<br>- a client's `wl_shm` buffer becomes a memfd surface (offset, v3);<br>- `linux-dmabuf` buffers become dmabuf surfaces that can go on a plane;<br>- the app in front is a frame node instead of the X mirror;<br>- snapshots are compositor copies of the client surface.<br>Before that, Moon's X overlays (status strip, keyboard and key callout, volume HUD, banners) become Moon layers in GPU-mode frames, since they would otherwise have to come from the mirror. Also `wp_presentation`, graceful close (`xdg_toplevel.close`, then kill), and the System app ported (dual backend). | GPU mode + M1 switch | The M1 proxy path (the software path always uses it) |
| **M3** | Moon reads touch from evdev itself, with no XI2: the MT-B parser follows the kernel protocol, and libinput with libudev-zero is the proven alternative. Moon then needs X only for legacy apps. | per-feature | XI2 path |
| **M4** | Xwayland replaces headless Xorg for CEF and legacy X11 apps. Moon is the X11 WM of the rootless Xwayland, and an Xwayland window's `wl_surface` is a client surface like any other. The supervisor's readiness becomes moon-compositor + Moon's sockets. | session flag | Today's Xorg session (moon-compositor's existing fallback) |

The GPU agent and I split the work: anything inside Moon is mine, anything
inside moon-compositor (plane rules for client buffers, direct scanout,
AFBC, protocol changes) is theirs. Protocol changes are versioned and stay
backward compatible.

## Milestone 1 in detail

### Protocols

| Global | Version | Notes |
|---|---|---|
| `wl_compositor`, `wl_surface`, `wl_region`, `wl_callback` | 6 | Double-buffered attach, damage and frame; buffer scale; `preferred_buffer_scale` and `enter(output)` so clients draw at the panel's density; no transform |
| `wl_subcompositor` | 1 | Subsurface position applies on the parent's commit; content applies at the subsurface's own commit (desync) |
| `wl_shm` | 1 | ARGB/XRGB8888. A pool larger than its file is refused, and pools are sealed against shrinking where the client allows it (SIGBUS) |
| `xdg_wm_base` | 6 | Toplevels are configured full-screen in the app area. `activated` while focused, `suspended` while in the background or screen-off. Simple popup positioning, no constraint adjustment. Each surface is shown cropped to its window geometry (client-side shadow margins would be opaque black in an X window) |
| `zxdg_decoration_manager_v1` | 1 | Always "server side": nobody draws decorations |
| `wl_seat` | 7 | Touch and keyboard. The keymap is a fixed US XKB keymap; repeat rate 0, because Moon repeats keys itself |
| `wl_output` | 4 | 1080x2340 at 120 Hz, integer scale 3 by default (`MOON_WAYLAND_SCALE`, 1-4); the server converts surface units to pixels, so Moon only deals in pixels |
| `wl_data_device_manager` | 3 | Clipboard between Wayland apps: the selection is offered to the app with keyboard focus, and reading it hands the reader's pipe to the source's owner (Moon never sees the contents). No drag and drop |
| `wp_presentation` | 1 | Answered when the commit's buffer is on screen: at the page flip (vsync, hardware clock) in GPU direct mode, at the end of the X copy otherwise |
| `zwp_text_input_manager_v3` | 1 | Enable/disable and the content purpose drive Moon's keyboard |

Not yet offered:
- `linux-dmabuf` (needs the direct path for dmabufs, since Xorg would read
  write-combined memory);
- `wp_viewporter` and fractional scale;
- pointer, primary selection and xdg-activation.

### How the pieces map

| Moon concept | Wayland side (`src/a26-shell/src/wayland/`) | Glue (`wl_apps.rs`) |
|---|---|---|
| An app's window | `xdg_toplevel` | X proxy window, managed by `manage_window` (the old MapRequest path) on the first buffer |
| Which app owns it | `SO_PEERCRED` pid of the client | `/proc/PID/cgroup` freezer path `/moon/<app>`, else the app Moon is launching |
| Placement and size | `xdg_toplevel.configure` | the proxy's ConfigureNotify (Moon's `fullscreen_window`/`resize_app_window`, the keyboard's app height) |
| Background, screen-off | `suspended` state, no frame callbacks | the proxy's Unmap/MapNotify (the registry's Hide/Show); freezer as for X11 apps |
| Focus | `wl_keyboard.enter/leave`, `text_input.enter/leave` | the proxy's FocusIn/FocusOut (Moon's `set_input_focus`) |
| Touch | `wl_touch` down/motion/up, cancel | X core pointer events on the proxy (Xorg's evdev touchscreen, implicit grab); a Moon gesture (home swipe, switcher) cancels |
| Keyboard | text-input-v3 `commit_string` / `delete_surrounding_text`, or `wl_keyboard` keys | `inject_keyboard_input` queues keys for a Wayland window instead of XTEST; text-input enable shows Moon's keyboard with the purpose, disable hides it |
| Content | `wl_surface.commit` + `wl_shm` buffer | `ShmAttachFd` of the pool (once per pool size), `ShmPutImage` of the damage. The buffer on screen is held until the next one has been copied, and exposed areas (a popup closing) are repainted from it |
| Frame pacing | `wl_callback.done` | on a 120 Hz grid, only for visible surfaces whose last content has been copied |
| Snapshots, switcher, GPU mirror | none | free: the proxy is an X window like any app's |

The supervisor passes `MOON_WAYLAND_DIR=/run/moon-wayland` (the chroot's
`/run` is a tmpfs of the session) only when `/data/adb/moon/config` has
`WAYLAND_APPS=1`. Apps get `WAYLAND_DISPLAY` (absolute path) and
`XDG_RUNTIME_DIR`.

### Safety

- Nothing in the display path changes:
  - moon-compositor is still the only DRM master, with its sanitize rule
    and fallback;
  - Xorg still owns input;
  - the switch off is exactly the previous Moon.
- A client can only hurt itself: protocol errors disconnect it, and its
  windows go away with it.
- The one cross-process hazard is shared memory: a client that shrinks its
  pool would SIGBUS whoever maps it. Pools are sealed against shrinking
  when the client allows it (most toolkits create memfds with
  `MFD_ALLOW_SEALING`), and size is checked on creation and resize. Apps
  are launched by Moon, so the remaining risk is a buggy app. moon-compositor
  inherits the same hazard in M2 (it maps memfd surfaces), so it should
  check seals or catch SIGBUS (on the wish list below).
- Password text is never logged or kept. Keys go out one at a time, as
  with XTEST.

### Verified off-device (whale)

On both display paths, the software one (Xvfb) and the GPU one (Xvfb plus
the stand-in compositor), with `src/moon-wl-demo` in the System slot
(`scripts/wayland/dev-run.sh`):

- Launch:
  - the client connects, binds the seat and text input, and is configured
    1080x2216, fullscreen and activated;
  - the app appears under Moon's status bar.
- Tapping a text field enables text-input-v3. Moon's keyboard opens and the
  app is resized to 1080x1396 above it.
- Typing:
  - the keys typed on Moon's keyboard (shift, h, i, backspace, i…) arrive
    as text;
  - return arrives as a key and the keyboard closes.
- The demo's full-screen animation paces at 120.0 fps (8.34 ms mean) from
  frame callbacks on the GPU path.
- The switcher shows the app's card.
- In the background or screen-off, the app is configured `suspended` and
  its frames stop; resuming restores focus and text input.
- `wayland::tests` drives the server with a real `wayland-client`:
  configure, commit, release, touch, text-input and keyboard.

- Stock toolkits from an Alpine container (`scripts/wayland/dev-client.sh`):
  - **foot** opens Moon's keyboard through text-input-v3 by itself, and
    `ls` plus return typed on it runs;
  - **GTK 4** (`gtk4-widget-factory`): the focused entry opens the
    keyboard, a dropdown popup opens, picks an item and closes cleanly;
    at scale 3 it draws at phone density.
- **The System app** (`a26-system`, branch `wayland`) is a native Wayland
  client when `WAYLAND_DISPLAY` is set and the X11 app otherwise. Pages,
  navigation and taps work; its text fields ask for the keyboard through
  text-input-v3 instead of Moon's control socket.

Only apps listed in `MOON_WAYLAND_APPS` (default: `system`) get
`WAYLAND_DISPLAY`. Chromium picks Wayland by itself when it sees the
variable, so the browser stays on X11 until it has been tested.

### Verified on the phone (2026-10-26, build 03c49e9, GPU tier, compositor ec0a4fb, planes off)

One 28-minute session with no reboot, using `scripts/wayland/moon-phone.sh`. Touches
were injected at evdev level on `sec_touchscreen` (sendevent), so they took
the same path as a finger: Xorg, XI2, the proxy window, wl_touch.

- Wayland off (`stop`): the v4 handshake succeeds, GPU frames run at 120 Hz,
  and the session is unchanged.
- Demo (`start demo debug`):
  - It maps fullscreen at scale 3 (a 360x739 logical toplevel) and direct
    mode engages.
  - Touch arrives at the right logical coordinates.
  - Tapping a field enables text-input-v3: Moon's keyboard opens with
    purpose text and the app is resized to 360x466. Taps on Moon's keys are
    committed to the field (`"h"`, then `"hi"`).
  - Direct mode turns off while a key callout shows and comes back on after.
  - On lock the app is configured suspended (state 9) and loses focus. On
    unlock and resume it is activated again.
- System app as a Wayland client (`start system`): it renders correctly
  (System page with the Activity and About cards).
- `restore` put back the exact Moon and System binaries (by sha256).

Found on the phone, and what came of it:
- **The first screen-off attempt is rejected ("unsafe shell state") and the
  lock screen comes back on.** This is fixed (8ab2451). The cause was not
  Wayland: it happened with X11 apps too, and reproduces off-device with
  both.
  - A finished power fade locks inside `state.tick()`, which runs after
    the loop has reconciled the registry for the app view.
  - The screen-off transaction in the same pass then still sees the app
    active, and its window managed.
  - Now the registry is reconciled again when the tick leaves the app.
    The app is unmapped and frozen before the lock frame and the safety
    check.
  - A directly shown Wayland app leaves direct mode in that same pass,
    and the check refuses while any app buffer is still on screen.
- **The animating demo runs at 60 fps in direct mode.** This is
  instrumented. One path-side delay is fixed (d499449). The remaining
  attribution needs the phone (see below).
  - Frame callbacks waited for the X path's free-running 120 Hz grid:
    1.1 ms mean, 2.1 ms max after the flip. Now they go out at the flip.
  - The rest is cost: draw + upload + render must fit in 8.33 ms. The
    mock with `--render-ms 4 --upload-ms-per-mpx 1.5` reproduces the
    phone: the full-screen demo runs at 60.0 fps, and the box client
    (0.26 MP of damage) at 120.
- **Found while instrumenting: partial damage was wrong in direct mode.**
  This is fixed (d499449).
  - Each wl_buffer's texture got only its own commit's damage, which is
    relative to the *other* buffer.
  - So foot showed every other letter, with stale cursor blocks. The
    full-damage demo hid it, and so did the mock until `--textures`.
- Not specific to Wayland (main has it too): keys typed on Moon's keyboard
  don't reset Auto-Lock (`begin_raw_keyboard_contact` never calls
  `note_input`). The UI agent has this one.

### Direct-mode timing (`MOON_PROFILE=1` or `MOON_WAYLAND_DEBUG=1`)

Every 2 s Moon logs `wayland: direct timing N frames` with mean/max ms
for each step:

| Field | What it measures |
|---|---|
| `frame-done->commit` | The client's draw (plus socket hops) |
| `commit->sent` | Moon |
| `damage->uploaded` | The compositor's texture upload (MG_UPLOADED reply) |
| `sent->flip` | The upload, the render (`render`), and the wait for vblank |
| `flip->frame-done` | Moon's frame-callback latency |
| `vblanks/frame` | Vblanks per frame (1 means every vblank) |

The demo's fps line also carries its paint time.
`MOON_DEMO_DRAW=box` is the cheap client: a box in one band, so it
measures the display path. Phone re-check:

```
moon-phone.sh start demo debug        # animate: full-screen numbers
moon-phone.sh start demo debug box    # the same with the cheap client
```

How to read the result:
- If full-screen `frame-done->commit` is several ms and box mode holds
  120, the demo's drawing is the cost.
- If `damage->uploaded` is several ms for 2.4 MP, the compositor's
  upload is the cost.

### Phone re-check (2026-10-26, rebased on main 0001a93)

**Screen-off through the power fade:** no rejection with the X11 System
app (Wayland off), nor with the Wayland demo in direct mode. Direct mode
leaves in the same pass, the app goes to the background, and the phase
reaches `screen_off`.

**Direct-mode timing**, ms mean (max):

| | Full-screen demo | Box client |
|---|---|---|
| Frame rate | 60.3 fps (2 vblanks per frame) | **120.5 fps (1 per frame)** |
| Client draw (frame done -> commit) | 2.6 (3.7), paint 2.2 | 0.5 (1.4), paint 0.15 |
| Upload (damage -> UPLOADED) | 2.85 (4.0) for 2.4 MP | 1.2 (1.6) for 0.26 MP |
| Compositor render | 3.2 (5.0) | 2.9 (4.4) |
| Flip -> frame done | 1.0 (2.1) | 0.8 (2.1) |

The path holds 120 Hz. A full-screen redraw does not:
1.0 + 2.6 + 2.85 + 3.2 = 9.7 ms, against 8.33 ms.

The upload is about 1 ms fixed plus about 0.75 ms per MP. A cheaper draw
alone would not be enough: with the demo's draw removed, the fixed costs
still leave about 1.3 ms for the client.

Not run: `evdev` touch, screen off/on via the power key, and the switcher
card.

## Milestone 2, first part: direct display in GPU mode (built)

In GPU mode, a Wayland app in front is no longer copied through X. Each of
its `wl_shm` buffers becomes a moon-compositor surface on the client's
pool (protocol v3: the buffer's byte offset). The app's frame is the X
mirror (status strip, keyboard area) plus one image node of the client
buffer over the app area. Every commit is one frame message:
- the client's damage goes with it;
- the frame's `MG_PRESENTED` makes that buffer current, releasing the
  previous one;
- the frame callback fires at that flip, so Wayland apps are paced by real
  page flips.

The X proxy stops receiving copies meanwhile. It is brought up to date
(one `ShmPutImage` of the buffer on screen) whenever direct mode ends.

| Shown directly when | Otherwise (X proxy path) |
|---|---|
| GPU mode, protocol v3, the app in front (the keyboard sits below the app's area; the volume HUD and banners over it are patches of the mirror, cut to their rounded shape by the node's corner radius) | a key callout over the app (not a rounded shape) |
| the pool is sealed against shrinking (moon-compositor maps it: a shrinking pool would SIGBUS it) | unsealable pools (memfd without `MFD_ALLOW_SEALING`) |
| popups and subsurfaces are extra nodes, in stacking order, ARGB ones blended | |
| no snapshot pending | the home gesture, the switcher and fades: the proxy is synced before the mirror is copied, so cards show the latest frame |

**Protocol version.** Moon offers its newest version (v3) and steps down,
one version at a time to v2, only when the compositor closes the link after
the hello, twice in a row on the same socket. Pre-v3 compositors drop any
other version. Not stepping down on anything else is from the GPU agent's
review:
- a timeout or a connect error is a plain failure, retried at the top
  version;
- a socket inode that changed means a new compositor, which is asked anew.

With v2, client surfaces are never announced (not even on a reconnect),
and Wayland apps return to the X path. Verified with the stand-in's new
`--max-version 2` (`MOCK_ARGS` in `mock-run.sh`).

Verified on whale with the stand-in compositor:
- the demo's animation runs at 120.0 fps from flip-paced frame callbacks,
  with the client surfaces (1080x2217 XRGB) on the stand-in;
- the switcher card shows the frame that was on screen (the proxy is
  synced before capture);
- with the keyboard up the app stays direct (it is resized above the
  keyboard), and typing works;
- GTK's text-handle popover (an ARGB `xdg_popup`) is drawn as a blended
  node over the app in direct mode.

Next in M2:
- the key callout as a frame node too;
- `linux-dmabuf` for clients with dmabufs (planes, zero copy).

## Milestone 3: touch from evdev (built, opt-in)

`MOON_TOUCH=evdev` makes Moon read the touchscreen itself (`src/touch.rs`):
- Multi-touch protocol B: slots become begin/update/end contacts in screen
  pixels, scaled from the device's axis ranges.
- `SYN_DROPPED` ends every contact.
- The device is found by name (`MOON_TOUCH_DEVICE`, default
  `sec_touchscreen`) and is never grabbed.
- XI2 raw touches are ignored while evdev is on.

The XI2 handlers are now `touch_begin/update/end`, shared by both sources,
so gestures, keyboard rollover and app taps behave the same.

**Verified off-device.** `MOON_TOUCH_REPLAY=FIFO:MAXX,MAXY` feeds the same
path from a FIFO; `scripts/wayland/touch-replay.py` writes protocol B
events at 120 Hz. A uinput device is not an option on a desktop host,
whose X server would take its touches. These work from the replay:
- the lock-screen swipe to the keypad
- the PIN typed on the keypad
- the home swipe out of an app
- Moon's keyboard typing into a Wayland app

On the phone, `getevent -p` gives the axes: 0..1079 x 0..2339, ten slots,
the same mapping. Still untested there: the ioctls and real fingers.

## Milestone 4: Xwayland replaces headless Xorg (M4a built, opt-in)

Two stages.

**M4a: Xwayland as a drop-in X server** (built, `MOON_XWAYLAND`). Without
Xorg, Moon starts first:
1. Its Wayland server binds.
2. It starts Xwayland on one end of a socket pair (`WAYLAND_SOCKET`),
   rootful at the panel's size (`MOON_XWAYLAND_GEOMETRY`, default
   1080x2340), with `-displayfd` (`src/xwayland.rs`).
3. It serves Xwayland until the display number arrives (140 to 270 ms),
   points `DISPLAY` at it, and connects as it does to Xorg. If Xwayland
   exits, so does Moon.

Everything on Moon's X side is unchanged: its windows, the X mirror on the
GPU path, XTEST keys, the Wayland proxies, apps launched with DISPLAY.

The Wayland server knows its Xwayland client (`ClientInfo::xwayland`):
- **Scale:** 1 for its output, preferred buffer scale and configure.
- **Window:** its one toplevel is the whole panel, fullscreen and active,
  and is not one of Moon's apps.
- **Buffers and frames:** its buffers go straight back, because Moon
  mirrors the X root instead. Its frame callbacks follow the panel.
- **Touch:** Moon reads touch from evdev (M3). Every contact also goes to
  Xwayland's root as wl_touch, just as Xorg read the device itself.
  Xwayland turns it into XI2 touch for the X window under it.
- **Keyboard:** entered on Xwayland's root for good, and never sent a key.
  Xwayland delivers no key events, XTEST's included, without Wayland
  keyboard focus. The keymap it gets is Moon's US map; keys still come
  through XTEST.
- **Deep suspend:** off under Xwayland. Its helper replaces Xorg.
- **Tier:** needs the GPU path. Xwayland has no DPMS, and moon-compositor
  powers the panel.

**Verified off-device** on whale with Alpine 3.24's Xwayland 24.1.14 (the
chroot's version), in a container: `scripts/wayland/xwayland-run.sh`, with
`xwayland-podman.sh` as `MOON_XWAYLAND`.
- Moon renders, unlocks and runs its gestures on Xwayland, in preview and
  through the GPU path (the mock compositor's frame mirrors Xwayland's root).
- vimbrowser's x86 build, run with the A26 launcher's flags, works as an X11
  app under it:
  - touches reach the page at the right CSS coordinates, with clicks
  - Moon's keyboard types into a focused input
  - the home swipe backgrounds it
- Opening Moon's keyboard from outside (`ctl keyboard show`) blurs
  vimbrowser's input. It does the same on Xvfb, so it isn't Xwayland; on
  the phone the Browser asks for the keyboard itself.
- A whale lesson: a container that shares the host's `/tmp/.X11-unix` but
  not its lock files let Xwayland take `:0` and replace the desktop's
  socket file. The wrapper now only takes a display nothing on the host
  uses, and refuses a pinned one that is in use.

**Packaging.** The chroot needs three pinned Alpine v3.24 packages:
`xwayland-24.1.14-r0`, `libtirpc-nokrb-1.3.5-r1` and
`libtirpc-conf-1.3.5-r1`. I checked them against the phone's own package
database: every other dependency is already installed as an unversioned
soname, so nothing Xorg uses is upgraded. `moon-phone.sh xwayland-install`
adds them offline, and apk verifies them with the chroot's keys.

**Session** (XWAYLAND=1 in /data/adb/moon/config, off by default):
- **Takeover:** `a26-xorg-takeover.sh` chooses it only when the GPU
  compositor is ready and Xwayland is installed.
- **Session holder:** `a26-xorg-persistent-session.sh --xwayland` starts no
  X server. It writes `XORG_READY=1` with `XSERVER=xwayland` and holds the
  session.
- **Supervisor start:** `moon-boot-supervisor.sh` starts Moon with
  `MOON_XWAYLAND=/usr/bin/Xwayland`, display `:0` and the Wayland
  directory. Its liveness loop accepts the held session in place of Xorg.
- **Fallback to Xorg:** if Moon doesn't come up on Xwayland, at start or
  when a recovery fails:
  1. `fall_back_to_xorg` marks the boot (`moon-gpu/xwayland-fallback`) and
     ends the held session.
  2. The takeover loop starts the same GPU session with headless Xorg.
  3. Moon starts again on it, with Wi-Fi left up.
  4. Only if that fails too is Android restored, as before.
- **Compositor failure:** the GPU fallback (`moon-gpu-session.sh`) ends the
  held session too, and Xorg comes back on the panel.
- **Tested so far:** the supervisor's new functions ran against a fake
  takeover on whale.
- **Not installed yet:** these boot scripts reach the phone only through
  `scripts/gpu/install.sh`, after the device test below.

**Device test** (no boot-path change):
1. `moon-phone.sh install` (Moon)
2. `moon-phone.sh xwayland-install`
3. `moon-phone.sh xwayland-start [debug]`

`moon-xwayland-test.sh` needs the GPU session. Like the earlier handoff
experiment, it SIGSTOPs the supervisor and SIGKILLs the takeover chain, so
no trap restores Android or stops Wi-Fi. It stops Moon and the headless
Xorg, and starts Moon on Xwayland next to the running moon-compositor. The
way back is `moon-phone.sh reboot`. To check there:
- lock and unlock with real touches
- the Browser (touch, keyboard, pages)
- the Wayland demo
- screen off and on
- frame pacing

**Xwayland's own root copy is switched off.** Rootful Xwayland double-buffers
its root: on every damaged frame it copies the damage into one of two
alternating `wl_shm` buffers (seen while the Browser scrolls: 56 attaches
in 3 s). It even does this without a configure. On the phone that would
be a second CPU copy (about 1.6 ms per full frame) beside the mirror's,
for buffers nobody shows.
- Moon now never configures Xwayland's root and never answers its frame
  callbacks.
- So after one buffer at startup Xwayland makes no more copies (0 attaches
  while scrolling).
- X input, rendering and focus don't need the root mapped. Chromium's
  `requestAnimationFrame` still runs at 59.9 fps with or without the
  callbacks.
- An earlier "presentation is free" measurement was wrong. The Browser in
  it had crashed with SIGBUS, because whale's `/tmp` quota filled up, so
  nothing was drawing. The test Browser now gets a private `TMPDIR`.

### dmabuf-backed wl_shm pools (built, opt-in per client)

A `wl_shm` buffer in a memfd costs moon-compositor an upload on every
frame (`glTexSubImage2D`: about 1 ms plus 0.75 ms per MP on the phone).
That is what held the full-screen demo at 60 fps. The X mirror avoids it
because it is a dmabuf, sampled in place.

A client may now back its `wl_shm` pool with a dma-heap buffer instead
(`/dev/dma_heap/system`, readable by all on the phone):
- **Pool:** Moon sees that the fd is a dmabuf and treats the pool as
  unshrinkable (a dmabuf cannot shrink).
- **Announce:** Moon sends each whole-buffer `wl_buffer` (offset 0) as an
  `MG_SURFACE_DMABUF` client surface, which the compositor imports as an
  EGLImage, with no upload.
- **Cache flush:** before each damage, Moon cleans the CPU caches with
  `DMA_BUF_IOCTL_SYNC`, as the compositor does for the mirror.
- **Other buffers:** only a whole buffer (offset 0) with 64-byte rows takes
  this path. The Mali may refuse other pitches, and a v3 compositor would
  not even report the failure. Anything else keeps the copy path; a dmabuf
  maps like a memfd there.
- **Planes:** the compositor owns the plane rules (main f79d6e8). A dmabuf
  goes on a plane only with a pitch that is a multiple of 64 and offset 0;
  otherwise the GPU composites it, with one log line per surface.

The demo allocates one dma-heap buffer per `wl_buffer`, with rows padded
to 64 bytes (4352 for 1080 pixels, like the X mirror), with
`MOON_DEMO_DMABUF=1` (`moon-phone.sh start demo debug dmabuf`). Expected on
the phone: the full-screen demo at about 7.2 ms a frame (draw 2.6 + cache
clean 0.4 + render 3.2 + wakeup 1.0), so 120 fps. This needs the phone:
whale's dma heap is root-only.

This is also what makes M4b worth doing. Xwayland's per-window buffers
are memfds, so rootless would add the upload per frame that the mirror
avoids. M4b therefore starts with an Xwayland that allocates its shm
pixmaps from the dma heap.

### On the phone (2026-10-26/27, xwayland-m4 ab83361, then 67d3a40)

**M4a, Moon on Xwayland** (`moon-xwayland-test.sh`, GPU session, planes on):
- **Startup:** Xwayland 24.1.14 (Alpine's, without glamor) reported `:0`
  after 187 ms. moon-compositor stayed up; Moon read `sec_touchscreen`
  itself.
- **Touch:** Moon unlocked. Injected evdev touches scrolled the Browser, an
  X11 app on Xwayland.
- **Keyboard:** tapping the address bar made the Browser ask for Moon's
  keyboard (purpose url), and Moon's keys typed into it through XTEST.
- **Screen-off:** went through cleanly with the Browser in front (CRTC off
  through moon-compositor).
- **Overnight:** the session then ran about 9 hours with 7 screen
  transitions, no rejection and no error, until a reboot restored the
  normal session.
- **Not yet done:** real fingers (the user was away).

**dmabuf pools, full-screen demo** (normal session, same Moon):

| | memfd pool | dma-heap buffers |
|---|---|---|
| Frame rate | 60.3 fps (2 vblanks a frame) | **96 fps** (1.3 vblanks a frame) |
| Upload (damage -> UPLOADED) | 2.86 ms | **0.17 ms** |
| Compositor render | 3.2 ms | **0.00 ms** (on a plane) |
| Sent -> flip | 12.9 ms | 4.2 ms |
| Client paint / frame done -> commit | 2.0 / 2.5 ms | 3.8 / 4.6 ms |
| Moon (commit -> sent, cache flush) | 0.06 ms | 0.8 ms |

The display path is no longer the limit. What is left for 120 Hz:
- **Painting is slower:** the demo paints about twice as slowly into
  dma-heap memory as into a memfd. The heap's CPU mapping may be
  write-combined or uncached. To check: `system` vs `system-uncached`,
  and the mapping's attributes.
- **The cache flush:** Moon's `DMA_BUF_IOCTL_SYNC` costs 0.8 ms; it could
  be bounded to the damage.

Also seen: twice, right after Moon restarted, an animating direct-mode
client got no frames until a screen off/on cycle.
- **Cause:** Moon woke the touchscreen and backlight at startup, but not
  the display pipeline (the compositor's CRTC, or Xorg's DPMS) that the
  Moon before it had turned off. This affects main too: any restart while
  the screen is asleep.
- **Status:** fixed in f9c0dce. Reproduced with
  `mock-compositor.py --powered-off`: 0 frames flipped before the fix, 37
  after.

**Next device checks** for the remaining paint and flush cost:
- `moon-phone.sh heapbench` measures memfd, `system` and `system-uncached`:
  paint, read-back, each `DMA_BUF_IOCTL_SYNC` variant and the paint after
  it, the mapping's VmFlags, and whether Android's partial sync exists. It
  needs the phone: whale's heap is root-only and x86 is cache-coherent.
- `start demo debug dmabuf sync-write` (clean only, no invalidate) and
  `sync-none` compare Moon's flush variants end to end.

**Measured on the phone** (main 4dfbcab + xwayland-m4):
- **The heap is cached.** heapbench: the `system` heap paints and reads
  like a memfd (paint 0.70 vs 0.72 ms, read 1.87 vs 1.97 ms for 9.6 MB).
  `system-uncached` is slower (paint 1.07, read 5.6 ms). The mapping is
  not the cause of the slower paint.
- **There is no partial sync** (every request number returns ENOTTY), so
  a flush always covers the whole buffer. Without an importer a sync
  costs nothing; with the compositor holding the buffer it is real.
- **Demo, end to end:**

  | | Moon's flush | fps |
  |---|---|---|
  | READ\|WRITE (old default) | 0.71 ms | 99 |
  | WRITE only | 0.49 ms | 98 |
  | none | 0.07 ms | 103 |

  The demo's paint stays at 3.5 ms in every variant, against 2.07 ms on
  the memfd path. So the flush is not what slows the paint either.
- **Default now:** WRITE only. It is the clean the device needs after
  CPU writes. `sync-rw` and `sync-none` stay for measurement.
- **Still open:** why the demo paints slower in dma-heap buffers while the
  display scans the other one out. Suspects are memory-bandwidth
  contention with the plane scanout, or CPU frequency.

### Merge gate: `scripts/wayland/m4-boot-test.sh` (about 15 minutes)

xwayland-m4 changes boot scripts, so it merges only after three real boots
through the supervisor:
1. **Boot A, XWAYLAND=1:** Moon must run on Xwayland, with no Xorg.
2. **Boot B, broken Xwayland:** `XWAYLAND_BIN=/bin/false`. Moon exits at
   once, and the supervisor must fall back to headless Xorg in the same
   boot.
3. **Boot C, XWAYLAND unset:** the session must be main's.

Around the boots:
- **Preflight:** the script refuses unless the phone's four boot scripts
  are main's, by sha256.
- **Backup:** it backs up the config, those scripts and Moon to the phone
  and to the host.
- **Install:** it installs the branch's scripts with `install.sh`'s paths
  and modes.
- **Afterwards:** it restores everything (or keeps the scripts with
  `--keep`), and a last boot must come up in Moon.
- **On a failed check:** it restores first, then stops.

**M4b (next): rootless Xwayland, Moon as its X window manager.**
- Each X window (the Browser, and Moon's own windows at first) has its own
  wl_surface (`xwayland_shell_v1`). Moon shows it directly, as M2 does for
  native apps, so the X mirror and its copy go away.
- Moon's own windows can stay X windows under Xwayland during the
  transition and become compositor layers later.
- Snapshots become compositor copies of client surfaces.

The CEF browser stays an X11 app under Xwayland:
- vimbrowser passes `--ozone-platform=x11` and has its own X11 window code
  (`src/browser_window*.cc`);
- its CEF 147 build does include Chromium's Ozone Wayland backend;
- porting vimbrowser's window code would make it native, but that's a
  vimbrowser change for later.

## Requests for moon-compositor (M2)

1. **Client memfd surfaces, which may be unsealed:** check `F_SEAL_SHRINK`
   on `MG_SURFACE_SHM` fds that aren't Moon's own, or copy under a SIGBUS
   guard.
2. **A per-surface "no longer sampled" signal**, so Moon can release
   dmabuf client buffers exactly. A dmabuf surface is sampled in place, so
   its buffer must stay untouched until no queued or current frame uses it.
   Moon can infer this from `MG_PRESENTED` of a later frame that no longer
   names the surface; an explicit event would also cover the
   compositor-internal copies.
3. **Plane rules for a full-screen client dmabuf** in the app area under
   Moon's status strip, keyboard and banner layers: direct scanout of the
   foreground app is the main payoff of M2.
4. **Later, for GPU-rendering clients:** acquire fences per surface
   (`IN_FENCE_FD` for their buffers). CPU clients don't need them.
