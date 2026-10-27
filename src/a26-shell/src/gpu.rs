//! Moon's side of moon-compositor (`src/moon-gpu`), the GPU compositor that
//! owns the panel in GPU mode (`MOON_GPU_SOCKET`).
//!
//! In GPU mode Xorg runs on a headless driver and the compositor, a native
//! Android process, renders every frame with the Mali GPU and page-flips it
//! at vblank. Moon keeps describing frames exactly as for the XRender path
//! (`compositor::Frame`); this module ships them over a Unix seqpacket socket
//! (protocol: `src/moon-gpu/proto.h`) instead of executing them in Xorg:
//!
//! - **Layers** live in shared memory (a memfd per layer). An upload copies
//!   the changed 64-pixel chunks into it and names the changed rectangles.
//! - **The X mirror.** Apps (and the status, keyboard and volume windows over
//!   them) still draw into Xorg. The compositor hands Moon a dmabuf, which
//!   Moon attaches as a MIT-SHM pixmap; Moon copies the root's DAMAGE into it
//!   with `CopyArea` and follows each copy with a one-pixel `ShmPutImage`
//!   whose completion event says, in request order, that the copy is done.
//!   Only then is the compositor told, so it never samples a half copy.
//!   While an app is in front, the compositor shows the mirror by itself and
//!   redraws on each mirror update. App snapshots are GPU copies of it.
//! - **Dims are exact.** A darkened background is the source layer with a
//!   brightness factor; no darkened copies exist.
//! - **Pacing.** Each frame is answered with its flip's vblank time, which
//!   replaces Present's completion events.
//!
//! Like the rest of Moon's event loop, nothing here waits for an X reply; the
//! version queries happen in `connect`, before the loop starts. The socket
//! is nonblocking; if the compositor goes away Moon keeps running, retries
//! once a second and re-sends every layer when it is back.

use std::error::Error;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::damage::{self, ConnectionExt as _};
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    ConnectionExt as _, CreateGCAux, Gcontext, ImageFormat, Pixmap, SubwindowMode, Window,
};

use crate::compositor::{FrameStats, Node, SurfaceId};
use crate::gfx::{Canvas, RectF};
use crate::present::Damage;

/// Moon offers the newest version it speaks (v3: a byte offset for shm
/// surfaces, which Wayland client buffers need) and steps down, at most to
/// v2, only when the compositor closes the link after the hello: a
/// compositor older than v3 drops a client whose version differs from its
/// own. Timeouts and connect errors are plain failures, retried at the top.
const PROTO_VERSION: u32 = 4;
const PROTO_MIN_VERSION: u32 = 2;
/// The newest version Moon can speak; MOON_GPU_PROTO offers an older one
/// (to test the v2/v3 paths).
const PROTO_MAX_KNOWN: u32 = 4;

fn offered_version() -> u32 {
    std::env::var("MOON_GPU_PROTO")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|version| (PROTO_MIN_VERSION..=PROTO_MAX_KNOWN).contains(version))
        .unwrap_or(PROTO_VERSION)
}
/// Wayland client surfaces need offsets (v3 and later).
const CLIENT_SURFACE_VERSION: u32 = 3;
const MIRROR_ID: u32 = u32::MAX;

const MSG_HELLO: u16 = 1;
const MSG_SURFACE_SHM: u16 = 2;
const MSG_SURFACE_DAMAGE: u16 = 3;
const MSG_SURFACE_COPY: u16 = 4;
const MSG_SURFACE_FREE: u16 = 5;
const MSG_MIRROR_DAMAGE: u16 = 6;
const MSG_FRAME: u16 = 7;
const MSG_POWER: u16 = 8;
const MSG_WELCOME: u16 = 64;
const MSG_MIRROR: u16 = 65;
const MSG_UPLOADED: u16 = 66;
const MSG_PRESENTED: u16 = 67;
const MSG_POWERED: u16 = 68;

const FORMAT_XRGB: u32 = 1;
const FORMAT_ARGB: u32 = 2;
const NODE_IMAGE: u32 = 1;
const NODE_FILL: u32 = 2;
const NODE_SMOOTH: u32 = 1;
const FRAME_HAS_BASE: u32 = 1;
const SURFACE_DMABUF: u32 = 1;
/// v4: memory that is not Moon's own and may shrink (a client's pool).
const SURFACE_FOREIGN: u32 = 2;
/// Client memory that may shrink needs v4 (the compositor guards it).
const FOREIGN_VERSION: u32 = 4;
const MSG_SURFACE_IDLE: u16 = 69;
const MSG_SURFACE_FAILED: u16 = 70;
const CAP_GPU: u32 = 1;
const CAP_PLANES: u32 = 2;
/// Corner radius baked into switcher card copies (see `prescale`).
const CARD_RADIUS: f32 = crate::interaction::CARD_RADIUS;

/// `DMA_HEAP_IOCTL_ALLOC` and `DMA_BUF_IOCTL_SYNC` (linux/dma-heap.h,
/// linux/dma-buf.h).
const DMA_HEAP_IOCTL_ALLOC: libc::c_ulong = 0xc018_4800;
const DMA_BUF_IOCTL_SYNC: libc::c_ulong = 0x4008_6200;
const DMA_BUF_SYNC_WRITE: u64 = 2;
const DMA_BUF_SYNC_END: u64 = 4;

#[repr(C)]
struct DmaHeapAllocation {
    len: u64,
    fd: u32,
    fd_flags: u32,
    heap_flags: u64,
}

const MAX_PACKET: usize = 64 * 1024;
/// Rectangles per damage message; more become their bounding box.
const MAX_RECTS: usize = 2000;
const BAND_ROWS: usize = 16;
const CHUNK: usize = 64;
/// A frame not presented within this long is presumed lost.
const PRESENT_WATCHDOG: Duration = Duration::from_millis(250);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

/// A little-endian message under construction.
struct Msg(Vec<u8>);

impl Msg {
    fn new(kind: u16) -> Self {
        let mut bytes = Vec::with_capacity(64);
        bytes.extend_from_slice(&kind.to_le_bytes());
        bytes.extend_from_slice(&0_u16.to_le_bytes());
        bytes.extend_from_slice(&0_u32.to_le_bytes());
        Self(bytes)
    }
    fn u32(&mut self, value: u32) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
    fn i32(&mut self, value: i32) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
    fn f32(&mut self, value: f32) -> &mut Self {
        self.0.extend_from_slice(&value.to_le_bytes());
        self
    }
    fn finish(&mut self) -> &[u8] {
        let length = self.0.len() as u32;
        self.0[4..8].copy_from_slice(&length.to_le_bytes());
        &self.0
    }
}

fn le_u32(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .unwrap_or(0)
}

fn le_u64(bytes: &[u8], at: usize) -> u64 {
    u64::from(le_u32(bytes, at)) | (u64::from(le_u32(bytes, at + 4)) << 32)
}

/// The compositor socket, shared with `DisplayPower` for panel on/off.
pub struct GpuLink {
    path: PathBuf,
    fd: Option<OwnedFd>,
    /// The compositor closed the link (EOF, reset or broken pipe), as
    /// opposed to Moon giving up on it.
    peer_closed: bool,
}

/// Why a handshake failed: the compositor hung up after the hello (it does
/// not speak that version, or it is restarting), or anything else.
enum HandshakeError {
    ClosedByPeer,
    Failed(Box<dyn Error>),
}

/// The compositor socket's inode: a different one after a failed attempt
/// means a new compositor (the keeper restarted it), not a refusal.
fn socket_inode(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).ok().map(|meta| meta.ino())
}

pub type SharedLink = std::rc::Rc<std::cell::RefCell<GpuLink>>;

