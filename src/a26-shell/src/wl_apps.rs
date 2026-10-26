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

use crate::keyboard::{KeyboardInput, KeyboardPurpose};
use crate::model::AppId;
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
    in_flight: HashMap<u32, VecDeque<WlBuffer>>,
    next_frame: Instant,
    /// The window a touch went down on (X keeps delivering to it).
    touch_window: Option<u32>,
    /// Last size of each toplevel proxy, for configures without a resize.
    sizes: HashMap<u32, (u16, u16)>,
    /// Parent of each popup or subsurface.
    parents: HashMap<SurfaceKey, SurfaceKey>,
    actions: Vec<Action>,
}

impl WaylandApps {
    pub fn new<C: Connection>(
        conn: &C,
        root: u32,
        width: u16,
        height: u16,
        runtime_dir: &Path,
    ) -> Result<Self, Box<dyn Error>> {
        let app_height = height.saturating_sub(crate::status_bar::HEIGHT).max(1);
        let server = WaylandServer::bind(
            runtime_dir,
            "wayland-0",
            (width, height),
            (width, app_height),
            120_000,
        )?;
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
            next_frame: Instant::now(),
            touch_window: None,
            sizes: HashMap::new(),
            parents: HashMap::new(),
            actions: Vec::new(),
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

    /// Runs the clients' requests and turns them into X requests and
    /// actions for the main loop.
    pub fn dispatch<C: Connection>(&mut self, conn: &C) -> Vec<Action> {
        self.server.dispatch();
        for event in self.server.take_events() {
            if let Err(error) = self.apply(conn, event) {
                eprintln!("wayland: proxy request failed: {error}");
            }
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
                self.place_child(conn, surface, placement)?;
            }
            wayland::Event::Commit {
                surface,
                buffer,
                resource,
                damage,
            } => self.commit(conn, surface, &buffer, resource, &damage)?,
            wayland::Event::Unmapped { surface } => {
                if let Some(proxy) = self.proxies.get_mut(&surface) {
                    proxy.has_content = false;
                    if proxy.toplevel.is_none() {
                        conn.unmap_window(proxy.window)?;
                    }
                }
            }
            wayland::Event::Destroyed { surface, toplevel } => {
                if let Some(id) = toplevel {
                    eprintln!("wayland: toplevel {} closed", id.0);
                }
                self.parents.remove(&surface);
                if let Some(proxy) = self.proxies.remove(&surface) {
                    self.windows.remove(&proxy.window);
                    self.sizes.remove(&proxy.window);
                    // Buffers still in flight are the client's to free.
                    self.in_flight.remove(&proxy.window);
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
        let (parent, x, y) = match placement {
            Placement::Popup { parent, x, y } | Placement::Subsurface { parent, x, y } => {
                (parent, x, y)
            }
            Placement::Toplevel(_) => return Ok(()),
        };
        let Some(parent_window) = self.proxies.get(&parent).map(|proxy| proxy.window) else {
            return Ok(());
        };
        if let Some(proxy) = self.proxies.get(&surface) {
            conn.configure_window(
                proxy.window,
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
            clamp16(x),
            clamp16(y),
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
        Ok(())
    }

    fn commit<C: Connection>(
        &mut self,
        conn: &C,
        surface: SurfaceKey,
        buffer: &ShmBuffer,
        resource: WlBuffer,
        damage: &[wayland::Rect],
    ) -> Result<(), Box<dyn Error>> {
        let Some((window, is_child, has_content)) = self
            .proxies
            .get(&surface)
            .map(|proxy| (proxy.window, proxy.toplevel.is_none(), proxy.has_content))
        else {
            self.server.release(&resource);
            return Ok(());
        };
        let seg = self.segment(conn, &buffer.pool)?;
        if is_child && !has_content {
            // A child shows at its buffer's size.
            conn.configure_window(
                window,
                &ConfigureWindowAux::new()
                    .width(buffer.width.clamp(1, 8192) as u32)
                    .height(buffer.height.clamp(1, 8192) as u32),
            )?;
            conn.map_window(window)?;
        }
        let rects = damage;
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
                clamp16(rect.x),
                clamp16(rect.y),
                24,
                ImageFormat::Z_PIXMAP.into(),
                index == last,
                seg,
                buffer.offset as u32,
            )?;
        }
        if rects.is_empty() {
            // Nothing to copy: the buffer is free again at once.
            self.server.release(&resource);
        } else {
            self.in_flight
                .entry(window)
                .or_default()
                .push_back(resource);
        }
        let proxy = self.proxies.get_mut(&surface).expect("looked up above");
        let first = !proxy.has_content;
        proxy.has_content = true;
        if first && proxy.toplevel.is_some() && !proxy.managed {
            proxy.managed = true;
            self.actions.push(Action::Manage {
                window,
                owner: proxy.owner,
            });
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
                if let Some(buffer) = queue.pop_front() {
                    self.server.release(&buffer);
                }
                if let Some(key) = self.windows.get(&event.drawable)
                    && let Some(proxy) = self.proxies.get_mut(key)
                {
                    proxy.frame_pending = true;
                }
                true
            }
            Event::ConfigureNotify(event) => {
                self.reconfigure(event.window, Some((event.width, event.height)));
                false
            }
            Event::MapNotify(event) => {
                if let Some(key) = self.windows.get(&event.window).copied()
                    && let Some(proxy) = self.proxies.get_mut(&key)
                {
                    proxy.mapped = true;
                    self.reconfigure(event.window, None);
                }
                false
            }
            Event::UnmapNotify(event) => {
                if let Some(key) = self.windows.get(&event.window).copied()
                    && let Some(proxy) = self.proxies.get_mut(&key)
                {
                    proxy.mapped = false;
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
                self.touch_window = Some(event.event);
                self.server.touch_down(
                    key,
                    0,
                    f64::from(event.event_x),
                    f64::from(event.event_y),
                    event.time,
                );
                false
            }
            Event::MotionNotify(event) if self.touch_window == Some(event.event) => {
                self.server.touch_motion(
                    0,
                    f64::from(event.event_x),
                    f64::from(event.event_y),
                    event.time,
                );
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
        if ready.is_empty() {
            return None;
        }
        if now < self.next_frame {
            return Some(self.next_frame);
        }
        let time = wayland::now_ms();
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
    EventMask::STRUCTURE_NOTIFY
        | EventMask::FOCUS_CHANGE
        | EventMask::BUTTON_PRESS
        | EventMask::BUTTON_RELEASE
        | EventMask::BUTTON_MOTION
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
