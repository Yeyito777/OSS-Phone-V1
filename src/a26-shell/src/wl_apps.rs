//! Wayland apps in Moon, milestone 1: every Wayland surface is shown through
//! an X proxy window.
//!
//! A toplevel gets a top-level X window that Moon manages exactly like an
//! X11 app's window (launch, focus, backgrounding, freezer, snapshots, the
//! GPU mirror and the software path all work unchanged); popups and
//! subsurfaces get child windows of their parent's. Each commit is copied
//! by Xorg straight from the client's shared memory (MIT-SHM `AttachFd` on
//! the pool, `ShmPutImage` of the damage), the same one copy an X11 app's
//! drawing costs, and the buffer is released when Xorg reports the copy
//! done. Touches on a proxy go to the client as wl_touch, the window's
//! geometry and mapping become xdg configures, and X focus becomes keyboard
//! and text-input focus.
//!
//! Everything here is one-way on Moon's long-lived X connection: requests
//! are queued and flushed, answers arrive as events, nothing waits.

use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::os::fd::{AsFd, RawFd};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use wayland_server::Resource as _;
use wayland_server::backend::ObjectId;
use wayland_server::protocol::wl_buffer::WlBuffer;
use x11rb::COPY_DEPTH_FROM_PARENT;
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::shm::ConnectionExt as _;
use x11rb::protocol::xproto::{
    ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask, ImageFormat,
    NotifyDetail, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;

use crate::compositor::SurfaceId;
use crate::gfx::RectF;
use crate::gpu::{Direct, GpuBackend};
use crate::keyboard::{KeyboardInput, KeyboardPurpose};
use crate::model::AppId;
use crate::present::Damage;
use crate::wayland::{self, Placement, ShmBuffer, ShmPool, SurfaceKey, ToplevelId, WaylandServer};

/// Frame callbacks fire at most once per refresh.
const FRAME_INTERVAL: Duration = Duration::from_micros(8_333);

/// What the main loop does for the Wayland side, after a dispatch.
#[derive(Debug)]
pub enum Action {
    /// A toplevel's proxy has its first content: manage it like a mapping
    /// X11 window of `owner` (or of the app Moon launched).
    Manage { window: u32, owner: Option<AppId> },
    /// The focused app's text field gained or lost focus.
    Keyboard {
        window: Option<u32>,
        show: Option<KeyboardPurpose>,
    },
}

struct Segment {
    seg: u32,
    generation: u32,
    pool: Arc<ShmPool>,
}

struct Proxy {
    window: u32,
    toplevel: Option<ToplevelId>,
    owner: Option<AppId>,
    /// Managed by Moon's window registry (toplevels, after first content).
    managed: bool,
    /// X MapNotify seen (toplevels: Moon decides; children: we map them).
    mapped: bool,
    has_content: bool,
    focused: bool,
    /// The last content was copied and the client waits for its frame.
    frame_pending: bool,
    /// Popups and subsurfaces: where the client put them.
    placement: Option<Placement>,
    /// The part of the buffer shown (xdg window geometry), buffer pixels.
    crop: Option<wayland::Rect>,
    /// The buffer on screen. Held (not released) until the next one has
    /// been copied, so exposed areas can be repainted from it.
    current: Option<(WlBuffer, Arc<ShmBuffer>)>,
}

pub struct WaylandApps {
    server: WaylandServer,
    root: u32,
    gc: u32,
    width: u16,
    height: u16,
    segments: HashMap<u64, Segment>,
    proxies: HashMap<SurfaceKey, Proxy>,
    windows: HashMap<u32, SurfaceKey>,
    /// Buffers whose ShmPutImage has not completed yet, per window.
    in_flight: HashMap<u32, VecDeque<(WlBuffer, Arc<ShmBuffer>)>>,
    /// Exposed window areas to repaint from the current buffer.
    exposed: Vec<(u32, wayland::Rect)>,
    next_frame: Instant,
    /// The window a touch went down on (X keeps delivering to it).
    touch_window: Option<u32>,
    /// Last size of each toplevel proxy, for configures without a resize.
    sizes: HashMap<u32, (u16, u16)>,
    /// Parent of each popup or subsurface.
    parents: HashMap<SurfaceKey, SurfaceKey>,
    actions: Vec<Action>,
    /// MOON_WAYLAND_DEBUG=1 logs every protocol event but commits.
    debug: bool,
    /// MOON_PROFILE (or debug): where each direct frame's time goes.
    timing: Option<DirectTiming>,
    /// GPU mode: the toplevel whose buffers go straight to moon-compositor
    /// (milestone 2), and its proxy's X content is stale meanwhile.
    direct: Option<SurfaceKey>,
    /// A compositor surface per client buffer shown directly.
    client_surfaces: HashMap<ObjectId, (WlBuffer, SurfaceId)>,
    /// Per such buffer: the Wayland surface it belongs to and what its
    /// compositor copy lacks. A commit's damage is relative to the
    /// surface's last buffer, so with several buffers each copy falls
    /// behind by every commit since it was last shown.
    behind: HashMap<ObjectId, (SurfaceKey, Vec<wayland::Rect>)>,
    /// Buffers sent in a direct frame, waiting for that frame's flip.
    presenting: Vec<(u32, SurfaceKey, WlBuffer, Arc<ShmBuffer>)>,
    /// Direct commits whose buffer is not yet in the proxy.
    stale: Vec<SurfaceKey>,
    /// Commits to show directly once the GPU backend is at hand.
    direct_commits: Vec<(SurfaceKey, WlBuffer, Arc<ShmBuffer>, Vec<wayland::Rect>)>,
    /// Per directly shown surface: its compositor surface and buffer size.
    shown: HashMap<SurfaceKey, (SurfaceId, i32, i32)>,
    /// The directly shown app's surfaces moved, appeared or went: resend.
    direct_dirty: bool,
    /// Moon's overlay windows over the app: (rectangle, corner radius).
    overlays: Vec<(RectF, f32)>,
    /// Buffers the compositor could not show (v4 MG_SURFACE_FAILED).
    unshareable: std::collections::HashSet<ObjectId>,
    /// While settling flipped buffers: that flip's time and counter.
    flip: Option<(u64, u64)>,
}

impl WaylandApps {
    pub fn new<C: Connection>(
        conn: &C,
        root: u32,
        width: u16,
        height: u16,
        runtime_dir: &Path,
    ) -> Result<Self, Box<dyn Error>> {
        let server = bind_server(runtime_dir, width, height)?;
        Self::with_server(conn, root, width, height, server)
    }

    /// Around a server bound before X was up (Xwayland's, milestone 4).
    pub fn with_server<C: Connection>(
        conn: &C,
        root: u32,
        width: u16,
        height: u16,
        server: WaylandServer,
    ) -> Result<Self, Box<dyn Error>> {
        let gc = conn.generate_id()?;
        conn.create_gc(gc, root, &CreateGCAux::new().graphics_exposures(0))?;
        Ok(Self {
            server,
            root,
            gc,
            width,
            height,
            segments: HashMap::new(),
            proxies: HashMap::new(),
            windows: HashMap::new(),
            in_flight: HashMap::new(),
            exposed: Vec::new(),
            next_frame: Instant::now(),
            touch_window: None,
            sizes: HashMap::new(),
            parents: HashMap::new(),
            actions: Vec::new(),
            debug: std::env::var_os("MOON_WAYLAND_DEBUG").is_some(),
            timing: (std::env::var_os("MOON_WAYLAND_DEBUG").is_some()
                || std::env::var_os("MOON_PROFILE").is_some())
            .then(DirectTiming::default),
            direct: None,
            client_surfaces: HashMap::new(),
            behind: HashMap::new(),
            presenting: Vec::new(),
            stale: Vec::new(),
            direct_commits: Vec::new(),
            shown: HashMap::new(),
            direct_dirty: false,
            overlays: Vec::new(),
            unshareable: std::collections::HashSet::new(),
            flip: None,
        })
    }

    /// The environment apps need to find this server.
    pub fn client_env(&self) -> Vec<(String, String)> {
        let path = self.server.socket_path();
        let mut env = vec![("WAYLAND_DISPLAY".into(), path.display().to_string())];
        if let Some(dir) = path.parent() {
            env.push(("XDG_RUNTIME_DIR".into(), dir.display().to_string()));
        }
        env
    }

    pub fn poll_fds(&mut self) -> [RawFd; 2] {
        self.server.poll_fds()
    }

    pub fn is_proxy(&self, window: u32) -> bool {
        self.windows.contains_key(&window)
    }

    /// Whether an app's own buffers are on screen (GPU direct mode).
    pub fn showing_direct(&self) -> bool {
        self.direct.is_some()
    }

    /// Runs the clients' requests and turns them into X requests and
    /// actions for the main loop.
    pub fn dispatch<C: Connection>(&mut self, conn: &C) -> Vec<Action> {
        self.server.dispatch();
        for event in self.server.take_events() {
            if self.debug && !matches!(event, wayland::Event::Commit { .. }) {
                eprintln!("wayland event: {event:?}");
            }
            if let Err(error) = self.apply(conn, event) {
                eprintln!("wayland: proxy request failed: {error}");
            }
        }
        if let Err(error) = self.repaint_exposed(conn) {
            eprintln!("wayland: repaint failed: {error}");
        }
        // Pools nobody uses any more (only our segment holds them).
        let unused: Vec<u64> = self
            .segments
            .iter()
            .filter(|(_, segment)| Arc::strong_count(&segment.pool) == 1)
            .map(|(id, _)| *id)
            .collect();
        for id in unused {
            if let Some(segment) = self.segments.remove(&id) {
                let _ = conn.shm_detach(segment.seg);
            }
        }
        std::mem::take(&mut self.actions)
    }

    fn apply<C: Connection>(
        &mut self,
        conn: &C,
        event: wayland::Event,
    ) -> Result<(), Box<dyn Error>> {
        match event {
            wayland::Event::ToplevelCreated { id, surface, pid } => {
                let window = conn.generate_id()?;
                let content_height = self.height.saturating_sub(crate::status_bar::HEIGHT).max(1);
                conn.create_window(
                    COPY_DEPTH_FROM_PARENT,
                    window,
                    self.root,
                    0,
                    crate::status_bar::HEIGHT as i16,
                    self.width,
                    content_height,
                    0,
                    WindowClass::INPUT_OUTPUT,
                    0,
                    &CreateWindowAux::new()
                        .background_pixmap(x11rb::NONE)
                        .event_mask(proxy_events()),
                )?;
                conn.change_property8(
                    x11rb::protocol::xproto::PropMode::REPLACE,
                    window,
                    x11rb::protocol::xproto::AtomEnum::WM_NAME,
                    x11rb::protocol::xproto::AtomEnum::STRING,
                    b"wayland",
                )?;
                let owner = pid.and_then(owner_from_pid);
                eprintln!(
                    "wayland: toplevel {} (pid {}) as X window 0x{window:08x}, app {}",
                    id.0,
                    pid.unwrap_or(0),
                    owner.map(AppId::display_name).unwrap_or("(launching)")
                );
                self.add_proxy(surface, window, Some(id), owner);
                // The first configure: the size Moon gives apps.
                self.server.configure_toplevel(
                    id,
                    i32::from(self.width),
                    i32::from(content_height),
                    false,
                    false,
                );
            }
            wayland::Event::ToplevelTitle { id, title } => {
                eprintln!("wayland: toplevel {} is {title:?}", id.0);
            }
            wayland::Event::SurfaceCreated { surface, placement }
            | wayland::Event::SurfaceMoved { surface, placement } => {
                self.direct_dirty = true;
                self.place_child(conn, surface, placement)?;
            }
            wayland::Event::Commit {
                surface,
                buffer,
                resource,
                damage,
                geometry,
            } => self.commit(conn, surface, buffer, resource, &damage, geometry)?,
            wayland::Event::Unmapped { surface } => {
                self.direct_dirty = true;
                self.shown.remove(&surface);
                if let Some(proxy) = self.proxies.get_mut(&surface) {
                    proxy.has_content = false;
                    if proxy.toplevel.is_none() {
                        conn.unmap_window(proxy.window)?;
                    }
                    if let Some((buffer, _)) = proxy.current.take() {
                        self.server.release(&buffer);
                    }
                }
            }
            wayland::Event::Destroyed { surface, toplevel } => {
                self.direct_dirty = true;
                self.shown.remove(&surface);
                self.stale.retain(|key| *key != surface);
                if let Some(id) = toplevel {
                    eprintln!("wayland: toplevel {} closed", id.0);
                }
                self.parents.remove(&surface);
                if let Some(proxy) = self.proxies.remove(&surface) {
                    self.windows.remove(&proxy.window);
                    self.sizes.remove(&proxy.window);
                    // Every buffer we held goes back to the client.
                    for (buffer, _) in self.in_flight.remove(&proxy.window).unwrap_or_default() {
                        self.server.release(&buffer);
                    }
                    if let Some((buffer, _)) = proxy.current {
                        self.server.release(&buffer);
                    }
                    if self.touch_window == Some(proxy.window) {
                        self.touch_window = None;
                    }
                    conn.destroy_window(proxy.window)?;
                }
            }
            wayland::Event::TextInput {
                toplevel, enabled, ..
            } if !enabled => {
                let window = toplevel.and_then(|id| self.toplevel_window(id));
                self.actions.push(Action::Keyboard { window, show: None });
            }
            wayland::Event::TextInput {
                toplevel, purpose, ..
            } => {
                let window = toplevel.and_then(|id| self.toplevel_window(id));
                self.actions.push(Action::Keyboard {
                    window,
                    show: Some(purpose),
                });
            }
        }
        Ok(())
    }

    fn add_proxy(
        &mut self,
        surface: SurfaceKey,
        window: u32,
        toplevel: Option<ToplevelId>,
        owner: Option<AppId>,
    ) {
        self.windows.insert(window, surface);
        self.proxies.insert(
            surface,
            Proxy {
                window,
                toplevel,
                owner,
                managed: false,
                mapped: false,
                has_content: false,
                focused: false,
                frame_pending: false,
                placement: None,
                crop: None,
                current: None,
            },
        );
    }

    fn toplevel_window(&self, id: ToplevelId) -> Option<u32> {
        self.proxies
            .values()
            .find(|proxy| proxy.toplevel == Some(id))
            .map(|proxy| proxy.window)
    }

    /// Popups and subsurfaces are child windows of their parent's proxy.
    fn place_child<C: Connection>(
        &mut self,
        conn: &C,
        surface: SurfaceKey,
        placement: Placement,
    ) -> Result<(), Box<dyn Error>> {
        let parent = match placement {
            Placement::Popup { parent, .. } | Placement::Subsurface { parent, .. } => parent,
            Placement::Toplevel(_) => return Ok(()),
        };
        let Some(parent_window) = self.proxies.get(&parent).map(|proxy| proxy.window) else {
            return Ok(());
        };
        if let Some(proxy) = self.proxies.get_mut(&surface) {
            proxy.placement = Some(placement);
            let window = proxy.window;
            let (x, y) = self.child_position(surface);
            conn.configure_window(
                window,
                &ConfigureWindowAux::new()
                    .x(x)
                    .y(y)
                    .stack_mode(x11rb::protocol::xproto::StackMode::ABOVE),
            )?;
            return Ok(());
        }
        let window = conn.generate_id()?;
        conn.create_window(
            COPY_DEPTH_FROM_PARENT,
            window,
            parent_window,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_OUTPUT,
            0,
            &CreateWindowAux::new()
                .background_pixmap(x11rb::NONE)
                .event_mask(proxy_events()),
        )?;
        let owner = self.proxies.get(&parent).and_then(|proxy| proxy.owner);
        self.add_proxy(surface, window, None, owner);
        self.parents.insert(surface, parent);
        if let Some(proxy) = self.proxies.get_mut(&surface) {
            proxy.placement = Some(placement);
        }
        Ok(())
    }

    /// A child window's position inside its parent's window. The parent's
    /// window shows its buffer from its crop origin; a popup is placed
    /// relative to the parent's window geometry (that same origin), a
    /// subsurface relative to the parent's buffer.
    fn child_position(&self, surface: SurfaceKey) -> (i32, i32) {
        let Some(proxy) = self.proxies.get(&surface) else {
            return (0, 0);
        };
        match proxy.placement {
            Some(Placement::Popup { x, y, .. }) => (x, y),
            Some(Placement::Subsurface { parent, x, y }) => {
                let origin = self
                    .proxies
                    .get(&parent)
                    .and_then(|parent| parent.crop)
                    .map(|crop| (crop.x, crop.y))
                    .unwrap_or((0, 0));
                (x - origin.0, y - origin.1)
            }
            _ => (0, 0),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn commit<C: Connection>(
        &mut self,
        conn: &C,
        surface: SurfaceKey,
        buffer: Arc<ShmBuffer>,
        resource: WlBuffer,
        damage: &[wayland::Rect],
        geometry: Option<wayland::Rect>,
    ) -> Result<(), Box<dyn Error>> {
        for (owner, rects) in self.behind.values_mut() {
            if *owner == surface {
                add_damage(rects, damage);
            }
        }
        let Some((window, is_child, has_content, old_crop)) =
            self.proxies.get(&surface).map(|proxy| {
                (
                    proxy.window,
                    proxy.toplevel.is_none(),
                    proxy.has_content,
                    proxy.crop,
                )
            })
        else {
            self.server.release(&resource);
            return Ok(());
        };
        let crop = geometry.and_then(|rect| rect.clip(buffer.width, buffer.height));
        let shown = crop.unwrap_or(wayland::Rect::new(0, 0, buffer.width, buffer.height));
        let crop_changed = has_content && crop != old_crop;
        if let Some(proxy) = self.proxies.get_mut(&surface) {
            proxy.crop = crop;
        }
        if is_child && (!has_content || crop_changed) {
            // A child shows at its window geometry's (or buffer's) size.
            let (x, y) = self.child_position(surface);
            conn.configure_window(
                window,
                &ConfigureWindowAux::new()
                    .x(x)
                    .y(y)
                    .width(shown.width.clamp(1, 8192) as u32)
                    .height(shown.height.clamp(1, 8192) as u32),
            )?;
            if !has_content {
                conn.map_window(window)?;
            }
        }
        if crop_changed {
            // Subsurfaces are placed relative to this buffer's origin.
            let children: Vec<SurfaceKey> = self
                .parents
                .iter()
                .filter(|(_, parent)| **parent == surface)
                .map(|(child, _)| *child)
                .collect();
            for child in children {
                if let Some(child_window) = self.proxies.get(&child).map(|proxy| proxy.window) {
                    let (x, y) = self.child_position(child);
                    conn.configure_window(child_window, &ConfigureWindowAux::new().x(x).y(y))?;
                }
            }
        }
        if self.direct.is_some() && self.direct == self.root_of(surface) {
            // Shown directly by moon-compositor (update_direct sends it);
            // the X window keeps its place and size for input.
            if let Some(timing) = self.timing.as_mut() {
                timing.commit_rx.get_or_insert_with(now_ns);
            }
            self.direct_commits
                .push((surface, resource, buffer, damage.to_vec()));
            self.note_content(surface, window);
            return Ok(());
        }
        let seg = self.segment(conn, &buffer.pool)?;
        let whole = [shown];
        let rects: Vec<wayland::Rect> = (if crop_changed { &whole[..] } else { damage })
            .iter()
            .filter_map(|rect| intersect(*rect, shown))
            .collect();
        let last = rects.len().saturating_sub(1);
        for (index, rect) in rects.iter().enumerate() {
            conn.shm_put_image(
                window,
                self.gc,
                (buffer.stride / 4).clamp(1, 65535) as u16,
                buffer.height.clamp(1, 65535) as u16,
                rect.x as u16,
                rect.y as u16,
                rect.width as u16,
                rect.height as u16,
                clamp16(rect.x - shown.x),
                clamp16(rect.y - shown.y),
                24,
                ImageFormat::Z_PIXMAP.into(),
                index == last,
                seg,
                buffer.offset as u32,
            )?;
        }
        if rects.is_empty() {
            // Nothing to copy: it is the buffer on screen at once.
            self.make_current(surface, resource, buffer);
        } else {
            self.in_flight
                .entry(window)
                .or_default()
                .push_back((resource, buffer));
        }
        self.note_content(surface, window);
        Ok(())
    }

    /// A surface has content: a toplevel's first content puts its proxy
    /// under Moon's management.
    fn note_content(&mut self, surface: SurfaceKey, window: u32) {
        let Some(proxy) = self.proxies.get_mut(&surface) else {
            return;
        };
        let first = !proxy.has_content;
        proxy.has_content = true;
        if first && proxy.toplevel.is_some() && !proxy.managed {
            proxy.managed = true;
            self.actions.push(Action::Manage {
                window,
                owner: proxy.owner,
            });
        }
    }

    /// The toplevel a surface belongs to.
    fn root_of(&self, mut key: SurfaceKey) -> Option<SurfaceKey> {
        for _ in 0..16 {
            if self.proxies.get(&key)?.toplevel.is_some() {
                return Some(key);
            }
            key = *self.parents.get(&key)?;
        }
        None
    }

    /// A surface and its popups and subsurfaces, parents first, siblings in
    /// creation order (their stacking order).
    fn tree(&self, root: SurfaceKey) -> Vec<SurfaceKey> {
        let mut order = vec![root];
        let mut index = 0;
        while index < order.len() && order.len() < 64 {
            let parent = order[index];
            let mut children: Vec<SurfaceKey> = self
                .parents
                .iter()
                .filter(|(_, p)| **p == parent)
                .map(|(child, _)| *child)
                .collect();
            children.sort();
            order.extend(children);
            index += 1;
        }
        order
    }

    /// Where a surface's window shows its first pixel on the screen.
    fn screen_origin(&self, key: SurfaceKey) -> (i32, i32) {
        match self.parents.get(&key) {
            Some(parent) => {
                let (px, py) = self.screen_origin(*parent);
                let (x, y) = self.child_position(key);
                (px + x, py + y)
            }
            None => (0, i32::from(crate::status_bar::HEIGHT)),
        }
    }

    /// GPU mode, once per loop iteration after `dispatch` (and again after
    /// the compositor's flips are read): shows the app in `window` (the
    /// foreground app, when nothing of Moon's covers it) from its own
    /// buffers instead of through its X proxies, and settles buffers whose
    /// frames reached the panel. Leaving direct mode (or `window` None, as
    /// before a snapshot) first brings the proxies up to date, so the X
    /// mirror and snapshots show the app's last frame.
    pub fn update_direct<C: Connection>(
        &mut self,
        conn: &C,
        gpu: Option<&mut GpuBackend>,
        window: Option<u32>,
        overlays: &[(RectF, f32)],
    ) {
        let Some(gpu) = gpu else {
            return;
        };
        // Surfaces the compositor gave up on: their apps go back to X.
        for failed in gpu.take_failed() {
            let buffers: Vec<ObjectId> = self
                .client_surfaces
                .iter()
                .filter(|(_, (_, surface))| *surface == failed)
                .map(|(id, _)| id.clone())
                .collect();
            for id in buffers {
                self.client_surfaces.remove(&id);
                self.behind.remove(&id);
                self.unshareable.insert(id);
            }
            self.direct_dirty = true;
        }
        self.unshareable.retain(|id| {
            self.proxies
                .values()
                .any(|p| p.current.as_ref().is_some_and(|(b, _)| &b.id() == id))
                || self.client_surfaces.contains_key(id)
        });
        let presented = gpu.presented_serial();
        let (flip_ns, flip_sequence) = gpu.presented_at();
        if let Some(timing) = self.timing.as_mut() {
            timing.presented(presented, flip_ns, flip_sequence, gpu);
        }
        self.flip = Some((flip_ns, u64::from(flip_sequence)));
        let mut index = 0;
        while index < self.presenting.len() {
            if presented.wrapping_sub(self.presenting[index].0) < u32::MAX / 2 {
                let (_, key, buffer, shm) = self.presenting.swap_remove(index);
                self.make_current(key, buffer, shm);
                // The proxy is behind until direct mode ends.
                self.mark_stale(key);
                // The flip paces the app: its next frame starts now, not at
                // the next tick of the X path's grid (up to a frame later,
                // too late for the next vblank).
                if self.server.wants_frame(key) {
                    if let Some(timing) = self.timing.as_mut() {
                        timing.frame_done();
                    }
                    self.server.frame_done(key, (flip_ns / 1_000_000) as u32);
                    self.server.flush();
                }
            } else {
                index += 1;
            }
        }
        self.flip = None;
        let target = window
            .and_then(|window| self.windows.get(&window).copied())
            .filter(|key| self.can_go_direct(*key, gpu));
        if target != self.direct
            && let Some(old) = self.direct.take()
        {
            gpu.set_direct(Vec::new());
            self.shown.clear();
            if self.debug {
                eprintln!("wayland: direct off for surface {}", old.0);
            }
        }
        let commits = std::mem::take(&mut self.direct_commits);
        let mut changed = std::mem::take(&mut self.direct_dirty);
        if self.overlays != overlays {
            self.overlays = overlays.to_vec();
            changed = true;
        }
        let mut sent: Vec<(SurfaceKey, WlBuffer, Arc<ShmBuffer>)> = Vec::new();
        if let Some(root) = target
            && self.direct != Some(root)
        {
            self.direct = Some(root);
            if self.debug {
                eprintln!("wayland: direct on for surface {}", root.0);
            }
            // Everything of the app on screen now comes from its buffers.
            for key in self.tree(root) {
                if let Some((buffer, shm)) = self.proxies.get(&key).and_then(|p| p.current.clone())
                {
                    let all = [wayland::Rect::new(0, 0, shm.width, shm.height)];
                    if self.share(gpu, key, &buffer, &shm, &all) {
                        changed = true;
                    }
                }
            }
        }
        for (key, buffer, shm, rects) in commits {
            if self.direct.is_some() && self.direct == self.root_of(key) {
                if self.share(gpu, key, &buffer, &shm, &rects) {
                    sent.push((key, buffer, shm));
                    changed = true;
                } else {
                    self.make_current(key, buffer, shm);
                    self.mark_stale(key);
                }
            } else {
                // Direct mode ended before this commit was shown.
                self.make_current(key, buffer, shm);
                self.mark_stale(key);
            }
        }
        if changed && let Some(root) = self.direct {
            let nodes = self.direct_nodes(root);
            match gpu.set_direct(nodes) {
                Some(serial) => {
                    if let Some(timing) = self.timing.as_mut()
                        && let Some(rx) = timing.commit_rx.take()
                    {
                        timing
                            .sent
                            .push((serial, rx, now_ns(), gpu.upload_times().0));
                    }
                    for (key, buffer, shm) in sent {
                        self.presenting.push((serial, key, buffer, shm));
                    }
                }
                None => {
                    for (key, buffer, shm) in sent {
                        self.make_current(key, buffer, shm);
                    }
                }
            }
        }
        for key in std::mem::take(&mut self.stale) {
            if self.direct.is_some() && self.direct == self.root_of(key) {
                self.stale.push(key);
            } else if let Err(error) = self.sync_proxy(conn, key) {
                eprintln!("wayland: proxy refresh failed: {error}");
            }
        }
        // Compositor surfaces of buffers the client destroyed.
        let dead: Vec<ObjectId> = self
            .client_surfaces
            .iter()
            .filter(|(_, (buffer, _))| !buffer.is_alive())
            .map(|(id, _)| id.clone())
            .collect();
        for id in dead {
            if let Some((_, surface)) = self.client_surfaces.remove(&id) {
                gpu.free(surface);
            }
            self.behind.remove(&id);
        }
    }

    /// Direct display needs client surfaces (protocol v3) and pools the
    /// compositor may map safely (sealed against shrinking): every surface
    /// of the app that shows content must qualify.
    fn can_go_direct(&self, key: SurfaceKey, gpu: &GpuBackend) -> bool {
        let Some(proxy) = self.proxies.get(&key) else {
            return false;
        };
        gpu.supports_client_surfaces()
            && proxy.toplevel.is_some()
            && proxy.mapped
            && proxy.has_content
            && self.tree(key).iter().all(|member| {
                self.proxies.get(member).is_none_or(|p| {
                    // A first buffer still on its way has no `current` yet;
                    // `share` checks every buffer as it is shown.
                    !p.has_content
                        || p.current.as_ref().is_none_or(|(buffer, shm)| {
                            (shm.pool.sealed || gpu.guards_foreign_memory())
                                && !self.unshareable.contains(&buffer.id())
                        })
                })
            })
    }

    /// Hands a buffer's damage to the compositor (creating its surface the
    /// first time); the surface then shows that buffer. False if it cannot.
    fn share(
        &mut self,
        gpu: &mut GpuBackend,
        key: SurfaceKey,
        buffer: &WlBuffer,
        shm: &Arc<ShmBuffer>,
        rects: &[wayland::Rect],
    ) -> bool {
        // A pool that may shrink is mapped by the compositor only when it
        // guards such memory (v4); a buffer it gave up on stays on X.
        if (!shm.pool.sealed && !gpu.guards_foreign_memory())
            || self.unshareable.contains(&buffer.id())
        {
            return false;
        }
        // What the compositor's copy of this buffer lacks: everything
        // committed since it was last shown, this commit included.
        let behind = match self.behind.get_mut(&buffer.id()) {
            Some((_, behind)) => std::mem::take(behind),
            None => rects.to_vec(),
        };
        let rects = if rects
            .iter()
            .any(|rect| rect.width >= shm.width && rect.height >= shm.height)
        {
            rects.to_vec()
        } else {
            behind
        };
        let surface = match self.client_surfaces.get(&buffer.id()) {
            Some((_, surface)) => *surface,
            None => match gpu.create_client_surface(
                &shm.pool.fd,
                shm.offset as u32,
                shm.width as u32,
                shm.height as u32,
                shm.stride as u32,
                shm.argb,
                // Only whole dmabufs (offset 0): with planes on, the
                // compositor may scan a dmabuf surface out, and buffers at
                // an offset inside a pool are untested on the panel's DPU.
                // Others take the copy path (a dmabuf maps like a memfd).
                shm.pool.dmabuf && shm.offset == 0,
            ) {
                Ok(surface) => {
                    self.client_surfaces
                        .insert(buffer.id(), (buffer.clone(), surface));
                    self.behind.insert(buffer.id(), (key, Vec::new()));
                    surface
                }
                Err(error) => {
                    eprintln!("wayland: cannot share a client buffer: {error}");
                    return false;
                }
            },
        };
        let damage: Vec<Damage> = rects
            .iter()
            .map(|rect| Damage {
                x0: rect.x,
                y0: rect.y,
                x1: rect.x + rect.width,
                y1: rect.y + rect.height,
            })
            .collect();
        gpu.client_damage(surface, &damage);
        self.shown.insert(key, (surface, shm.width, shm.height));
        self.mark_stale(key);
        true
    }

    /// The app's visible surfaces as frame nodes, bottom to top.
    fn direct_nodes(&self, root: SurfaceKey) -> Vec<Direct> {
        let mut nodes = Vec::new();
        for key in self.tree(root) {
            let Some(proxy) = self.proxies.get(&key) else {
                continue;
            };
            let Some(&(surface, width, height)) = self.shown.get(&key) else {
                continue;
            };
            if !proxy.has_content || (key != root && !proxy.mapped) {
                continue;
            }
            let shown = proxy
                .crop
                .unwrap_or(wayland::Rect::new(0, 0, width, height));
            let (mut w, mut h) = (shown.width, shown.height);
            if key == root {
                // The toplevel is clipped to the area Moon gave the app.
                let (ww, wh) = self.last_size(proxy.window);
                w = w.min(i32::from(ww));
                h = h.min(i32::from(wh));
            }
            let (x, y) = self.screen_origin(key);
            nodes.push(Direct {
                surface: Some(surface),
                src: RectF::new(shown.x as f32, shown.y as f32, w as f32, h as f32),
                dst: RectF::new(x as f32, y as f32, w as f32, h as f32),
                radius: 0.0,
            });
        }
        // Moon's overlay windows over the app (volume, banner) come from
        // the mirror, cut to their rounded shape: around the shape the
        // mirror still holds the app's proxy, which is stale here.
        for &(rect, radius) in &self.overlays {
            nodes.push(Direct {
                surface: None,
                src: rect,
                dst: rect,
                radius,
            });
        }
        nodes
    }

    fn mark_stale(&mut self, key: SurfaceKey) {
        if !self.stale.contains(&key) {
            self.stale.push(key);
        }
    }

    /// Copies the buffer on screen into the proxy (after direct mode).
    fn sync_proxy<C: Connection>(
        &mut self,
        conn: &C,
        key: SurfaceKey,
    ) -> Result<(), Box<dyn Error>> {
        let Some((window, shm, crop)) = self
            .proxies
            .get(&key)
            .and_then(|proxy| Some((proxy.window, proxy.current.as_ref()?.1.clone(), proxy.crop)))
        else {
            return Ok(());
        };
        let shown = crop.unwrap_or(wayland::Rect::new(0, 0, shm.width, shm.height));
        let seg = self.segment(conn, &shm.pool)?;
        conn.shm_put_image(
            window,
            self.gc,
            (shm.stride / 4).clamp(1, 65535) as u16,
            shm.height.clamp(1, 65535) as u16,
            shown.x as u16,
            shown.y as u16,
            shown.width as u16,
            shown.height as u16,
            0,
            0,
            24,
            ImageFormat::Z_PIXMAP.into(),
            false,
            seg,
            shm.offset as u32,
        )?;
        Ok(())
    }

    /// A copied buffer is the one on screen; the one it replaces goes back.
    fn make_current(&mut self, surface: SurfaceKey, buffer: WlBuffer, shm: Arc<ShmBuffer>) {
        // Presentation feedback: a flip's time in direct mode, otherwise the
        // time the copy into the X proxy finished.
        match self.flip {
            Some((time_ns, sequence)) => {
                self.server
                    .buffer_presented(&buffer, time_ns, sequence, true);
            }
            None => self.server.buffer_presented(&buffer, now_ns(), 0, false),
        }
        let Some(proxy) = self.proxies.get_mut(&surface) else {
            self.server.release(&buffer);
            return;
        };
        if let Some((old, _)) = proxy.current.replace((buffer.clone(), shm))
            && old != buffer
        {
            self.server.release(&old);
        }
    }

    /// Repaints exposed areas of proxies from their buffers on screen.
    fn repaint_exposed<C: Connection>(&mut self, conn: &C) -> Result<(), Box<dyn Error>> {
        for (window, rect) in std::mem::take(&mut self.exposed) {
            let Some((shm, crop)) = self
                .windows
                .get(&window)
                .and_then(|key| self.proxies.get(key))
                .and_then(|proxy| Some((proxy.current.as_ref()?.1.clone(), proxy.crop)))
            else {
                continue;
            };
            let origin = crop.map(|crop| (crop.x, crop.y)).unwrap_or((0, 0));
            let source = wayland::Rect::new(
                rect.x + origin.0,
                rect.y + origin.1,
                rect.width,
                rect.height,
            );
            let Some(source) = source.clip(shm.width, shm.height) else {
                continue;
            };
            let seg = self.segment(conn, &shm.pool)?;
            conn.shm_put_image(
                window,
                self.gc,
                (shm.stride / 4).clamp(1, 65535) as u16,
                shm.height.clamp(1, 65535) as u16,
                source.x as u16,
                source.y as u16,
                source.width as u16,
                source.height as u16,
                clamp16(source.x - origin.0),
                clamp16(source.y - origin.1),
                24,
                ImageFormat::Z_PIXMAP.into(),
                false,
                seg,
                shm.offset as u32,
            )?;
        }
        Ok(())
    }

    /// The pool's MIT-SHM segment, attached (again after a resize) on use.
    fn segment<C: Connection>(
        &mut self,
        conn: &C,
        pool: &Arc<ShmPool>,
    ) -> Result<u32, Box<dyn Error>> {
        let (_, generation) = pool.size();
        if let Some(segment) = self.segments.get(&pool.id)
            && segment.generation == generation
        {
            return Ok(segment.seg);
        }
        if let Some(old) = self.segments.remove(&pool.id) {
            // Requests run in order: copies already queued use the old
            // attachment before this detach.
            conn.shm_detach(old.seg)?;
        }
        let seg = conn.generate_id()?;
        let fd = pool.fd.as_fd().try_clone_to_owned()?;
        conn.shm_attach_fd(seg, fd, true)?;
        self.segments.insert(
            pool.id,
            Segment {
                seg,
                generation,
                pool: pool.clone(),
            },
        );
        Ok(seg)
    }

    /// X events about proxies. Returns true when the event was only ours
    /// (a copy completion); everything else also goes to Moon's handler.
    pub fn handle_x_event(&mut self, event: &Event) -> bool {
        match event {
            Event::ShmCompletion(event) => {
                let Some(queue) = self.in_flight.get_mut(&event.drawable) else {
                    return false;
                };
                let copied = queue.pop_front();
                if let Some(key) = self.windows.get(&event.drawable).copied() {
                    if let Some((buffer, shm)) = copied {
                        self.make_current(key, buffer, shm);
                    }
                    if let Some(proxy) = self.proxies.get_mut(&key) {
                        proxy.frame_pending = true;
                    }
                }
                true
            }
            Event::ConfigureNotify(event) => {
                self.reconfigure(event.window, Some((event.width, event.height)));
                false
            }
            Event::Expose(event) if self.is_proxy(event.window) => {
                // A child window went away or moved: repaint from the buffer
                // on screen at the next dispatch.
                self.exposed.push((
                    event.window,
                    wayland::Rect::new(
                        i32::from(event.x),
                        i32::from(event.y),
                        i32::from(event.width),
                        i32::from(event.height),
                    ),
                ));
                false
            }
            Event::MapNotify(event) => {
                if let Some(key) = self.windows.get(&event.window).copied()
                    && let Some(proxy) = self.proxies.get_mut(&key)
                {
                    proxy.mapped = true;
                    self.direct_dirty = true;
                    self.reconfigure(event.window, None);
                }
                false
            }
            Event::UnmapNotify(event) => {
                if let Some(key) = self.windows.get(&event.window).copied()
                    && let Some(proxy) = self.proxies.get_mut(&key)
                {
                    proxy.mapped = false;
                    self.direct_dirty = true;
                    if self.touch_window == Some(event.window) {
                        self.touch_window = None;
                        self.server.touch_cancel();
                    }
                    self.reconfigure(event.window, None);
                }
                false
            }
            Event::FocusIn(event) => {
                if let Some(id) = self.toplevel_of_window(event.event) {
                    if let Some(proxy) = self.proxy_of_window_mut(event.event) {
                        proxy.focused = true;
                    }
                    self.server.set_keyboard_focus(Some(id));
                    self.reconfigure(event.event, None);
                }
                false
            }
            Event::FocusOut(event) if event.detail != NotifyDetail::INFERIOR => {
                if self.toplevel_of_window(event.event).is_some() {
                    if let Some(proxy) = self.proxy_of_window_mut(event.event) {
                        proxy.focused = false;
                    }
                    self.server.set_keyboard_focus(None);
                    self.reconfigure(event.event, None);
                }
                false
            }
            Event::ButtonPress(event) if self.is_proxy(event.event) => {
                let key = self.windows[&event.event];
                let (x, y) = self.surface_point(event.event, event.event_x, event.event_y);
                self.touch_window = Some(event.event);
                self.server.touch_down(key, 0, x, y, event.time);
                false
            }
            Event::MotionNotify(event) if self.touch_window == Some(event.event) => {
                let (x, y) = self.surface_point(event.event, event.event_x, event.event_y);
                self.server.touch_motion(0, x, y, event.time);
                false
            }
            Event::ButtonRelease(event) if self.touch_window == Some(event.event) => {
                self.touch_window = None;
                self.server.touch_up(0, event.time);
                false
            }
            _ => false,
        }
    }

    /// Window pixels to surface coordinates (the window shows the buffer
    /// from its crop origin).
    fn surface_point(&self, window: u32, x: i16, y: i16) -> (f64, f64) {
        let origin = self
            .windows
            .get(&window)
            .and_then(|key| self.proxies.get(key))
            .and_then(|proxy| proxy.crop)
            .map(|crop| (crop.x, crop.y))
            .unwrap_or((0, 0));
        (
            f64::from(i32::from(x) + origin.0),
            f64::from(i32::from(y) + origin.1),
        )
    }

    fn toplevel_of_window(&self, window: u32) -> Option<ToplevelId> {
        self.windows
            .get(&window)
            .and_then(|key| self.proxies.get(key))
            .and_then(|proxy| proxy.toplevel)
    }

    fn proxy_of_window_mut(&mut self, window: u32) -> Option<&mut Proxy> {
        let key = self.windows.get(&window)?;
        self.proxies.get_mut(key)
    }

    /// A toplevel's X state changed: tell the client its size, whether it
    /// has focus, and whether it is visible at all.
    fn reconfigure(&mut self, window: u32, size: Option<(u16, u16)>) {
        let Some(key) = self.windows.get(&window) else {
            return;
        };
        let Some(proxy) = self.proxies.get(key) else {
            return;
        };
        let Some(id) = proxy.toplevel else {
            return;
        };
        let (focused, suspended) = (proxy.focused, proxy.managed && !proxy.mapped);
        let (width, height) = size.unwrap_or_else(|| self.last_size(window));
        self.sizes.insert(window, (width, height));
        self.server
            .configure_toplevel(id, i32::from(width), i32::from(height), focused, suspended);
    }

    fn last_size(&self, window: u32) -> (u16, u16) {
        self.sizes.get(&window).copied().unwrap_or((
            self.width,
            self.height.saturating_sub(crate::status_bar::HEIGHT),
        ))
    }

    /// Frame callbacks: every visible surface whose last content has been
    /// copied hears `done` once per refresh, so animating clients draw at
    /// the panel's rate and stop drawing while hidden or the screen is off.
    pub fn tick(&mut self, now: Instant, screen_awake: bool) -> Option<Instant> {
        let xwayland = screen_awake && self.server.xwayland_wants_frame();
        let ready: Vec<SurfaceKey> = self
            .proxies
            .iter()
            .filter(|(key, proxy)| {
                screen_awake
                    && proxy.frame_pending
                    && self.visible(**key)
                    && self.server.wants_frame(**key)
            })
            .map(|(key, _)| *key)
            .collect();
        if ready.is_empty() && !xwayland {
            return None;
        }
        if now < self.next_frame {
            return Some(self.next_frame);
        }
        let time = wayland::now_ms();
        if xwayland {
            self.server.xwayland_frame_done(time);
        }
        if let Some(timing) = self.timing.as_mut() {
            timing.frame_done();
        }
        for key in ready {
            if let Some(proxy) = self.proxies.get_mut(&key) {
                proxy.frame_pending = false;
            }
            self.server.frame_done(key, time);
        }
        self.server.flush();
        // A fixed grid, so late wakeups do not stretch every interval.
        self.next_frame += FRAME_INTERVAL;
        if self.next_frame <= now {
            self.next_frame = now + FRAME_INTERVAL;
        }
        None
    }

    /// A touchscreen contact (milestone 4): to Xwayland, which hands it to
    /// the X window under it as Xorg did. Its ids stay apart from the ids
    /// the proxies give native apps.
    pub fn xwayland_touch(&mut self, contact: crate::touch::Contact) {
        const BASE: i32 = 1 << 20;
        let time = wayland::now_ms();
        match contact {
            crate::touch::Contact::Begin(id, x, y) => {
                self.server
                    .xwayland_touch_down(BASE + id as i32, f64::from(x), f64::from(y), time);
            }
            crate::touch::Contact::Update(id, x, y) => {
                self.server
                    .touch_motion(BASE + id as i32, f64::from(x), f64::from(y), time);
            }
            crate::touch::Contact::End(id) => self.server.touch_up(BASE + id as i32, time),
        }
    }

    fn visible(&self, mut key: SurfaceKey) -> bool {
        for _ in 0..16 {
            let Some(proxy) = self.proxies.get(&key) else {
                return false;
            };
            if proxy.toplevel.is_some() {
                return proxy.mapped;
            }
            if !proxy.mapped && !proxy.has_content {
                return false;
            }
            match self.parent_of(key) {
                Some(parent) => key = parent,
                None => return false,
            }
        }
        false
    }

    fn parent_of(&self, key: SurfaceKey) -> Option<SurfaceKey> {
        self.parents.get(&key).copied()
    }

    /// Text from Moon's keyboard for the focused Wayland app.
    pub fn key_input(&mut self, input: KeyboardInput) -> bool {
        self.server.key_input(input, wayland::now_ms())
    }

    /// A Moon gesture took over the touch: the client must forget it.
    pub fn cancel_touch(&mut self) {
        if self.touch_window.take().is_some() {
            self.server.touch_cancel();
        }
    }

    pub fn touch_active(&self) -> bool {
        self.touch_window.is_some()
    }
}

fn proxy_events() -> EventMask {
    EventMask::EXPOSURE
        | EventMask::STRUCTURE_NOTIFY
        | EventMask::FOCUS_CHANGE
        | EventMask::BUTTON_PRESS
        | EventMask::BUTTON_RELEASE
        | EventMask::BUTTON_MOTION
}

fn now_ns() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    now.tv_sec as u64 * 1_000_000_000 + now.tv_nsec as u64
}

fn intersect(a: wayland::Rect, b: wayland::Rect) -> Option<wayland::Rect> {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.width).min(b.x + b.width);
    let y1 = (a.y + a.height).min(b.y + b.height);
    (x1 > x0 && y1 > y0).then(|| wayland::Rect::new(x0, y0, x1 - x0, y1 - y0))
}

fn clamp16(value: i32) -> i16 {
    value.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// The app a process belongs to, from the freezer cgroup Moon put it in
/// before exec (children inherit it). None off-device or for strangers.
fn owner_from_pid(pid: u32) -> Option<AppId> {
    let cgroups = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    cgroups.lines().find_map(|line| {
        let mut fields = line.splitn(3, ':');
        let (_, controllers, path) = (fields.next()?, fields.next()?, fields.next()?);
        if !controllers.split(',').any(|name| name == "freezer") {
            return None;
        }
        let name = path.strip_prefix("/moon/")?.split('/').next()?;
        [AppId::System, AppId::Browser]
            .into_iter()
            .find(|app| app.cgroup_name() == name)
    })
}

/// Moon's Wayland server for a `width` x `height` panel, with the app area
/// below the status strip for new toplevels.
pub fn bind_server(
    runtime_dir: &Path,
    width: u16,
    height: u16,
) -> Result<WaylandServer, Box<dyn Error>> {
    let app_height = height.saturating_sub(crate::status_bar::HEIGHT).max(1);
    // Apps draw at the panel's density: 3 pixels per unit by default (the
    // A26 is 386 dpi; Moon's own UI uses 2.77 px per iPhone point).
    let scale = std::env::var("MOON_WAYLAND_SCALE")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|scale| (1..=4).contains(scale))
        .unwrap_or(3);
    Ok(WaylandServer::bind(
        runtime_dir,
        "wayland-0",
        (width, height),
        (width, app_height),
        120_000,
        scale,
    )?)
}

/// Adds `new` to a damage list; past a handful of rectangles it becomes
/// their bounding box (one upload instead of many small ones).
fn add_damage(rects: &mut Vec<wayland::Rect>, new: &[wayland::Rect]) {
    rects.extend_from_slice(new);
    if rects.len() > 8 {
        let x0 = rects.iter().map(|r| r.x).min().unwrap_or(0);
        let y0 = rects.iter().map(|r| r.y).min().unwrap_or(0);
        let x1 = rects.iter().map(|r| r.x + r.width).max().unwrap_or(0);
        let y1 = rects.iter().map(|r| r.y + r.height).max().unwrap_or(0);
        rects.clear();
        rects.push(wayland::Rect::new(x0, y0, x1 - x0, y1 - y0));
    }
}

/// MOON_PROFILE: one direct frame's path, from the frame callback that
/// asked for it to the flip that showed it, summarized every 2 s:
/// frame done -> commit (the client draws), commit -> frame sent (Moon),
/// damage -> UPLOADED (the compositor's texture upload), frame sent ->
/// flip (upload, render, the wait for vblank), and flip -> next frame
/// done (Moon's wakeup and frame callback pacing).
#[derive(Default)]
struct DirectTiming {
    /// The first direct commit since the last frame sent.
    commit_rx: Option<u64>,
    /// Frames sent: serial, commit received, frame sent, damage sent.
    sent: Vec<(u32, u64, u64, u64)>,
    /// The last frame callback and the last flip seen.
    done_ns: u64,
    flip: (u64, u32),
    /// Per presented frame: [client, moon, upload, scanout, render, pacing,
    /// vblanks].
    samples: Vec<[f64; 7]>,
    /// The pacing (flip -> frame done) of the frame now being drawn.
    pacing_ms: f64,
    report_at: Option<Instant>,
}

impl DirectTiming {
    fn frame_done(&mut self) {
        self.done_ns = now_ns();
        if self.flip.0 > 0 && self.done_ns > self.flip.0 {
            self.pacing_ms = (self.done_ns - self.flip.0) as f64 / 1e6;
        }
    }

    fn presented(&mut self, serial: u32, flip_ns: u64, sequence: u32, gpu: &GpuBackend) {
        let Some(index) = self
            .sent
            .iter()
            .rposition(|(sent, ..)| serial.wrapping_sub(*sent) < u32::MAX / 2)
        else {
            return;
        };
        // Older entries were superseded before they reached the panel.
        let (_, rx, sent_ns, damage_ns) = self.sent[index];
        self.sent.drain(..=index);
        let ms = |from: u64, to: u64| to.saturating_sub(from) as f64 / 1e6;
        let (_, uploaded_ns) = gpu.upload_times();
        let upload = if damage_ns > 0 && uploaded_ns > damage_ns {
            ms(damage_ns, uploaded_ns)
        } else {
            0.0
        };
        let vblanks = if self.flip.1 > 0 {
            sequence.wrapping_sub(self.flip.1) as f64
        } else {
            0.0
        };
        let client = if self.done_ns > 0 && rx > self.done_ns {
            ms(self.done_ns, rx)
        } else {
            0.0
        };
        self.samples.push([
            client,
            ms(rx, sent_ns),
            upload,
            ms(sent_ns, flip_ns),
            gpu.presented_render_ns() as f64 / 1e6,
            self.pacing_ms,
            vblanks,
        ]);
        self.flip = (flip_ns, sequence);
        let now = Instant::now();
        let report_at = *self.report_at.get_or_insert(now + Duration::from_secs(2));
        if now >= report_at && !self.samples.is_empty() {
            let n = self.samples.len() as f64;
            let stat = |i: usize| {
                let mean = self.samples.iter().map(|s| s[i]).sum::<f64>() / n;
                let max = self.samples.iter().map(|s| s[i]).fold(0.0, f64::max);
                format!("{mean:.2}/{max:.2}")
            };
            eprintln!(
                "wayland: direct timing {} frames, ms mean/max: frame-done->commit {} commit->sent {} \
                 damage->uploaded {} sent->flip {} (render {}) flip->frame-done {} vblanks/frame {}",
                self.samples.len(),
                stat(0),
                stat(1),
                stat(2),
                stat(3),
                stat(4),
                stat(5),
                stat(6),
            );
            self.samples.clear();
            self.report_at = Some(now + Duration::from_secs(2));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_collapses_to_its_bounds_past_a_handful() {
        let mut rects = Vec::new();
        add_damage(&mut rects, &[wayland::Rect::new(10, 10, 5, 5)]);
        assert_eq!(rects.len(), 1);
        let many: Vec<_> = (0..8)
            .map(|i| wayland::Rect::new(i * 20, 100, 4, 4))
            .collect();
        add_damage(&mut rects, &many);
        assert_eq!(rects, vec![wayland::Rect::new(0, 10, 144, 94)]);
    }
}