impl GpuLink {
    fn connect(&mut self) -> io::Result<()> {
        // SAFETY: plain socket(2); the result is checked.
        let fd = unsafe {
            libc::socket(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a fresh socket owned by nothing else.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        self.peer_closed = false;
        // SAFETY: sockaddr_un is plain old data; zeroed is a valid value.
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as libc::sa_family_t;
        let path = self.path.as_os_str().as_encoded_bytes();
        if path.len() >= address.sun_path.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "socket path too long",
            ));
        }
        for (dst, src) in address.sun_path.iter_mut().zip(path) {
            *dst = *src as libc::c_char;
        }
        // SAFETY: address is a valid sockaddr_un of the given size.
        let result = unsafe {
            libc::connect(
                fd.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        self.fd = Some(fd);
        Ok(())
    }

    pub fn connected(&self) -> bool {
        self.fd.is_some()
    }

    fn raw_fd(&self) -> Option<RawFd> {
        self.fd.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// Sends one packet (with an optional fd). A full socket drops the
    /// packet; any other failure closes the link.
    fn send(&mut self, packet: &[u8], fd: Option<RawFd>) -> bool {
        let Some(socket) = self.raw_fd() else {
            return false;
        };
        let mut iov = libc::iovec {
            iov_base: packet.as_ptr() as *mut libc::c_void,
            iov_len: packet.len(),
        };
        let mut control = [0_u64; 4];
        // SAFETY: msghdr is plain old data; zeroed is a valid value.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        if let Some(fd) = fd {
            // SAFETY: control is large and aligned enough for one SCM_RIGHTS
            // header carrying one int; the CMSG macros stay within it.
            unsafe {
                message.msg_control = control.as_mut_ptr().cast();
                message.msg_controllen = libc::CMSG_SPACE(std::mem::size_of::<RawFd>() as u32) as _;
                let header = libc::CMSG_FIRSTHDR(&message);
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len = libc::CMSG_LEN(std::mem::size_of::<RawFd>() as u32) as _;
                std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), fd);
            }
        }
        // SAFETY: message points at live buffers for the duration of the call.
        let sent = unsafe { libc::sendmsg(socket, &message, libc::MSG_NOSIGNAL) };
        if sent < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::WouldBlock {
                eprintln!("gpu: compositor socket full; dropped a message");
                return true;
            }
            eprintln!("gpu: compositor link lost: {error}");
            self.peer_closed = matches!(
                error.kind(),
                io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
            );
            self.fd = None;
            return false;
        }
        true
    }

    /// Receives one packet and any fds it carries; None when none is
    /// waiting (or the link closed, which `connected` then reports).
    fn recv(&mut self, buffer: &mut [u8]) -> Option<(usize, Vec<OwnedFd>)> {
        let socket = self.raw_fd()?;
        let mut iov = libc::iovec {
            iov_base: buffer.as_mut_ptr().cast(),
            iov_len: buffer.len(),
        };
        let mut control = [0_u64; 8];
        // SAFETY: msghdr is plain old data; zeroed is a valid value.
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = std::mem::size_of_val(&control) as _;
        // SAFETY: message points at live buffers for the duration of the call.
        let received = unsafe { libc::recvmsg(socket, &mut message, libc::MSG_CMSG_CLOEXEC) };
        if received < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::WouldBlock
                && error.kind() != io::ErrorKind::Interrupted
            {
                eprintln!("gpu: compositor link lost: {error}");
                self.peer_closed = matches!(
                    error.kind(),
                    io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                );
                self.fd = None;
            }
            return None;
        }
        if received == 0 {
            eprintln!("gpu: compositor closed the link");
            self.peer_closed = true;
            self.fd = None;
            return None;
        }
        let mut fds = Vec::new();
        // SAFETY: the kernel filled msg_control; CMSG_* walk only within it,
        // and each SCM_RIGHTS fd is new and owned by this process.
        unsafe {
            let mut header = libc::CMSG_FIRSTHDR(&message);
            while !header.is_null() {
                if (*header).cmsg_level == libc::SOL_SOCKET
                    && (*header).cmsg_type == libc::SCM_RIGHTS
                {
                    let count = ((*header).cmsg_len as usize - libc::CMSG_LEN(0) as usize)
                        / std::mem::size_of::<RawFd>();
                    let data = libc::CMSG_DATA(header).cast::<RawFd>();
                    for i in 0..count {
                        fds.push(OwnedFd::from_raw_fd(std::ptr::read_unaligned(data.add(i))));
                    }
                }
                header = libc::CMSG_NXTHDR(&message, header);
            }
        }
        Some((received as usize, fds))
    }

    /// Turns the panel and display pipeline off or on (the CRTC's ACTIVE).
    pub fn power(&mut self, on: bool) {
        let mut msg = Msg::new(MSG_POWER);
        msg.u32(u32::from(on));
        self.send(msg.finish(), None);
    }
}

/// A layer's pixels in memory the compositor shares: a cached dma-heap
/// buffer it can also scan out, or a memfd where dma-heap is unavailable
/// (off-device).
struct Shared {
    fd: OwnedFd,
    dmabuf: bool,
    pixels: *mut u32,
    /// Row pitch in pixels.
    stride: usize,
    len: usize,
}

impl Shared {
    fn new(width: usize, height: usize) -> io::Result<Self> {
        if let Some(shared) = Self::dmabuf(width, height) {
            return Ok(shared);
        }
        let len = width * height * 4;
        // SAFETY: memfd_create with a static name; the result is checked.
        let fd = unsafe { libc::memfd_create(c"moon-layer".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a fresh memfd owned by nothing else.
        let fd = unsafe { OwnedFd::from_raw_fd(fd) };
        // SAFETY: ftruncate on our own memfd.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), len as libc::off_t) } < 0 {
            return Err(io::Error::last_os_error());
        }
        Self::map(fd, false, width, len)
    }

    /// A cached `system` dma-heap buffer with 64-byte rows (scanout and
    /// GPU import both accept that pitch).
    fn dmabuf(width: usize, height: usize) -> Option<Self> {
        let stride = (width * 4).div_ceil(64) * 16;
        let len = stride * 4 * height;
        // SAFETY: open(2) of a device path; the result is checked.
        let heap = unsafe {
            libc::open(
                c"/dev/dma_heap/system".as_ptr(),
                libc::O_RDONLY | libc::O_CLOEXEC,
            )
        };
        if heap < 0 {
            return None;
        }
        // SAFETY: heap is a fresh fd owned here.
        let heap = unsafe { OwnedFd::from_raw_fd(heap) };
        let mut request = DmaHeapAllocation {
            len: len as u64,
            fd: 0,
            fd_flags: (libc::O_RDWR | libc::O_CLOEXEC) as u32,
            heap_flags: 0,
        };
        // SAFETY: the ioctl fills `request`, which outlives the call.
        if unsafe { libc::ioctl(heap.as_raw_fd(), DMA_HEAP_IOCTL_ALLOC as _, &mut request) } < 0 {
            return None;
        }
        // SAFETY: the kernel returned a new dmabuf fd owned by nothing else.
        let fd = unsafe { OwnedFd::from_raw_fd(request.fd as RawFd) };
        Self::map(fd, true, stride, len).ok()
    }

    fn map(fd: OwnedFd, dmabuf: bool, stride: usize, len: usize) -> io::Result<Self> {
        // SAFETY: maps len bytes of a buffer at least that large.
        let pixels = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd.as_raw_fd(),
                0,
            )
        };
        if pixels == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            fd,
            dmabuf,
            pixels: pixels.cast(),
            stride,
            len,
        })
    }

    fn slice(&mut self) -> &mut [u32] {
        // SAFETY: pixels maps len bytes for as long as self lives, and only
        // this process writes it.
        unsafe { std::slice::from_raw_parts_mut(self.pixels, self.len / 4) }
    }

    /// Brackets CPU writes to a cached dmabuf (the end cleans the caches
    /// for the GPU and the display).
    fn sync(&self, end: bool) {
        if !self.dmabuf {
            return;
        }
        let flags: u64 = DMA_BUF_SYNC_WRITE | if end { DMA_BUF_SYNC_END } else { 0 };
        // SAFETY: DMA_BUF_IOCTL_SYNC reads one u64 from `flags`.
        unsafe { libc::ioctl(self.fd.as_raw_fd(), DMA_BUF_IOCTL_SYNC as _, &flags) };
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        // SAFETY: unmaps exactly the mapping made in `new`.
        unsafe { libc::munmap(self.pixels.cast(), self.len) };
    }
}

