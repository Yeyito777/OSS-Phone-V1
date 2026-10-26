# Moon as a Wayland compositor: decision, plan and status

Status: milestone 1 built and verified off-device (branch `wayland-moon`),
opt-in, off by default. Companion documents: `notes/wayland/DESIGN.md` (the
measurements that led here) and `notes/gpu/DESIGN.md` (moon-compositor,
the display path every milestone builds on).

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
| `wl_data_device_manager` | 3 | Stub: no clipboard yet |
| `zwp_text_input_manager_v3` | 1 | Enable/disable and the content purpose drive Moon's keyboard |

Not yet offered: `linux-dmabuf` (M2: with the X proxy it would mean Xorg
reading write-combined memory), `wp_presentation` (M2), `wp_viewporter`,
pointer, clipboard and primary selection, xdg-activation.

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

Not verified yet: the phone (`scripts/wayland/moon-phone.sh`).

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
