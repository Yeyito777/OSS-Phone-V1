//! Moon's Wayland server.
//!
//! Native apps connect to a socket in Moon's runtime directory and get the
//! protocols a full-screen phone app needs: wl_compositor/wl_subcompositor,
//! wl_shm, xdg-shell, wl_seat (touch and keyboard), wl_output, a minimal
//! data device, and text-input-v3 so Moon's own keyboard types into them.
//!
//! This module only speaks the protocol. Everything that shows or moves a
//! surface goes out as an [`Event`] in an outbox that the main loop drains
//! after each dispatch (milestone 1: `xproxy.rs` turns them into X proxy
//! windows); everything Moon decides (geometry, focus, input, frame pacing,
//! buffer release) comes back through the methods on [`WaylandServer`].
//! Like the rest of Moon, nothing here ever blocks: the listening socket and
//! the clients are nonblocking and polled by the main loop.

mod clipboard;
mod presentation;
mod seat;
mod shell;
mod shm;
mod surface;
mod text_input;

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsFd, AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wayland_server::backend::{ClientData, ClientId, DisconnectReason, ObjectId};
use wayland_server::protocol::wl_buffer::WlBuffer;
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{Display, DisplayHandle, ListeningSocket, Resource};

use crate::keyboard::{KeyboardInput, KeyboardPurpose};

pub use shm::ShmBuffer;
pub use shm::ShmPool;

/// A toplevel as Moon knows it. Stable for the toplevel's life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToplevelId(pub u32);

/// Any surface that shows pixels: a toplevel, a popup or a subsurface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SurfaceKey(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Surface units to buffer pixels.
    pub fn scaled(self, scale: i32) -> Self {
        Self::new(
            self.x.saturating_mul(scale),
            self.y.saturating_mul(scale),
            self.width.saturating_mul(scale),
            self.height.saturating_mul(scale),
        )
    }

    pub fn clip(self, width: i32, height: i32) -> Option<Self> {
        let x0 = self.x.max(0);
        let y0 = self.y.max(0);
        let x1 = (self.x.saturating_add(self.width)).min(width);
        let y1 = (self.y.saturating_add(self.height)).min(height);
        (x1 > x0 && y1 > y0).then(|| Self::new(x0, y0, x1 - x0, y1 - y0))
    }
}

/// What a surface is for, as far as placement goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// A toplevel: Moon decides its size and position.
    Toplevel(ToplevelId),
    /// A popup, at this offset from its parent surface.
    Popup { parent: SurfaceKey, x: i32, y: i32 },
    /// A subsurface, at this offset from its parent surface.
    Subsurface { parent: SurfaceKey, x: i32, y: i32 },
}

/// The outbox: everything the protocol side asks Moon to do.
#[derive(Debug)]
pub enum Event {
    /// A client created a toplevel. `pid` names its process (it may be a
    /// child of the app Moon launched).
    ToplevelCreated {
        id: ToplevelId,
        surface: SurfaceKey,
        pid: Option<u32>,
    },
    ToplevelTitle {
        id: ToplevelId,
        title: String,
    },
    /// A popup or subsurface appeared (it shows once it has a buffer).
    SurfaceCreated {
        surface: SurfaceKey,
        placement: Placement,
    },
    /// A popup or subsurface moved relative to its parent.
    SurfaceMoved {
        surface: SurfaceKey,
        placement: Placement,
    },
    /// New content: show `damage` (buffer pixels) of `buffer`. The buffer
    /// stays the client's until [`WaylandServer::release`] is called.
    Commit {
        surface: SurfaceKey,
        buffer: Arc<ShmBuffer>,
        resource: WlBuffer,
        damage: Vec<Rect>,
        /// The xdg window geometry: the part of the buffer to show.
        geometry: Option<Rect>,
    },
    /// The surface has no content any more (a null buffer): hide it.
    Unmapped {
        surface: SurfaceKey,
    },
    /// The surface (and, for a toplevel, the toplevel) is gone.
    Destroyed {
        surface: SurfaceKey,
        toplevel: Option<ToplevelId>,
    },
    /// The focused client turned text input on (with what the field holds)
    /// or off. Moon shows or hides its keyboard.
    TextInput {
        toplevel: Option<ToplevelId>,
        enabled: bool,
        purpose: KeyboardPurpose,
    },
}