enum Surface {
    Layer {
        shared: Shared,
        width: usize,
        height: usize,
        transparent: bool,
        /// The memory holds an upload (else the first one sends everything).
        valid: bool,
    },
    /// A GPU copy of the X mirror; `present` once the compositor made it.
    Snapshot { present: bool },
    /// Another surface drawn darker: no pixels of its own.
    Dimmed { source: SurfaceId, dim: f32 },
    /// A Wayland client's wl_shm buffer, shown in place of the X mirror's
    /// copy of the app (protocol v3: the buffer's offset in its pool).
    Client {
        fd: OwnedFd,
        offset: u32,
        width: u32,
        height: u32,
        stride: u32,
        argb: bool,
    },
    /// The live X mirror as a node's source (`MG_MIRROR_ID`): an app's
    /// window moving inside one of Moon's frames.
    Mirror,
}

/// One surface of a Wayland app shown directly over the X mirror while the
/// app is in front: its buffer's `src` rectangle drawn at `dst` on the
/// screen (the toplevel, then its subsurfaces and popups in stacking order).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Direct {
    /// A client surface, or None for a patch of the X mirror (one of Moon's
    /// overlay windows over the app, cut to its shape by `radius`).
    pub surface: Option<SurfaceId>,
    pub src: RectF,
    pub dst: RectF,
    pub radius: f32,
}

fn wire(id: SurfaceId) -> u32 {
    id.0 as u32 + 1
}

/// The dmabuf Xorg copies the screen into, as a MIT-SHM pixmap.
struct Mirror {
    seg: shm::Seg,
    pixmap: Pixmap,
    width: u16,
    height: u16,
}

struct InFlight {
    serial: u32,
    sent: Instant,
    back_to_back: bool,
}

pub struct GpuBackend {
    link: SharedLink,
    root: Window,
    width: u16,
    height: u16,
    period_us: u64,
    surfaces: Vec<Option<Surface>>,
    mirror: Option<Mirror>,
    capture_gc: Gcontext,
    marker: Pixmap,
    marker_gc: Gcontext,
    damage: damage::Damage,
    /// Root damage not yet copied into the mirror.
    mirror_damage: Vec<Damage>,
    /// Rectangles of the copy whose completion marker is outstanding.
    flushing: Option<Vec<Damage>>,
    /// Snapshots to take once the outstanding copy completes.
    captures_after_flush: Vec<SurfaceId>,
    captures_waiting: Vec<SurfaceId>,
    /// Show the mirror alone (an app is in front).
    mirror_mode: bool,
    /// The last frame samples the live mirror (a page transition): Xorg's
    /// damage must keep reaching it.
    frame_uses_mirror: bool,
    /// The mirror frame waits for this copy (by number) to complete.
    mirror_frame_after: Option<u64>,
    flushes_issued: u64,
    /// A frame held until outstanding captures reach the compositor.
    held_frame: Option<Vec<u8>>,
    serial: u32,
    in_flight: Option<InFlight>,
    last_flip_us: u64,
    last_sequence: u32,
    last_presented_at: Option<Instant>,
    last_frame: Option<Vec<u8>>,
    reconnect_at: Option<Instant>,
    rows: Vec<SurfaceId>,
    /// Switcher cards: per snapshot, a copy at card size with rounded
    /// corners, which display planes can show as they are.
    cards: Vec<(SurfaceId, SurfaceId, u16, u16)>,
    profile: bool,
    buffer: Vec<u8>,
    /// The negotiated protocol version (3, or 2 for an older compositor).
    version: u32,
    direct: Vec<Direct>,
    /// The newest frame serial the compositor reported on screen.
    presented_serial: u32,
    /// Surfaces the compositor reported failed (v4), for the Wayland side.
    failed: Vec<SurfaceId>,
    /// The vblank time and counter of the newest flip reported.
    presented_at: (u64, u32),
    /// That frame's render time in the compositor (ns).
    presented_render_ns: u64,
    /// CLOCK_MONOTONIC (ns) of the last client damage sent and of the
    /// compositor's last UPLOADED reply: the upload's cost, roughly.
    damage_sent_ns: u64,
    uploaded_ns: u64,
}

impl GpuBackend {
    /// Connects to moon-compositor and sets up the X mirror. Called before
    /// the event loop, after `main.rs` checked MIT-SHM 1.2 and DAMAGE 1.1
    /// (version queries are reply waits); it waits for the compositor's
    /// welcome.
    pub fn connect<C: Connection>(
        conn: &C,
        root: Window,
        socket: PathBuf,
        width: u16,
        height: u16,
        profile: bool,
    ) -> Result<(Self, SharedLink), Box<dyn Error>> {
        let link = std::rc::Rc::new(std::cell::RefCell::new(GpuLink {
            path: socket,
            fd: None,
            peer_closed: false,
        }));
        let capture_gc = conn.generate_id()?;
        conn.create_gc(
            capture_gc,
            root,
            &CreateGCAux::new()
                .graphics_exposures(0)
                .subwindow_mode(SubwindowMode::INCLUDE_INFERIORS),
        )?;
        let marker = conn.generate_id()?;
        conn.create_pixmap(24, marker, root, 1, 1)?;
        let marker_gc = conn.generate_id()?;
        conn.create_gc(marker_gc, marker, &CreateGCAux::new().graphics_exposures(0))?;
        let damage = conn.generate_id()?;
        conn.damage_create(damage, root, damage::ReportLevel::RAW_RECTANGLES)?;
        let mut backend = Self {
            link: link.clone(),
            root,
            width,
            height,
            period_us: 8_333,
            surfaces: Vec::new(),
            mirror: None,
            capture_gc,
            marker,
            marker_gc,
            damage,
            mirror_damage: Vec::new(),
            flushing: None,
            captures_after_flush: Vec::new(),
            captures_waiting: Vec::new(),
            mirror_mode: false,
            frame_uses_mirror: false,
            mirror_frame_after: None,
            flushes_issued: 0,
            held_frame: None,
            serial: 0,
            in_flight: None,
            last_flip_us: 0,
            last_sequence: 0,
            last_presented_at: None,
            last_frame: None,
            reconnect_at: None,
            rows: Vec::new(),
            cards: Vec::new(),
            profile,
            buffer: vec![0; MAX_PACKET],
            version: offered_version(),
            direct: Vec::new(),
            presented_serial: 0,
            failed: Vec::new(),
            presented_at: (0, 0),
            presented_render_ns: 0,
            damage_sent_ns: 0,
            uploaded_ns: 0,
        };
        // The compositor starts before Moon; allow it a moment if not.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match backend.handshake(conn, Duration::from_secs(2)) {
                Ok(()) => break,
                Err(error) if Instant::now() < deadline => {
                    eprintln!("gpu: waiting for the compositor: {error}");
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(error) => return Err(error),
            }
        }
        Ok((backend, link))
    }

    /// Connects, says hello and attaches the mirror the compositor offers,
    /// in the newest version both speak. A version is given up only when
    /// the compositor closes the link after its hello, twice in a row on
    /// the same socket (once could be a compositor dying under a restart;
    /// a new socket inode means a new compositor, asked again).
    fn handshake<C: Connection>(&mut self, conn: &C, wait: Duration) -> Result<(), Box<dyn Error>> {
        let path = self.link.borrow().path.clone();
        let top = offered_version();
        let mut version = top;
        let mut refusals = 0;
        // A restarting compositor is asked again, but not forever.
        let mut new_compositors = 0;
        loop {
            let inode = socket_inode(&path);
            match self.handshake_version(conn, wait, version) {
                Ok(()) => {
                    if version != self.version || version < top {
                        eprintln!("gpu: protocol v{version}");
                    }
                    if version < CLIENT_SURFACE_VERSION {
                        eprintln!(
                            "gpu: compositor speaks v{version}: Wayland apps stay on the X path"
                        );
                    }
                    self.version = version;
                    return Ok(());
                }
                Err(HandshakeError::ClosedByPeer) => {
                    if socket_inode(&path) != inode && new_compositors < 3 {
                        // A new compositor came up meanwhile: ask it anew.
                        new_compositors += 1;
                        refusals = 0;
                        continue;
                    }
                    refusals += 1;
                    if refusals < 2 {
                        continue;
                    }
                    if version <= PROTO_MIN_VERSION {
                        return Err("the compositor closes every hello".into());
                    }
                    eprintln!("gpu: protocol v{version} refused; trying v{}", version - 1);
                    version -= 1;
                    refusals = 0;
                }
                Err(HandshakeError::Failed(error)) => return Err(error),
            }
        }
    }

    fn handshake_version<C: Connection>(
        &mut self,
        conn: &C,
        wait: Duration,
        version: u32,
    ) -> Result<(), HandshakeError> {
        let closed = |link: &SharedLink| {
            if link.borrow().peer_closed {
                HandshakeError::ClosedByPeer
            } else {
                HandshakeError::Failed("compositor link lost".into())
            }
        };
        self.link
            .borrow_mut()
            .connect()
            .map_err(|error| HandshakeError::Failed(error.into()))?;
        let mut hello = Msg::new(MSG_HELLO);
        hello.u32(version);
        if !self.link.borrow_mut().send(hello.finish(), None) {
            return Err(closed(&self.link));
        }
        let deadline = Instant::now() + wait;
        let mut welcomed = false;
        while Instant::now() < deadline {
            let Some(socket) = self.link.borrow().raw_fd() else {
                return Err(closed(&self.link));
            };
            let mut pfd = libc::pollfd {
                fd: socket,
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: one valid pollfd.
            unsafe { libc::poll(&mut pfd, 1, 100) };
            let mut buffer = std::mem::take(&mut self.buffer);
            let received = self.link.borrow_mut().recv(&mut buffer);
            let result = match received {
                Some((len, fds)) => {
                    let kind = u16::from_le_bytes([buffer[0], buffer[1]]);
                    match kind {
                        MSG_WELCOME => {
                            let refresh_mhz = le_u32(&buffer, 20);
                            if refresh_mhz > 0 {
                                self.period_us = 1_000_000_000 / u64::from(refresh_mhz);
                            }
                            eprintln!(
                                "gpu: compositor {}x{} at {}.{:03} Hz",
                                le_u32(&buffer, 12),
                                le_u32(&buffer, 16),
                                refresh_mhz / 1000,
                                refresh_mhz % 1000
                            );
                            let caps = le_u32(&buffer, 32);
                            eprintln!(
                                "gpu: frames by {}{}",
                                if caps & CAP_PLANES != 0 {
                                    "display planes, then "
                                } else {
                                    ""
                                },
                                if caps & CAP_GPU != 0 {
                                    "the GPU"
                                } else {
                                    "the CPU (no GPU)"
                                }
                            );
                            welcomed = true;
                            Ok(false)
                        }
                        MSG_MIRROR => self.attach_mirror(conn, &buffer[..len], fds).map(|()| true),
                        _ => Ok(false),
                    }
                }
                None => Ok(false),
            };
            self.buffer = buffer;
            if result.map_err(HandshakeError::Failed)? && welcomed {
                return Ok(());
            }
        }
        self.link.borrow_mut().fd = None;
        Err(HandshakeError::Failed(
            "no welcome from the compositor".into(),
        ))
    }

    fn attach_mirror<C: Connection>(
        &mut self,
        conn: &C,
        msg: &[u8],
        mut fds: Vec<OwnedFd>,
    ) -> Result<(), Box<dyn Error>> {
        let (width, height, stride) = (le_u32(msg, 12), le_u32(msg, 16), le_u32(msg, 20));
        let Some(fd) = fds.pop() else {
            return Err("mirror without a buffer".into());
        };
        if let Some(old) = self.mirror.take() {
            conn.free_pixmap(old.pixmap)?;
            conn.shm_detach(old.seg)?;
        }
        let seg = conn.generate_id()?;
        conn.shm_attach_fd(seg, fd, false)?;
        // The pixmap's row pitch must be the buffer's: X pads 32-bit rows
        // to whole pixels, so a stride of N bytes is N/4 pixels wide.
        let pixmap = conn.generate_id()?;
        conn.shm_create_pixmap(
            pixmap,
            self.root,
            (stride / 4) as u16,
            height as u16,
            24,
            seg,
            0,
        )?;
        self.mirror = Some(Mirror {
            seg,
            pixmap,
            width: width.min(u32::from(self.width)) as u16,
            height: height.min(u32::from(self.height)) as u16,
        });
        conn.flush()?;
        eprintln!("gpu: X mirror attached ({width}x{height}, stride {stride})");
        Ok(())
    }

    pub fn set_profile(&mut self, enabled: bool) {
        self.profile = enabled;
    }

    pub fn raw_fd(&self) -> Option<RawFd> {
        self.link.borrow().raw_fd()
    }

    fn insert(&mut self, surface: Surface) -> SurfaceId {
        if let Some(index) = self.surfaces.iter().position(Option::is_none) {
            self.surfaces[index] = Some(surface);
            SurfaceId(index)
        } else {
            self.surfaces.push(Some(surface));
            SurfaceId(self.surfaces.len() - 1)
        }
    }

    fn send(&mut self, packet: &[u8], fd: Option<RawFd>) {
        if !self.link.borrow_mut().send(packet, fd) {
            self.lost();
        }
    }

    fn lost(&mut self) {
        if self.reconnect_at.is_none() {
            self.reconnect_at = Some(Instant::now() + RECONNECT_INTERVAL);
        }
        self.in_flight = None;
        self.held_frame = None;
        self.flushing = None;
    }

    fn announce_layer(&mut self, id: SurfaceId) {
        let Some(Some(Surface::Layer {
            shared,
            width,
            height,
            transparent,
            ..
        })) = self.surfaces.get(id.0)
        else {
            return;
        };
        let mut msg = Msg::new(MSG_SURFACE_SHM);
        msg.u32(wire(id))
            .u32(*width as u32)
            .u32(*height as u32)
            .u32(shared.stride as u32 * 4)
            .u32(if *transparent {
                FORMAT_ARGB
            } else {
                FORMAT_XRGB
            })
            .u32(if shared.dmabuf { SURFACE_DMABUF } else { 0 });
        let fd = shared.fd.as_raw_fd();
        let packet = msg.finish().to_vec();
        self.send(&packet, Some(fd));
    }

    pub fn create_layer(
        &mut self,
        width: u16,
        height: u16,
        transparent: bool,
    ) -> Result<SurfaceId, Box<dyn Error>> {
        let (width, height) = (usize::from(width), usize::from(height));
        let shared = Shared::new(width, height)?;
        let id = self.insert(Surface::Layer {
            shared,
            width,
            height,
            transparent,
            valid: false,
        });
        self.announce_layer(id);
        Ok(id)
    }

    /// Copies what changed in `canvas` into the layer's shared memory and
    /// tells the compositor which rectangles to upload.
    pub fn upload(
        &mut self,
        id: SurfaceId,
        canvas: &Canvas,
    ) -> Result<Vec<Damage>, Box<dyn Error>> {
        let Some(Some(Surface::Layer {
            shared,
            width,
            height,
            valid,
            ..
        })) = self.surfaces.get_mut(id.0)
        else {
            return Err("unknown layer".into());
        };
        if canvas.width as usize != *width || canvas.height as usize != *height {
            return Err("canvas size does not match layer".into());
        }
        shared.sync(false);
        let stride = shared.stride;
        let damage = diff_into(
            &canvas.pixels,
            shared.slice(),
            *width,
            *height,
            stride,
            *valid,
        );
        shared.sync(true);
        *valid = true;
        if !damage.is_empty() {
            self.send_damage(MSG_SURFACE_DAMAGE, wire(id), &damage);
        }
        Ok(damage)
    }

    fn send_damage(&mut self, kind: u16, id: u32, damage: &[Damage]) {
        let bounds;
        let rects = if damage.len() > MAX_RECTS {
            bounds = [damage.iter().copied().reduce(Damage::union).unwrap()];
            &bounds[..]
        } else {
            damage
        };
        let mut msg = Msg::new(kind);
        msg.u32(id).u32(0).u32(rects.len() as u32).u32(0);
        for r in rects {
            msg.i32(r.x0).i32(r.y0).i32(r.x1).i32(r.y1);
        }
        let packet = msg.finish().to_vec();
        self.send(&packet, None);
    }

    pub fn create_snapshot(&mut self) -> SurfaceId {
        self.insert(Surface::Snapshot { present: false })
    }

    /// Snapshots the screen into `id` once everything drawn so far reached
    /// the mirror.
    pub fn capture_root<C: Connection>(
        &mut self,
        conn: &C,
        id: SurfaceId,
    ) -> Result<(), Box<dyn Error>> {
        if let Some(Some(Surface::Snapshot { present })) = self.surfaces.get_mut(id.0) {
            *present = false;
        }
        self.drop_card(id);
        self.mirror_damage.push(self.screen());
        self.captures_after_flush.push(id);
        self.flush_mirror(conn)
    }

    /// A copy of snapshot `id` at card size (`width` x `height`) with the
    /// switcher's rounded corners baked in, used where a frame shows the
    /// snapshot at exactly that size (the switcher at rest and scrolling).
    pub fn prescale(&mut self, id: SurfaceId, width: u16, height: u16) {
        if self
            .cards
            .iter()
            .any(|&(of, _, w, h)| of == id && (w, h) == (width, height))
            || self.pending_capture(id)
            || !matches!(
                self.surfaces.get(id.0),
                Some(Some(Surface::Snapshot { present: true }))
            )
        {
            return;
        }
        self.drop_card(id);
        let copy = self.insert(Surface::Snapshot { present: true });
        let mut msg = Msg::new(MSG_SURFACE_COPY);
        msg.u32(wire(copy))
            .u32(wire(id))
            .f32(0.0)
            .f32(0.0)
            .f32(f32::from(self.width))
            .f32(f32::from(self.height))
            .u32(u32::from(width))
            .u32(u32::from(height))
            .f32(CARD_RADIUS);
        let packet = msg.finish().to_vec();
        self.send(&packet, None);
        self.cards.push((id, copy, width, height));
    }

    fn drop_card(&mut self, of: SurfaceId) {
        while let Some(index) = self.cards.iter().position(|card| card.0 == of) {
            let (_, copy, _, _) = self.cards.swap_remove(index);
            self.free(copy);
        }
    }

    /// The card copy of `surface` to draw instead, if `dst` is its size.
    fn card_for(&self, surface: SurfaceId, src: RectF, dst: RectF) -> Option<(u32, RectF)> {
        let &(_, copy, w, h) = self.cards.iter().find(|card| card.0 == surface)?;
        if (dst.w - f32::from(w)).abs() >= 1.0 || (dst.h - f32::from(h)).abs() >= 1.0 {
            return None;
        }
        let (kx, ky) = (
            f32::from(w) / f32::from(self.width),
            f32::from(h) / f32::from(self.height),
        );
        Some((
            wire(copy),
            RectF::new(src.x * kx, src.y * ky, src.w * kx, src.h * ky),
        ))
    }

    /// A handle on the live X mirror for frame nodes.
    pub fn create_mirror_node(&mut self) -> SurfaceId {
        self.insert(Surface::Mirror)
    }

    /// A snapshot's copy has been sent: whatever Xorg draws from now on
    /// reaches the compositor after it.
    pub fn snapshot_taken(&self, id: SurfaceId) -> bool {
        matches!(
            self.surfaces.get(id.0),
            Some(Some(Surface::Snapshot { present: true }))
        ) && !self.pending_capture(id)
    }

    pub fn create_dimmed(&mut self, source: SurfaceId, dim: f32) -> SurfaceId {
        self.insert(Surface::Dimmed { source, dim })
    }

    pub fn free(&mut self, id: SurfaceId) {
        self.drop_card(id);
        if let Some(surface) = self.surfaces.get_mut(id.0).and_then(Option::take)
            && !matches!(surface, Surface::Dimmed { .. } | Surface::Mirror)
        {
            let mut msg = Msg::new(MSG_SURFACE_FREE);
            msg.u32(wire(id));
            let packet = msg.finish().to_vec();
            self.send(&packet, None);
        }
        self.captures_waiting.retain(|c| *c != id);
        self.captures_after_flush.retain(|c| *c != id);
    }

    /// Returns whether the dim changed.
    pub fn set_dim(&mut self, id: SurfaceId, value: f32) -> bool {
        match self.surfaces.get_mut(id.0) {
            Some(Some(Surface::Dimmed { dim, .. })) if *dim != value => {
                *dim = value;
                true
            }
            _ => false,
        }
    }

    pub fn set_rows(&mut self, rows: &[SurfaceId]) {
        self.rows.clear();
        self.rows.extend_from_slice(rows);
    }

    fn screen(&self) -> Damage {
        Damage {
            x0: 0,
            y0: 0,
            x1: i32::from(self.width),
            y1: i32::from(self.height),
        }
    }

    /// Copies pending root damage into the mirror, followed by the marker
    /// whose completion releases it to the compositor. One copy at a time.
    fn flush_mirror<C: Connection>(&mut self, conn: &C) -> Result<(), Box<dyn Error>> {
        if self.flushing.is_some()
            || self.mirror_damage.is_empty()
            || !self.link.borrow().connected()
        {
            return Ok(());
        }
        let Some(mirror) = self.mirror.as_ref() else {
            return Ok(());
        };
        let mut rects = std::mem::take(&mut self.mirror_damage);
        let screen = self.screen();
        if rects.len() > 64 {
            rects = vec![rects.iter().copied().reduce(Damage::union).unwrap()];
        }
        for r in &mut rects {
            r.x0 = r.x0.clamp(0, screen.x1);
            r.x1 = r.x1.clamp(0, screen.x1).min(i32::from(mirror.width));
            r.y0 = r.y0.clamp(0, screen.y1);
            r.y1 = r.y1.clamp(0, screen.y1).min(i32::from(mirror.height));
        }
        rects.retain(|r| r.x1 > r.x0 && r.y1 > r.y0);
        for r in &rects {
            conn.copy_area(
                self.root,
                mirror.pixmap,
                self.capture_gc,
                r.x0 as i16,
                r.y0 as i16,
                r.x0 as i16,
                r.y0 as i16,
                (r.x1 - r.x0) as u16,
                (r.y1 - r.y0) as u16,
            )?;
        }
        conn.shm_put_image(
            self.marker,
            self.marker_gc,
            1,
            1,
            0,
            0,
            1,
            1,
            0,
            0,
            24,
            ImageFormat::Z_PIXMAP.into(),
            true,
            mirror.seg,
            0,
        )?;
        conn.flush()?;
        self.captures_waiting
            .extend(std::mem::take(&mut self.captures_after_flush));
        self.flushes_issued += 1;
        self.flushing = Some(rects);
        Ok(())
    }

    /// Handles DAMAGE and MIT-SHM completion events; true if consumed.
    pub fn handle_event(&mut self, event: &Event) -> bool {
        match event {
            Event::DamageNotify(event) if event.damage == self.damage => {
                let a = event.area;
                self.mirror_damage.push(Damage {
                    x0: i32::from(a.x),
                    y0: i32::from(a.y),
                    x1: i32::from(a.x) + i32::from(a.width),
                    y1: i32::from(a.y) + i32::from(a.height),
                });
                if self.mirror_damage.len() > 256 {
                    let all = self.mirror_damage.iter().copied().reduce(Damage::union);
                    self.mirror_damage = all.into_iter().collect();
                }
                true
            }
            Event::ShmCompletion(event) if event.drawable == self.marker => {
                if let Some(rects) = self.flushing.take()
                    && !rects.is_empty()
                {
                    self.send_damage(MSG_MIRROR_DAMAGE, 0, &rects);
                }
                for id in std::mem::take(&mut self.captures_waiting) {
                    let mut msg = Msg::new(MSG_SURFACE_COPY);
                    msg.u32(wire(id))
                        .u32(MIRROR_ID)
                        .f32(0.0)
                        .f32(0.0)
                        .f32(f32::from(self.width))
                        .f32(f32::from(self.height))
                        .u32(u32::from(self.width))
                        .u32(u32::from(self.height))
                        .f32(0.0);
                    let packet = msg.finish().to_vec();
                    self.send(&packet, None);
                    if let Some(Some(Surface::Snapshot { present })) = self.surfaces.get_mut(id.0) {
                        *present = true;
                    }
                }
                if self.captures_after_flush.is_empty()
                    && let Some(frame) = self.held_frame.take()
                {
                    self.submit(frame, false);
                }
                if self
                    .mirror_frame_after
                    .is_some_and(|after| self.flushes_issued >= after)
                {
                    self.mirror_frame_after = None;
                    self.send_mirror_frame();
                }
                true
            }
            _ => false,
        }
    }

    /// Per loop iteration: reads the compositor's messages, reconnects,
    /// and keeps the mirror flowing while it is on screen.
    pub fn dispatch<C: Connection>(&mut self, conn: &C, stats: &mut FrameStats) {
        if !self.link.borrow().connected() {
            if self.reconnect_at.is_none() {
                self.lost();
            }
            if self.reconnect_at.is_some_and(|at| Instant::now() >= at) {
                self.reconnect(conn);
            }
            return;
        }
        let mut buffer = std::mem::take(&mut self.buffer);
        loop {
            let received = self.link.borrow_mut().recv(&mut buffer);
            let Some((len, fds)) = received else {
                break;
            };
            let msg = &buffer[..len];
            match u16::from_le_bytes([msg[0], msg[1]]) {
                MSG_PRESENTED => self.presented(msg, stats),
                MSG_MIRROR => {
                    if let Err(error) = self.attach_mirror(conn, msg, fds) {
                        eprintln!("gpu: mirror: {error}");
                    }
                }
                MSG_SURFACE_FAILED => {
                    // v4: the surface is gone (bad memory, a fault); frames
                    // skip it. Client surfaces return their app to X.
                    let wire_id = le_u32(msg, 8);
                    if wire_id > 0 {
                        let id = SurfaceId(wire_id as usize - 1);
                        eprintln!("gpu: surface {wire_id} failed (reason {})", le_u32(msg, 12));
                        self.failed.push(id);
                    }
                }
                MSG_UPLOADED => self.uploaded_ns = monotonic_ns(),
                MSG_POWERED | MSG_WELCOME | MSG_SURFACE_IDLE => {}
                other => eprintln!("gpu: unexpected message {other}"),
            }
        }
        self.buffer = buffer;
        if !self.link.borrow().connected() {
            self.lost();
            return;
        }
        // Copies are needed while the mirror is on screen or about to be,
        // and for snapshots.
        if (self.mirror_mode || self.frame_uses_mirror || !self.captures_after_flush.is_empty())
            && let Err(error) = self.flush_mirror(conn)
        {
            eprintln!("gpu: mirror copy failed: {error}");
        }
    }

    fn reconnect<C: Connection>(&mut self, conn: &C) {
        // A running compositor answers at once; never hold the loop long.
        match self.handshake(conn, Duration::from_millis(300)) {
            Ok(()) => {
                eprintln!("gpu: reconnected to the compositor; re-sending layers");
                self.reconnect_at = None;
                for index in 0..self.surfaces.len() {
                    match &mut self.surfaces[index] {
                        Some(Surface::Layer { .. }) => self.announce_layer(SurfaceId(index)),
                        // A v2 compositor cannot take client surfaces: the
                        // Wayland side sees that and falls back to X.
                        Some(Surface::Client { .. }) if self.version >= CLIENT_SURFACE_VERSION => {
                            self.announce_client(SurfaceId(index))
                        }
                        Some(Surface::Snapshot { present }) => *present = false,
                        _ => {}
                    }
                }
                if self.version < CLIENT_SURFACE_VERSION {
                    self.direct.clear();
                }
                self.mirror_damage.push(self.screen());
                if self.mirror_mode {
                    self.mirror_frame_after = Some(self.flushes_issued + 1);
                    let _ = self.flush_mirror(conn);
                } else if let Some(frame) = self.last_frame.clone() {
                    self.submit(frame, false);
                }
            }
            Err(error) => {
                self.reconnect_at = Some(Instant::now() + RECONNECT_INTERVAL);
                eprintln!("gpu: compositor unavailable: {error}");
            }
        }
    }

    fn presented(&mut self, msg: &[u8], stats: &mut FrameStats) {
        let serial = le_u32(msg, 8);
        if serial > self.presented_serial || self.presented_serial - serial > u32::MAX / 2 {
            self.presented_serial = serial;
            self.presented_at = (le_u64(msg, 16), le_u32(msg, 12));
            self.presented_render_ns = le_u64(msg, 24);
        }
        let sequence = le_u32(msg, 12);
        let time_ns = le_u64(msg, 16);
        let render_ns = le_u64(msg, 24);
        let path = match le_u32(msg, 32) {
            1 => "planes",
            2 => "gpu",
            3 => "cpu",
            _ => "?",
        };
        let flip_us = time_ns / 1000;
        let Some(in_flight) = self.in_flight.take_if(|f| serial >= f.serial) else {
            return;
        };
        if self.last_flip_us > 0 && sequence > self.last_sequence && flip_us > self.last_flip_us {
            let delta = u64::from(sequence - self.last_sequence);
            let interval_us = flip_us - self.last_flip_us;
            if delta == 1 && interval_us > 2_000 && interval_us < 40_000 {
                self.period_us = (self.period_us * 7 + interval_us) / 8;
            }
            let interval_ms = interval_us as f32 / 1000.0;
            let latency_ms = in_flight.sent.elapsed().as_secs_f32() * 1000.0;
            if stats.latency_ms.len() < 4096 {
                stats.frames += 1;
                stats.latency_ms.push(latency_ms);
                if in_flight.back_to_back {
                    stats.missed_vblanks += delta.saturating_sub(1);
                    stats.intervals_ms.push(interval_ms);
                }
            }
            if self.profile {
                eprintln!(
                    "gpu frame msc +{delta} interval {interval_ms:.2} ms submit-to-scanout {latency_ms:.2} ms {path} {:.2} ms{}",
                    render_ns as f64 / 1e6,
                    if in_flight.back_to_back {
                        ""
                    } else {
                        " (after idle)"
                    }
                );
            }
        }
        if sequence > 0 {
            self.last_sequence = sequence;
        }
        self.last_flip_us = flip_us;
        self.last_presented_at = Some(Instant::now());
    }

    /// While an app is in front the compositor shows the mirror by itself.
    pub fn set_mirror_mode<C: Connection>(&mut self, conn: &C, on: bool) {
        if on == self.mirror_mode {
            return;
        }
        self.mirror_mode = on;
        if on {
            // Everything must reach the mirror before it is shown: the next
            // copy issued includes the whole screen.
            self.mirror_damage.push(self.screen());
            self.mirror_frame_after = Some(self.flushes_issued + 1);
            if let Err(error) = self.flush_mirror(conn) {
                eprintln!("gpu: mirror copy failed: {error}");
            }
        } else {
            self.mirror_frame_after = None;
        }
    }

    fn send_mirror_frame(&mut self) {
        let mut msg = Msg::new(MSG_FRAME);
        self.serial = self.serial.wrapping_add(1).max(1);
        let direct: Vec<Direct> = self
            .direct
            .iter()
            .copied()
            .filter(|direct| {
                direct.surface.is_none_or(|surface| {
                    matches!(
                        self.surfaces.get(surface.0),
                        Some(Some(Surface::Client { .. }))
                    )
                })
            })
            .collect();
        msg.u32(self.serial)
            .u32(0)
            .u32(0)
            .u32(1 + direct.len() as u32);
        push_image(
            &mut msg,
            MIRROR_ID,
            self.full_rect(),
            self.full_rect(),
            1.0,
            1.0,
            0.0,
            false,
        );
        // A Wayland app in front: its own buffer over the mirror's (stale)
        // copy of it. The status strip and keyboard still come from the
        // mirror, beside the app's area.
        for direct in direct {
            push_image(
                &mut msg,
                direct.surface.map(wire).unwrap_or(MIRROR_ID),
                direct.src,
                direct.dst,
                1.0,
                1.0,
                direct.radius,
                false,
            );
        }
        let packet = msg.finish().to_vec();
        self.send(&packet, None);
        self.last_frame = None;
    }

    /// Whether Wayland client buffers can be shown directly (v3 offsets).
    pub fn supports_client_surfaces(&self) -> bool {
        self.version >= CLIENT_SURFACE_VERSION && self.link.borrow().connected()
    }

    /// A compositor surface on a client's wl_shm buffer (its pool's fd at
    /// `offset`). The compositor copies it on each damage (GPU tier) or
    /// reads it while drawing (CPU tier), so the buffer stays the client's
    /// only once a later frame no longer shows it.
    pub fn create_client_surface(
        &mut self,
        fd: &OwnedFd,
        offset: u32,
        width: u32,
        height: u32,
        stride: u32,
        argb: bool,
    ) -> io::Result<SurfaceId> {
        let fd = fd.try_clone()?;
        let id = self.insert(Surface::Client {
            fd,
            offset,
            width,
            height,
            stride,
            argb,
        });
        self.announce_client(id);
        Ok(id)
    }

    fn announce_client(&mut self, id: SurfaceId) {
        let Some(Some(Surface::Client {
            fd,
            offset,
            width,
            height,
            stride,
            argb,
        })) = self.surfaces.get(id.0)
        else {
            return;
        };
        let mut msg = Msg::new(MSG_SURFACE_SHM);
        msg.u32(wire(id))
            .u32(*width)
            .u32(*height)
            .u32(*stride)
            .u32(if *argb { FORMAT_ARGB } else { FORMAT_XRGB })
            .u32(if self.version >= FOREIGN_VERSION {
                SURFACE_FOREIGN
            } else {
                0
            })
            .u32(*offset);
        let raw = fd.as_raw_fd();
        let all = [Damage {
            x0: 0,
            y0: 0,
            x1: *width as i32,
            y1: *height as i32,
        }];
        let packet = msg.finish().to_vec();
        self.send(&packet, Some(raw));
        self.send_damage(MSG_SURFACE_DAMAGE, wire(id), &all);
    }

    /// The client drew `rects` (buffer pixels) into the surface's buffer.
    pub fn client_damage(&mut self, id: SurfaceId, rects: &[Damage]) {
        if !rects.is_empty() {
            self.damage_sent_ns = monotonic_ns();
            self.send_damage(MSG_SURFACE_DAMAGE, wire(id), rects);
        }
    }

    /// Shows a Wayland app's buffer over the mirror (or stops): a new frame
    /// at once while an app is in front, so each commit is one frame and
    /// its `presented_serial` says when the buffer reached the panel.
    pub fn set_direct(&mut self, direct: Vec<Direct>) -> Option<u32> {
        if self.version < CLIENT_SURFACE_VERSION {
            // A v2 compositor never heard of client surfaces: nodes naming
            // them must not reach it, even before the Wayland side notices.
            self.direct.clear();
            return None;
        }
        self.direct = direct;
        if self.mirror_mode && self.mirror.is_some() && self.link.borrow().connected() {
            self.send_mirror_frame();
            return Some(self.serial);
        }
        None
    }

    pub fn presented_serial(&self) -> u32 {
        self.presented_serial
    }

    /// CLOCK_MONOTONIC time (ns) and vblank counter of that flip.
    pub fn presented_at(&self) -> (u64, u32) {
        self.presented_at
    }

    /// The newest presented frame's render time in the compositor (ns).
    pub fn presented_render_ns(&self) -> u64 {
        self.presented_render_ns
    }

    /// When client damage was last sent and when the compositor last
    /// reported an upload done (CLOCK_MONOTONIC ns).
    pub fn upload_times(&self) -> (u64, u64) {
        (self.damage_sent_ns, self.uploaded_ns)
    }

    /// Whether the compositor guards client memory that may shrink (v4):
    /// then unsealed pools can be shown directly too.
    pub fn guards_foreign_memory(&self) -> bool {
        self.version >= FOREIGN_VERSION
    }

    /// Client surfaces the compositor gave up on since the last call.
    pub fn take_failed(&mut self) -> Vec<SurfaceId> {
        std::mem::take(&mut self.failed)
    }

    fn full_rect(&self) -> RectF {
        RectF::new(0.0, 0.0, f32::from(self.width), f32::from(self.height))
    }

    /// Resolves a surface to (wire id, brightness), following dimmed
    /// surfaces. A snapshot still being captured resolves: frames that
    /// show it are held until its copy has been sent.
    fn resolve(&self, id: SurfaceId) -> Option<(u32, f32)> {
        match self.surfaces.get(id.0)?.as_ref()? {
            // `dim` darkens, as a black overlay of that alpha would.
            Surface::Dimmed { source, dim } => {
                let (wire, base) = self.resolve(*source)?;
                Some((wire, base * (1.0 - dim)))
            }
            Surface::Snapshot { present: false } if !self.pending_capture(id) => None,
            Surface::Mirror => Some((MIRROR_ID, 1.0)),
            _ => Some((wire(id), 1.0)),
        }
    }

    fn pending_capture(&self, id: SurfaceId) -> bool {
        self.captures_waiting.contains(&id) || self.captures_after_flush.contains(&id)
    }

    /// Ships a frame; it is drawn at the next vblank.
    pub fn compose(&mut self, base: Option<SurfaceId>, nodes: &[Node]) -> bool {
        let mut msg = Msg::new(MSG_FRAME);
        self.serial = self.serial.wrapping_add(1).max(1);
        let mut held = false;
        let (flags, base_wire) = match base.and_then(|b| {
            held |= self.pending_capture(b);
            self.resolve(b)
        }) {
            Some((wire, _)) => (FRAME_HAS_BASE, wire),
            None => (0, 0),
        };
        let count_at = 20;
        msg.u32(self.serial).u32(flags).u32(base_wire).u32(0);
        let mut count = 0_u32;
        let mut uses_mirror = false;
        for node in nodes {
            match node {
                Node::Image {
                    surface,
                    src,
                    dst,
                    alpha,
                    radius,
                    smooth,
                } => {
                    held |= self.pending_capture(*surface);
                    if let Some((wire, src)) = self.card_for(*surface, *src, *dst) {
                        push_image(&mut msg, wire, src, *dst, *alpha, 1.0, *radius, *smooth);
                        count += 1;
                    } else if let Some((wire, dim)) = self.resolve(*surface) {
                        uses_mirror |= wire == MIRROR_ID;
                        push_image(&mut msg, wire, *src, *dst, *alpha, dim, *radius, *smooth);
                        count += 1;
                    }
                }
                Node::Fill {
                    dst,
                    color,
                    alpha,
                    radius,
                } => {
                    msg.u32(NODE_FILL).u32(0);
                    msg.f32(0.0).f32(0.0).f32(0.0).f32(0.0);
                    msg.f32(dst.x).f32(dst.y).f32(dst.w).f32(dst.h);
                    msg.f32(*alpha)
                        .f32(1.0)
                        .f32(*radius)
                        .u32(0)
                        .u32(*color & 0xff_ffff)
                        .u32(0);
                    count += 1;
                }
                Node::Rows { .. } => {
                    // Rows of darkened copies of one source: their mean
                    // brightness, exactly, in one pass.
                    let Some(first) = self.rows.first() else {
                        continue;
                    };
                    let source = match self.surfaces.get(first.0).and_then(Option::as_ref) {
                        Some(Surface::Dimmed { source, .. }) => *source,
                        _ => *first,
                    };
                    let dims: f32 = self
                        .rows
                        .iter()
                        .map(|row| self.resolve(*row).map_or(1.0, |(_, dim)| dim))
                        .sum();
                    let dim = dims / self.rows.len() as f32;
                    if let Some((wire, base_dim)) = self.resolve(source) {
                        let full = self.full_rect();
                        push_image(&mut msg, wire, full, full, 1.0, dim * base_dim, 0.0, false);
                        count += 1;
                    }
                }
            }
        }
        msg.0[count_at..count_at + 4].copy_from_slice(&count.to_le_bytes());
        self.frame_uses_mirror = uses_mirror;
        let packet = msg.finish().to_vec();
        if packet.len() > MAX_PACKET {
            eprintln!("gpu: frame too large ({} bytes); dropped", packet.len());
            return false;
        }
        self.last_frame = Some(packet.clone());
        if held {
            self.held_frame = Some(packet);
        } else {
            self.submit(packet, true);
        }
        true
    }

    fn submit(&mut self, packet: Vec<u8>, fresh: bool) {
        let serial = le_u32(&packet, 8);
        let back_to_back = fresh
            && self
                .last_presented_at
                .is_some_and(|at| at.elapsed() < Duration::from_millis(4));
        self.send(&packet, None);
        if self.link.borrow().connected() {
            self.in_flight = Some(InFlight {
                serial,
                sent: Instant::now(),
                back_to_back,
            });
        }
    }

    pub fn ready(&mut self, now: Instant) -> bool {
        match &self.in_flight {
            Some(in_flight) if now.duration_since(in_flight.sent) >= PRESENT_WATCHDOG => {
                eprintln!("gpu: frame {} not presented; continuing", in_flight.serial);
                self.in_flight = None;
                true
            }
            Some(_) => false,
            None => self.held_frame.is_none(),
        }
    }

    pub fn frame_in_flight(&self) -> bool {
        self.in_flight.is_some()
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let watchdog = self.in_flight.as_ref().map(|f| f.sent + PRESENT_WATCHDOG);
        match (watchdog, self.reconnect_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The vblank the next frame will reach.
    pub fn next_display_time(&self, now: Instant, now_us: u64) -> Instant {
        if self.last_flip_us == 0 {
            return now + Duration::from_micros(self.period_us);
        }
        let mut next = self.last_flip_us + self.period_us;
        while next + 500 < now_us {
            next += self.period_us;
        }
        now + Duration::from_micros(next.saturating_sub(now_us))
    }
}

#[allow(clippy::too_many_arguments)]
fn push_image(
    msg: &mut Msg,
    wire: u32,
    src: RectF,
    dst: RectF,
    alpha: f32,
    dim: f32,
    radius: f32,
    smooth: bool,
) {
    msg.u32(NODE_IMAGE).u32(wire);
    msg.f32(src.x).f32(src.y).f32(src.w).f32(src.h);
    msg.f32(dst.x).f32(dst.y).f32(dst.w).f32(dst.h);
    msg.f32(alpha)
        .f32(dim)
        .f32(radius)
        .u32(if smooth { NODE_SMOOTH } else { 0 })
        .u32(0)
        .u32(0);
}

/// Copies the 64-pixel chunks of each 16-row band that differ between `new`
/// and `shared` into `shared`, returning the changed rectangles merged down
/// across bands (everything when `valid` is false).
fn diff_into(
    new: &[u32],
    shared: &mut [u32],
    width: usize,
    height: usize,
    stride: usize,
    valid: bool,
) -> Vec<Damage> {
    let chunks = width.div_ceil(CHUNK);
    let mut dirty = vec![false; chunks];
    let mut open: Vec<Damage> = Vec::new();
    let mut closed: Vec<Damage> = Vec::new();
    let mut band_y = 0;
    while band_y < height {
        let band_end = (band_y + BAND_ROWS).min(height);
        dirty.fill(!valid);
        if valid {
            for y in band_y..band_end {
                let (a, b) = (
                    &new[y * width..(y + 1) * width],
                    &shared[y * stride..y * stride + width],
                );
                if a == b {
                    continue;
                }
                for (chunk, flag) in dirty.iter_mut().enumerate() {
                    if !*flag {
                        let span = chunk * CHUNK..((chunk + 1) * CHUNK).min(width);
                        *flag = a[span.clone()] != b[span];
                    }
                }
            }
        }
        let mut still_open = Vec::new();
        let mut chunk = 0;
        while chunk < chunks {
            if !dirty[chunk] {
                chunk += 1;
                continue;
            }
            let first = chunk;
            while chunk < chunks && dirty[chunk] {
                chunk += 1;
            }
            let (x0, x1) = (first * CHUNK, (chunk * CHUNK).min(width));
            for y in band_y..band_end {
                shared[y * stride + x0..y * stride + x1]
                    .copy_from_slice(&new[y * width + x0..y * width + x1]);
            }
            let (x0, x1) = (x0 as i32, x1 as i32);
            if let Some(position) = open
                .iter()
                .position(|d| d.x0 == x0 && d.x1 == x1 && d.y1 == band_y as i32)
            {
                let mut grown = open.swap_remove(position);
                grown.y1 = band_end as i32;
                still_open.push(grown);
            } else {
                still_open.push(Damage {
                    x0,
                    y0: band_y as i32,
                    x1,
                    y1: band_end as i32,
                });
            }
        }
        closed.append(&mut open);
        open = still_open;
        band_y = band_end;
    }
    closed.append(&mut open);
    closed
}

fn monotonic_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: ts is a valid timespec for clock_gettime to fill.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_copies_and_reports_only_changed_chunks() {
        let (w, h) = (200, 40);
        let mut shared = vec![0_u32; w * h];
        let mut canvas = vec![0_u32; w * h];
        assert_eq!(diff_into(&canvas, &mut shared, w, h, w, true), vec![]);
        canvas[20 * w + 130] = 7;
        let damage = diff_into(&canvas, &mut shared, w, h, w, true);
        assert_eq!(
            damage,
            vec![Damage {
                x0: 128,
                y0: 16,
                x1: 192,
                y1: 32
            }]
        );
        assert_eq!(shared[20 * w + 130], 7);
        let all = diff_into(&canvas, &mut shared, w, h, w, false);
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].x1, all[0].y1), (200, 40));
    }

    #[test]
    fn messages_carry_their_length() {
        let mut msg = Msg::new(MSG_POWER);
        msg.u32(1);
        let bytes = msg.finish();
        assert_eq!(bytes.len(), 12);
        assert_eq!(le_u32(bytes, 4), 12);
        assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), MSG_POWER);
    }
}