/// Per-client data: the process credentials, read once at connection.
#[derive(Debug, Default)]
pub struct ClientInfo {
    pub pid: Option<u32>,
    /// Moon's own Xwayland (milestone 4): the X server Moon's X side runs
    /// on, not an app. Its one toplevel is the X root, sized by Moon's
    /// -geometry, in pixels (scale 1) and never configured, so never mapped:
    /// Moon still mirrors the X root, and Xwayland makes no buffers for it.
    /// It gets touch, and a keyboard that only ever enters its root (keys
    /// reach X through XTEST, as with Xorg).
    pub xwayland: bool,
}

pub(crate) fn is_xwayland(client: &wayland_server::Client) -> bool {
    client
        .get_data::<ClientInfo>()
        .is_some_and(|info| info.xwayland)
}

impl ClientData for ClientInfo {
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

/// The protocol state that every request handler mutates.
pub struct WlState {
    pub(crate) next_key: u32,
    pub(crate) next_toplevel: u32,
    pub(crate) surfaces: HashMap<ObjectId, surface::SurfaceData>,
    pub(crate) keys: HashMap<SurfaceKey, ObjectId>,
    pub(crate) toplevels: HashMap<ToplevelId, shell::Toplevel>,
    pub(crate) xdg: HashMap<SurfaceKey, shell::XdgInfo>,
    pub(crate) seat: seat::Seat,
    pub(crate) text_input: text_input::TextInputs,
    pub(crate) outbox: Vec<Event>,
    pub(crate) output_size: (i32, i32),
    /// The size a toplevel is first configured at (Moon's app area).
    pub(crate) toplevel_size: (i32, i32),
    pub(crate) refresh_mhz: i32,
    /// The panel's integer scale: logical units are this many pixels.
    pub(crate) scale: i32,
    pub(crate) outputs: Vec<wayland_server::protocol::wl_output::WlOutput>,
    pub(crate) feedbacks: presentation::Feedbacks,
    pub(crate) clipboard: clipboard::Clipboard,
    pub(crate) serial: u32,
    /// Xwayland's root surface, once it has one.
    pub(crate) xwayland_root: Option<SurfaceKey>,
}

impl WlState {
    pub(crate) fn next_serial(&mut self) -> u32 {
        self.serial = self.serial.wrapping_add(1).max(1);
        self.serial
    }

    /// A popup or subsurface position, in pixels of its parent's buffer.
    pub(crate) fn placement_px(&self, placement: Placement) -> Placement {
        let parent_scale = |parent: SurfaceKey| {
            self.surface_by_key(parent)
                .map(|data| data.scale)
                .unwrap_or(self.scale)
        };
        match placement {
            Placement::Popup { parent, x, y } => {
                let scale = parent_scale(parent);
                Placement::Popup {
                    parent,
                    x: x * scale,
                    y: y * scale,
                }
            }
            Placement::Subsurface { parent, x, y } => {
                let scale = parent_scale(parent);
                Placement::Subsurface {
                    parent,
                    x: x * scale,
                    y: y * scale,
                }
            }
            toplevel => toplevel,
        }
    }

    pub(crate) fn surface_key(&self, surface: &WlSurface) -> Option<SurfaceKey> {
        self.surfaces.get(&surface.id()).map(|data| data.key)
    }

    pub(crate) fn surface_by_key(&self, key: SurfaceKey) -> Option<&surface::SurfaceData> {
        self.surfaces.get(self.keys.get(&key)?)
    }

    pub(crate) fn surface_by_key_mut(
        &mut self,
        key: SurfaceKey,
    ) -> Option<&mut surface::SurfaceData> {
        let id = self.keys.get(&key)?.clone();
        self.surfaces.get_mut(&id)
    }

    /// The toplevel a surface belongs to, following popup and subsurface
    /// parents.
    pub(crate) fn toplevel_of(&self, mut key: SurfaceKey) -> Option<ToplevelId> {
        for _ in 0..16 {
            match self.surface_by_key(key)?.placement? {
                Placement::Toplevel(id) => return Some(id),
                Placement::Popup { parent, .. } | Placement::Subsurface { parent, .. } => {
                    key = parent
                }
            }
        }
        None
    }
}

pub struct WaylandServer {
    display: Display<WlState>,
    listener: ListeningSocket,
    state: WlState,
    socket_path: PathBuf,
}

impl WaylandServer {
    /// Listens on `runtime_dir/name` (the directory must exist and be private
    /// to the session) with the panel's size and refresh, and the size of
    /// Moon's app area for new toplevels.
    pub fn bind(
        runtime_dir: &Path,
        name: &str,
        output_size: (u16, u16),
        toplevel_size: (u16, u16),
        refresh_mhz: i32,
        scale: i32,
    ) -> io::Result<Self> {
        let display: Display<WlState> =
            Display::new().map_err(|error| io::Error::other(format!("{error}")))?;
        let socket_path = runtime_dir.join(name);
        let listener = ListeningSocket::bind_absolute(socket_path.clone())
            .map_err(|error| io::Error::other(format!("{}: {error}", socket_path.display())))?;
        let handle = display.handle();
        surface::create_globals(&handle);
        shm::create_globals(&handle);
        shell::create_globals(&handle);
        seat::create_globals(&handle);
        text_input::create_globals(&handle);
        presentation::create_globals(&handle);
        clipboard::create_globals(&handle);
        Ok(Self {
            display,
            listener,
            state: WlState {
                next_key: 0,
                next_toplevel: 0,
                surfaces: HashMap::new(),
                keys: HashMap::new(),
                toplevels: HashMap::new(),
                xdg: HashMap::new(),
                seat: seat::Seat::new(),
                text_input: text_input::TextInputs::default(),
                outbox: Vec::new(),
                output_size: (i32::from(output_size.0), i32::from(output_size.1)),
                toplevel_size: (i32::from(toplevel_size.0), i32::from(toplevel_size.1)),
                refresh_mhz,
                scale: scale.max(1),
                outputs: Vec::new(),
                feedbacks: presentation::Feedbacks::default(),
                clipboard: clipboard::Clipboard::default(),
                serial: 0,
                xwayland_root: None,
            },
            socket_path,
        })
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The two descriptors the main loop polls for input.
    pub fn poll_fds(&mut self) -> [RawFd; 2] {
        [
            self.listener.as_raw_fd(),
            self.display.backend().poll_fd().as_raw_fd(),
        ]
    }

    /// Accepts new clients and runs every pending request. The resulting
    /// events are in [`Self::take_events`].
    pub fn dispatch(&mut self) {
        loop {
            match self.listener.accept() {
                Ok(Some(stream)) => self.insert_client(stream),
                Ok(None) => break,
                Err(error) => {
                    eprintln!("wayland: accept failed: {error}");
                    break;
                }
            }
        }
        if let Err(error) = self.display.dispatch_clients(&mut self.state) {
            eprintln!("wayland: dispatch failed: {error}");
        }
        self.flush();
    }

    fn insert_client(&mut self, stream: UnixStream) {
        if let Err(error) = self.insert(stream, false) {
            eprintln!("wayland: cannot insert client: {error}");
        }
    }

    /// Moon's own Xwayland, on its end of a socket pair (WAYLAND_SOCKET):
    /// see [`ClientInfo::xwayland`].
    pub fn insert_xwayland(&mut self, stream: UnixStream) -> io::Result<()> {
        self.insert(stream, true)
    }

    fn insert(&mut self, stream: UnixStream, xwayland: bool) -> io::Result<()> {
        stream.set_nonblocking(true)?;
        let pid = peer_pid(&stream);
        self.display
            .handle()
            .insert_client(stream, Arc::new(ClientInfo { pid, xwayland }))?;
        eprintln!(
            "wayland: {} connected pid={}",
            if xwayland { "Xwayland" } else { "client" },
            pid.unwrap_or(0)
        );
        Ok(())
    }

    /// Whether Xwayland has mapped its root (it is ready for input).
    pub fn xwayland_root(&self) -> Option<SurfaceKey> {
        self.state
            .xwayland_root
            .filter(|key| self.state.surface_by_key(*key).is_some())
    }

    pub fn flush(&mut self) {
        if let Err(error) = self.display.flush_clients() {
            // A client that stopped reading is disconnected by the backend.
            if error.kind() != io::ErrorKind::BrokenPipe {
                eprintln!("wayland: flush failed: {error}");
            }
        }
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.state.outbox)
    }

    fn handle(&self) -> DisplayHandle {
        self.display.handle()
    }

    /// Moon placed a toplevel: tell the client its size and state.
    pub fn configure_toplevel(
        &mut self,
        id: ToplevelId,
        width: i32,
        height: i32,
        activated: bool,
        suspended: bool,
    ) {
        shell::configure(&mut self.state, id, width, height, activated, suspended);
        self.flush();
    }

    /// The surface's last content is on the panel: its frame callbacks
    /// fire, and the client may draw the next frame.
    pub fn frame_done(&mut self, surface: SurfaceKey, time_ms: u32) {
        surface::frame_done(&mut self.state, surface, time_ms);
    }

    /// Whether a surface waits for a frame callback (it is animating).
    pub fn wants_frame(&self, surface: SurfaceKey) -> bool {
        self.state
            .surface_by_key(surface)
            .is_some_and(|data| !data.frame_callbacks.is_empty())
    }

    /// The buffer's content is on the panel since `time_ns` (CLOCK_MONOTONIC):
    /// presentation feedback for it is answered. `vsync`: the time is a
    /// page flip's (else the time the content was handed on).
    pub fn buffer_presented(
        &mut self,
        buffer: &WlBuffer,
        time_ns: u64,
        sequence: u64,
        vsync: bool,
    ) {
        presentation::presented(&mut self.state, buffer, time_ns, sequence, vsync);
    }

    /// Moon has copied the buffer: the client may reuse it.
    pub fn release(&mut self, buffer: &WlBuffer) {
        if buffer.is_alive() {
            buffer.release();
        }
    }

    pub fn set_keyboard_focus(&mut self, toplevel: Option<ToplevelId>) {
        let handle = self.handle();
        seat::set_keyboard_focus(&mut self.state, &handle, toplevel);
        self.flush();
    }

    pub fn touch_down(&mut self, surface: SurfaceKey, id: i32, x: f64, y: f64, time_ms: u32) {
        seat::touch_down(&mut self.state, surface, id, x, y, time_ms);
        self.flush();
    }

    /// A touch on the X root (screen pixels): Xwayland turns it into X
    /// touch events for the window under it.
    pub fn xwayland_touch_down(&mut self, id: i32, x: f64, y: f64, time_ms: u32) -> bool {
        let Some(root) = self.xwayland_root() else {
            return false;
        };
        self.touch_down(root, id, x, y, time_ms);
        true
    }

    pub fn touch_motion(&mut self, id: i32, x: f64, y: f64, time_ms: u32) {
        seat::touch_motion(&mut self.state, id, x, y, time_ms);
        self.flush();
    }

    pub fn touch_up(&mut self, id: i32, time_ms: u32) {
        seat::touch_up(&mut self.state, id, time_ms);
        self.flush();
    }

    pub fn touch_cancel(&mut self) {
        seat::touch_cancel(&mut self.state);
        self.flush();
    }

    /// Whether the focused client takes any text (text-input-v3), not only
    /// the keys of the US keymap.
    pub fn takes_text(&self) -> bool {
        text_input::enabled(&self.state)
    }

    /// One key from Moon's on-screen keyboard, for the focused toplevel:
    /// text through text-input-v3 when the client uses it, otherwise key
    /// events from the built-in US keymap.
    pub fn key_input(&mut self, input: KeyboardInput, time_ms: u32) -> bool {
        let delivered = if text_input::deliver(&mut self.state, input) {
            true
        } else {
            seat::deliver_key(&mut self.state, input, time_ms)
        };
        self.flush();
        delivered
    }
}

impl Drop for WaylandServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

fn peer_pid(stream: &UnixStream) -> Option<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: SO_PEERCRED fills exactly one ucred of the given length.
    let result = unsafe {
        libc::getsockopt(
            stream.as_fd().as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut credentials as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    (result == 0 && credentials.pid > 0).then_some(credentials.pid as u32)
}

/// CLOCK_MONOTONIC in milliseconds, the clock of every input timestamp.
pub fn now_ms() -> u32 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) };
    (now.tv_sec as u64 * 1000 + now.tv_nsec as u64 / 1_000_000) as u32
}

#[cfg(test)]
mod tests;
