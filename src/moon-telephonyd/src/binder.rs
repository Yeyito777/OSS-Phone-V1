//! A small client for Android's hwbinder (HIDL) transport: enough to look
//! services up in hwservicemanager, call them, and host the callback objects
//! they call back, from a musl process in the chroot with no Android
//! libraries. Wire formats are the binder kernel UAPI (64-bit) and
//! libhwbinder's Parcel; libgbinder (mer-hybris) implements the same thing in
//! C and was the reference.
//!
//! Single-threaded by design: the owner registers as a looper, polls `fd()`
//! and calls `process()`; synchronous calls wait on the same thread and
//! dispatch whatever arrives meanwhile (callbacks a service makes into us
//! while serving our call land on this thread, as binder requires).

use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration, Instant};

const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    (dir << 30) | ((size as u32) << 16) | ((ty as u32) << 8) | nr as u32
}
const fn io(ty: u8, nr: u8) -> u32 {
    ioc(0, ty, nr, 0)
}
const fn iow(ty: u8, nr: u8, size: usize) -> u32 {
    ioc(1, ty, nr, size)
}
const fn ior(ty: u8, nr: u8, size: usize) -> u32 {
    ioc(2, ty, nr, size)
}
const fn iowr(ty: u8, nr: u8, size: usize) -> u32 {
    ioc(3, ty, nr, size)
}

const TXN_SIZE: usize = 64; // struct binder_transaction_data
const BINDER_WRITE_READ: u32 = iowr(b'b', 1, 48);
const BINDER_SET_MAX_THREADS: u32 = iow(b'b', 5, 4);
const BINDER_VERSION: u32 = iowr(b'b', 9, 4);

const BC_TRANSACTION_SG: u32 = iow(b'c', 17, TXN_SIZE + 8);
const BC_REPLY_SG: u32 = iow(b'c', 18, TXN_SIZE + 8);
const BC_FREE_BUFFER: u32 = iow(b'c', 3, 8);
const BC_INCREFS: u32 = iow(b'c', 4, 4);
const BC_ACQUIRE: u32 = iow(b'c', 5, 4);
const BC_RELEASE: u32 = iow(b'c', 6, 4);
const BC_DECREFS: u32 = iow(b'c', 7, 4);
const BC_INCREFS_DONE: u32 = iow(b'c', 8, 16);
const BC_ACQUIRE_DONE: u32 = iow(b'c', 9, 16);
const BC_ENTER_LOOPER: u32 = io(b'c', 12);
const BC_REQUEST_DEATH_NOTIFICATION: u32 = iow(b'c', 14, 12);
const BC_CLEAR_DEATH_NOTIFICATION: u32 = iow(b'c', 15, 12);
const BC_DEAD_BINDER_DONE: u32 = iow(b'c', 16, 8);

const BR_ERROR: u32 = ior(b'r', 0, 4);
const BR_TRANSACTION: u32 = ior(b'r', 2, TXN_SIZE);
const BR_REPLY: u32 = ior(b'r', 3, TXN_SIZE);
const BR_DEAD_REPLY: u32 = io(b'r', 5);
const BR_TRANSACTION_COMPLETE: u32 = io(b'r', 6);
const BR_INCREFS: u32 = ior(b'r', 7, 16);
const BR_ACQUIRE: u32 = ior(b'r', 8, 16);
const BR_DEAD_BINDER: u32 = ior(b'r', 15, 8);
const BR_FAILED_REPLY: u32 = io(b'r', 17);
const BR_FROZEN_REPLY: u32 = io(b'r', 18);

const TF_ONE_WAY: u32 = 0x01;
const TF_STATUS_CODE: u32 = 0x08;
const TF_ACCEPT_FDS: u32 = 0x10;
const FLAT_BINDER_FLAG_ACCEPTS_FDS: u32 = 0x100;
const BUFFER_FLAG_HAS_PARENT: u32 = 0x01;

const fn pack(a: u8, b: u8, c: u8) -> u32 {
    ((a as u32) << 24) | ((b as u32) << 16) | ((c as u32) << 8) | 0x85
}
pub const TYPE_BINDER: u32 = pack(b's', b'b', b'*');
pub const TYPE_HANDLE: u32 = pack(b's', b'h', b'*');
const TYPE_PTR: u32 = pack(b'p', b't', b'*');
const FLAT_OBJECT_SIZE: usize = 24;
const BUFFER_OBJECT_SIZE: usize = 40;

const fn hidl(a: u8, b: u8, c: u8) -> u32 {
    (0x0f << 24) | ((a as u32) << 16) | ((b as u32) << 8) | c as u32
}
pub const HIDL_IBASE: &str = "android.hidl.base@1.0::IBase";
const HIDL_PING: u32 = hidl(b'P', b'N', b'G');
pub const HIDL_DESCRIPTOR_CHAIN: u32 = hidl(b'C', b'H', b'N');
const HIDL_GET_DESCRIPTOR: u32 = hidl(b'D', b'S', b'C');
const HIDL_HASH_CHAIN: u32 = hidl(b'H', b'S', b'H');
pub const HIDL_DEBUG_INFO: u32 = hidl(b'R', b'E', b'F');
const HIDL_DEBUG: u32 = hidl(b'D', b'B', b'G');

pub const HWSERVICEMANAGER: u32 = 0;
const SERVICE_MANAGER: &str = "android.hidl.manager@1.0::IServiceManager";
const SM_GET: u32 = 1;
/// libbinder's mapping size: 1 MiB minus two pages.
const MAP_SIZE: usize = 1024 * 1024 - 2 * 4096;
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    /// The remote process died (BR_DEAD_REPLY).
    Dead,
    Failed,
    /// A transport status code instead of a reply.
    Status(i32),
    Timeout,
    Protocol(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "binder I/O: {e}"),
            Error::Dead => write!(f, "remote binder is dead"),
            Error::Failed => write!(f, "binder transaction failed"),
            Error::Status(s) => write!(f, "binder status {s}"),
            Error::Timeout => write!(f, "binder call timed out"),
            Error::Protocol(what) => write!(f, "binder protocol: {what}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}
fn align8(n: usize) -> usize {
    (n + 7) & !7
}

/// An in-memory struct for a HIDL scatter-gather argument: the struct bytes
/// plus the buffers its embedded `hidl_string`/`hidl_vec` fields point to.
/// The kernel patches the pointers; we only fill sizes.
#[derive(Debug, Clone)]
pub struct Blob {
    pub bytes: Vec<u8>,
    children: Vec<(usize, Blob)>,
}

impl Blob {
    pub fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
            children: Vec::new(),
        }
    }
    pub fn i32(&mut self, offset: usize, value: i32) -> &mut Self {
        self.bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        self
    }
    pub fn u8(&mut self, offset: usize, value: u8) -> &mut Self {
        self.bytes[offset] = value;
        self
    }
    /// A `hidl_string` field (16 bytes: pointer, u32 size, bool owns).
    pub fn string(&mut self, offset: usize, value: &str) -> &mut Self {
        let mut data = value.as_bytes().to_vec();
        data.push(0);
        self.i32(offset + 8, value.len() as i32);
        self.children.push((
            offset,
            Blob {
                bytes: data,
                children: Vec::new(),
            },
        ));
        self
    }
    /// A `hidl_vec` field whose elements are `elements` (count * size bytes).
    pub fn vec(&mut self, offset: usize, count: usize, elements: Blob) -> &mut Self {
        self.i32(offset + 8, count as i32);
        self.children.push((offset, elements));
        self
    }
    /// Element array helper: `count` elements of `size` bytes, filled by `f`.
    pub fn array(count: usize, size: usize, mut f: impl FnMut(&mut Blob, usize, usize)) -> Blob {
        let mut blob = Blob::new(count * size);
        for i in 0..count {
            f(&mut blob, i * size, i);
        }
        blob
    }
}

/// An outgoing parcel. Buffer objects point at heap memory owned here; it
/// must live until the transaction has been written.
#[derive(Default)]
pub struct Parcel {
    data: Vec<u8>,
    offsets: Vec<u64>,
    buffers: Vec<Vec<u8>>,
    buffers_size: usize,
}

impl Parcel {
    /// A request: the interface token libhwbinder writes (a C string).
    pub fn request(interface: &str) -> Self {
        let mut parcel = Self::default();
        parcel.write_cstring(interface);
        parcel
    }
    /// A reply: HIDL's status header (EX_NONE).
    pub fn reply_ok() -> Self {
        let mut parcel = Self::default();
        parcel.write_i32(0);
        parcel
    }
    fn pad(&mut self) {
        let len = align4(self.data.len());
        self.data.resize(len, 0);
    }
    pub fn write_i32(&mut self, value: i32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }
    pub fn write_u32(&mut self, value: u32) {
        self.data.extend_from_slice(&value.to_le_bytes());
    }
    /// hwbinder writes bool (and uint8) as a 4-byte slot.
    pub fn write_bool(&mut self, value: bool) {
        self.write_i32(value as i32);
    }
    pub fn write_cstring(&mut self, value: &str) {
        self.data.extend_from_slice(value.as_bytes());
        self.data.push(0);
        self.pad();
    }
    fn write_buffer_object(&mut self, bytes: Vec<u8>, parent: Option<(usize, usize)>) -> usize {
        let index = self.offsets.len();
        self.offsets.push(self.data.len() as u64);
        let (flags, parent_index, parent_offset) = match parent {
            Some((i, o)) => (BUFFER_FLAG_HAS_PARENT, i as u64, o as u64),
            None => (0, 0, 0),
        };
        let pointer = if bytes.is_empty() {
            0
        } else {
            bytes.as_ptr() as u64
        };
        self.write_u32(TYPE_PTR);
        self.write_u32(flags);
        self.data.extend_from_slice(&pointer.to_le_bytes());
        self.data
            .extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        self.data.extend_from_slice(&parent_index.to_le_bytes());
        self.data.extend_from_slice(&parent_offset.to_le_bytes());
        self.buffers_size += align8(bytes.len());
        self.buffers.push(bytes);
        index
    }
    fn write_blob_with_parent(&mut self, blob: Blob, parent: Option<(usize, usize)>) {
        let Blob {
            bytes,
            mut children,
        } = blob;
        let index = self.write_buffer_object(bytes, parent);
        // The kernel validates fixups in increasing offset order per parent.
        children.sort_by_key(|(offset, _)| *offset);
        for (offset, child) in children {
            self.write_blob_with_parent(child, Some((index, offset)));
        }
    }
    /// A struct argument (and everything it points to), depth first.
    pub fn write_blob(&mut self, blob: Blob) {
        self.write_blob_with_parent(blob, None);
    }
    /// A top-level `hidl_string` argument.
    pub fn write_string(&mut self, value: &str) {
        let mut blob = Blob::new(16);
        blob.string(0, value);
        self.write_blob(blob);
    }
    /// A top-level `vec<string>` argument.
    pub fn write_string_vec(&mut self, values: &[String]) {
        let mut blob = Blob::new(16);
        let elements = Blob::array(values.len(), 16, |b, off, i| {
            b.string(off, &values[i]);
        });
        blob.vec(0, values.len(), elements);
        self.write_blob(blob);
    }
    /// A strong reference to one of our local objects.
    pub fn write_local_object(&mut self, ptr: u64) {
        self.offsets.push(self.data.len() as u64);
        self.write_u32(TYPE_BINDER);
        self.write_u32(0x7f | FLAT_BINDER_FLAG_ACCEPTS_FDS);
        self.data.extend_from_slice(&ptr.to_le_bytes());
        self.data.extend_from_slice(&ptr.to_le_bytes());
    }
}

/// The receive mapping, for validating pointers the kernel handed us.
#[derive(Clone, Copy)]
struct Mem {
    base: usize,
    len: usize,
}

impl Mem {
    fn slice<'a>(&self, ptr: u64, len: usize) -> Option<&'a [u8]> {
        if len == 0 {
            return Some(&[]);
        }
        let start = usize::try_from(ptr).ok()?;
        let end = start.checked_add(len)?;
        if start < self.base || end > self.base + self.len {
            return None;
        }
        // SAFETY: the range lies inside our live read-only binder mapping, and
        // the slice does not outlive the transaction buffer (callers drop it
        // before BC_FREE_BUFFER).
        Some(unsafe { std::slice::from_raw_parts(start as *const u8, len) })
    }
}

/// A view of a struct the kernel copied into our mapping.
#[derive(Clone, Copy)]
pub struct View<'a> {
    pub bytes: &'a [u8],
    mem: Mem,
}

impl<'a> View<'a> {
    pub fn i32(&self, offset: usize) -> i32 {
        self.bytes
            .get(offset..offset + 4)
            .map_or(0, |b| i32::from_le_bytes(b.try_into().unwrap()))
    }
    pub fn u8(&self, offset: usize) -> u8 {
        self.bytes.get(offset).copied().unwrap_or(0)
    }
    pub fn u64(&self, offset: usize) -> u64 {
        self.bytes
            .get(offset..offset + 8)
            .map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
    }
    /// The `hidl_string` at `offset` (empty if absent or invalid).
    pub fn string(&self, offset: usize) -> String {
        let len = self.i32(offset + 8).max(0) as usize;
        self.mem
            .slice(self.u64(offset), len)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .unwrap_or_default()
    }
    /// The `hidl_vec<T>` at `offset`, as views of `size`-byte elements.
    pub fn vec(&self, offset: usize, size: usize) -> Vec<View<'a>> {
        let count = self.i32(offset + 8).max(0) as usize;
        let Some(bytes) = count
            .checked_mul(size)
            .and_then(|total| self.mem.slice(self.u64(offset), total))
        else {
            return Vec::new();
        };
        bytes
            .chunks_exact(size.max(1))
            .map(|chunk| View {
                bytes: chunk,
                mem: self.mem,
            })
            .collect()
    }
    /// The `hidl_vec<uint8_t>` at `offset`.
    pub fn byte_vec(&self, offset: usize) -> Vec<u8> {
        let count = self.i32(offset + 8).max(0) as usize;
        self.mem
            .slice(self.u64(offset), count)
            .map(<[u8]>::to_vec)
            .unwrap_or_default()
    }
    pub fn sub(&self, offset: usize, len: usize) -> View<'a> {
        View {
            bytes: self.bytes.get(offset..offset + len).unwrap_or(&[]),
            mem: self.mem,
        }
    }
}

/// A received parcel (request or reply) in our mapping.
pub struct Reader<'a> {
    data: &'a [u8],
    offsets: Vec<u64>,
    pos: usize,
    mem: Mem,
    /// Handles in a reply to take strong references to before it is freed.
    pub keep: Vec<u32>,
}

pub struct Object {
    pub kind: u32,
    /// The handle for TYPE_HANDLE, the local pointer for TYPE_BINDER.
    pub value: u64,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let bytes = self.data.get(self.pos..self.pos + n)?;
        self.pos += align4(n);
        Some(bytes)
    }
    pub fn i32(&mut self) -> Option<i32> {
        self.take(4)
            .map(|b| i32::from_le_bytes(b.try_into().unwrap()))
    }
    pub fn i64(&mut self) -> Option<i64> {
        self.take(8)
            .map(|b| i64::from_le_bytes(b.try_into().unwrap()))
    }
    pub fn bool(&mut self) -> Option<bool> {
        self.i32().map(|v| v != 0)
    }
    pub fn cstring(&mut self) -> Option<String> {
        let rest = self.data.get(self.pos..)?;
        let len = rest.iter().position(|&b| b == 0)?;
        let text = String::from_utf8_lossy(&rest[..len]).into_owned();
        self.pos += align4(len + 1);
        Some(text)
    }
    fn at_object(&self) -> bool {
        self.offsets.contains(&(self.pos as u64))
    }
    pub fn object(&mut self) -> Option<Object> {
        if !self.at_object() {
            return None;
        }
        let bytes = self.data.get(self.pos..self.pos + FLAT_OBJECT_SIZE)?;
        let kind = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let value = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        self.pos += FLAT_OBJECT_SIZE;
        Some(Object { kind, value })
    }
    /// The next scatter-gather buffer (a struct or a top-level string/vec).
    pub fn buffer(&mut self) -> Option<View<'a>> {
        if !self.at_object() {
            return None;
        }
        let bytes = self.data.get(self.pos..self.pos + BUFFER_OBJECT_SIZE)?;
        if u32::from_le_bytes(bytes[0..4].try_into().unwrap()) != TYPE_PTR {
            return None;
        }
        let ptr = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let len = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
        self.pos += BUFFER_OBJECT_SIZE;
        let data = self.mem.slice(ptr, len)?;
        // Skip the child buffers (strings, vectors) that follow; the struct
        // view reaches them through its patched pointers.
        while self.at_object() {
            let next = self.data.get(self.pos..self.pos + BUFFER_OBJECT_SIZE)?;
            let kind = u32::from_le_bytes(next[0..4].try_into().unwrap());
            let flags = u32::from_le_bytes(next[4..8].try_into().unwrap());
            if kind != TYPE_PTR || flags & BUFFER_FLAG_HAS_PARENT == 0 {
                break;
            }
            self.pos += BUFFER_OBJECT_SIZE;
        }
        Some(View {
            bytes: data,
            mem: self.mem,
        })
    }
    /// A top-level `hidl_string` argument.
    pub fn string(&mut self) -> Option<String> {
        self.buffer().map(|v| v.string(0))
    }
    /// A top-level `vec<uint8_t>` argument.
    pub fn byte_vec(&mut self) -> Option<Vec<u8>> {
        self.buffer().map(|v| v.byte_vec(0))
    }
}

pub struct Incoming<'a> {
    /// Which local object (the value `add_object` returned).
    pub object: u64,
    pub code: u32,
    pub interface: String,
    pub data: Reader<'a>,
}

pub type Handler<'h> = dyn FnMut(Incoming<'_>) + 'h;

struct Transaction {
    target: u64,
    code: u32,
    flags: u32,
    data: u64,
    data_size: usize,
    offsets: u64,
    offsets_size: usize,
}

impl Transaction {
    fn parse(b: &[u8]) -> Self {
        let u64_at = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        Self {
            target: u64_at(0),
            code: u32_at(16),
            flags: u32_at(20),
            data_size: u64_at(32) as usize,
            offsets_size: u64_at(40) as usize,
            data: u64_at(48),
            offsets: u64_at(56),
        }
    }
}

enum Waiting {
    Nothing,
    Complete(u64),
    Reply,
}

enum Outcome {
    Complete,
    Reply(Transaction),
    Error(Error),
}

pub struct Binder {
    fd: OwnedFd,
    mem: Mem,
    /// Interface chains of our local objects; object pointer = index + 1.
    objects: Vec<Vec<String>>,
    out: Vec<u8>,
    sent: u64,
    completed: u64,
    deaths: Vec<u64>,
}

impl Binder {
    pub fn open(path: &str) -> Result<Self, Error> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        let fd: OwnedFd = file.into();
        let mut version: i32 = 0;
        // SAFETY: valid fd and a pointer to an i32, as BINDER_VERSION expects.
        if unsafe { libc::ioctl(fd.as_raw_fd(), BINDER_VERSION as _, &mut version) } < 0 {
            return Err(io::Error::last_os_error().into());
        }
        if version != 8 {
            return Err(Error::Protocol("binder protocol version is not 8 (64-bit)"));
        }
        let threads: u32 = 0;
        // SAFETY: valid fd and pointer to u32.
        unsafe { libc::ioctl(fd.as_raw_fd(), BINDER_SET_MAX_THREADS as _, &threads) };
        // SAFETY: a fresh private read-only mapping of the binder fd.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                MAP_SIZE,
                libc::PROT_READ,
                libc::MAP_PRIVATE | libc::MAP_NORESERVE,
                fd.as_raw_fd(),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            return Err(io::Error::last_os_error().into());
        }
        let mut binder = Self {
            fd,
            mem: Mem {
                base: map as usize,
                len: MAP_SIZE,
            },
            objects: Vec::new(),
            out: Vec::new(),
            sent: 0,
            completed: 0,
            deaths: Vec::new(),
        };
        binder.command(BC_ENTER_LOOPER, &[]);
        binder.flush()?;
        Ok(binder)
    }

    pub fn fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    /// Hosts a local object implementing `chain` (most derived first;
    /// android.hidl.base@1.0::IBase is appended). Returns its pointer value.
    pub fn add_object(&mut self, chain: &[&str]) -> u64 {
        let mut chain: Vec<String> = chain.iter().map(|s| s.to_string()).collect();
        chain.push("android.hidl.base@1.0::IBase".into());
        self.objects.push(chain);
        self.objects.len() as u64
    }

    /// Cookies of remote objects that died since the last call.
    pub fn take_deaths(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.deaths)
    }

    fn command(&mut self, cmd: u32, payload: &[u8]) {
        self.out.extend_from_slice(&cmd.to_le_bytes());
        self.out.extend_from_slice(payload);
    }

    fn write_read(&mut self, read: Option<&mut [u8]>) -> io::Result<usize> {
        // struct binder_write_read: write_size, write_consumed, write_buffer,
        // read_size, read_consumed, read_buffer (all 64-bit).
        let mut bwr = [0u64; 6];
        bwr[0] = self.out.len() as u64;
        bwr[2] = self.out.as_ptr() as u64;
        if let Some(buf) = read.as_ref() {
            bwr[3] = buf.len() as u64;
            bwr[5] = buf.as_ptr() as u64;
        }
        // SAFETY: bwr describes live buffers of the stated sizes.
        let rc = unsafe {
            libc::ioctl(
                self.fd.as_raw_fd(),
                BINDER_WRITE_READ as _,
                bwr.as_mut_ptr(),
            )
        };
        let consumed = (bwr[1] as usize).min(self.out.len());
        self.out.drain(..consumed);
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(bwr[4] as usize)
    }

    fn flush(&mut self) -> io::Result<()> {
        while !self.out.is_empty() {
            match self.write_read(None) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn wait_readable(&self, deadline: Instant) -> Result<(), Error> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Timeout);
        }
        let mut pfd = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let rc = unsafe { libc::poll(&mut pfd, 1, left.as_millis().max(1) as i32) };
        if rc < 0 {
            let e = io::Error::last_os_error();
            if e.kind() != io::ErrorKind::Interrupted {
                return Err(e.into());
            }
        }
        Ok(())
    }

    /// Reads and dispatches whatever is pending, without blocking.
    pub fn process(&mut self, handler: &mut Handler) -> Result<(), Error> {
        loop {
            self.flush()?;
            let mut buf = [0u8; 1024];
            let n = match self.write_read(Some(&mut buf)) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            if n == 0 {
                return Ok(());
            }
            if let Some(Outcome::Error(e)) = self.dispatch(&buf[..n], Waiting::Nothing, handler) {
                return Err(e);
            }
        }
    }

    /// Parses one read buffer. Returns the reply (or failure) we were waiting
    /// for, if it arrived; everything else is handled in place.
    fn dispatch(&mut self, buf: &[u8], waiting: Waiting, handler: &mut Handler) -> Option<Outcome> {
        let mut outcome = None;
        let mut pos = 0;
        while pos + 4 <= buf.len() {
            let cmd = u32::from_le_bytes(buf[pos..pos + 4].try_into().unwrap());
            let size = ((cmd >> 16) & 0x3fff) as usize;
            pos += 4;
            let Some(payload) = buf.get(pos..pos + size) else {
                break;
            };
            pos += size;
            match cmd {
                BR_TRANSACTION => self.incoming(Transaction::parse(payload), handler),
                BR_REPLY => outcome = Some(Outcome::Reply(Transaction::parse(payload))),
                BR_TRANSACTION_COMPLETE => {
                    self.completed += 1;
                }
                BR_DEAD_REPLY => outcome = Some(Outcome::Error(Error::Dead)),
                BR_FAILED_REPLY | BR_FROZEN_REPLY => outcome = Some(Outcome::Error(Error::Failed)),
                BR_ERROR => {
                    let code = i32::from_le_bytes(payload.try_into().unwrap());
                    outcome = Some(Outcome::Error(Error::Status(code)));
                }
                BR_INCREFS => self.command(BC_INCREFS_DONE, payload),
                BR_ACQUIRE => self.command(BC_ACQUIRE_DONE, payload),
                BR_DEAD_BINDER => {
                    self.deaths
                        .push(u64::from_le_bytes(payload.try_into().unwrap()));
                    self.command(BC_DEAD_BINDER_DONE, payload);
                }
                // BR_NOOP, BR_SPAWN_LOOPER, BR_RELEASE, BR_DECREFS,
                // BR_CLEAR_DEATH_NOTIFICATION_DONE, BR_ONEWAY_SPAM_SUSPECT...
                _ => {}
            }
        }
        match waiting {
            Waiting::Complete(seq) if self.completed >= seq => outcome.or(Some(Outcome::Complete)),
            Waiting::Complete(_) => outcome.filter(|o| matches!(o, Outcome::Error(_))),
            _ => outcome,
        }
    }

    /// The reader borrows the mapping, not `self`: it must not outlive the
    /// BC_FREE_BUFFER of its transaction, which every caller sends after
    /// dropping it.
    fn reader(&self, t: &Transaction) -> Reader<'static> {
        let data = self.mem.slice(t.data, t.data_size).unwrap_or(&[]);
        let offsets = self
            .mem
            .slice(t.offsets, t.offsets_size)
            .unwrap_or(&[])
            .chunks_exact(8)
            .map(|c| u64::from_le_bytes(c.try_into().unwrap()))
            .collect();
        Reader {
            data,
            offsets,
            pos: 0,
            mem: self.mem,
            keep: Vec::new(),
        }
    }

    fn incoming(&mut self, t: Transaction, handler: &mut Handler) {
        let oneway = t.flags & TF_ONE_WAY != 0;
        let mut data = self.reader(&t);
        let interface = data.cstring().unwrap_or_default();
        let chain = usize::try_from(t.target)
            .ok()
            .and_then(|p| p.checked_sub(1))
            .and_then(|i| self.objects.get(i))
            .cloned();
        let reply = match (chain, t.code) {
            (None, _) => None,
            (Some(_), HIDL_PING) | (Some(_), HIDL_DEBUG) => Some(Parcel::reply_ok()),
            (Some(chain), HIDL_DESCRIPTOR_CHAIN) => {
                let mut p = Parcel::reply_ok();
                p.write_string_vec(&chain);
                Some(p)
            }
            (Some(chain), HIDL_GET_DESCRIPTOR) => {
                let mut p = Parcel::reply_ok();
                p.write_string(&chain[0]);
                Some(p)
            }
            (Some(chain), HIDL_HASH_CHAIN) => {
                let mut p = Parcel::reply_ok();
                let mut blob = Blob::new(16);
                blob.vec(0, chain.len(), Blob::new(32 * chain.len()));
                p.write_blob(blob);
                Some(p)
            }
            (Some(_), HIDL_DEBUG_INFO) => {
                let mut p = Parcel::reply_ok();
                let mut blob = Blob::new(24);
                blob.i32(0, std::process::id() as i32)
                    .i32(16, 2 /* IS_64BIT */);
                p.write_blob(blob);
                Some(p)
            }
            (Some(_), code) => {
                handler(Incoming {
                    object: t.target,
                    code,
                    interface,
                    data,
                });
                Some(Parcel::reply_ok())
            }
        };
        if !oneway {
            let reply = reply.unwrap_or_else(|| {
                let mut p = Parcel::default();
                p.write_i32(-1); // EX_SECURITY-ish: unknown object
                p
            });
            self.send(BC_REPLY_SG, 0, 0, &reply, 0);
        }
        self.command(BC_FREE_BUFFER, &t.data.to_le_bytes());
    }

    fn send(&mut self, cmd: u32, handle: u32, code: u32, parcel: &Parcel, flags: u32) {
        let mut tx = Vec::with_capacity(TXN_SIZE + 8);
        tx.extend_from_slice(&(handle as u64).to_le_bytes()); // target
        tx.extend_from_slice(&0u64.to_le_bytes()); // cookie
        tx.extend_from_slice(&code.to_le_bytes());
        tx.extend_from_slice(&flags.to_le_bytes());
        tx.extend_from_slice(&0i32.to_le_bytes()); // sender_pid
        tx.extend_from_slice(&0u32.to_le_bytes()); // sender_euid
        tx.extend_from_slice(&(parcel.data.len() as u64).to_le_bytes());
        tx.extend_from_slice(&((parcel.offsets.len() * 8) as u64).to_le_bytes());
        tx.extend_from_slice(&(parcel.data.as_ptr() as u64).to_le_bytes());
        tx.extend_from_slice(&(parcel.offsets.as_ptr() as u64).to_le_bytes());
        tx.extend_from_slice(&(align8(parcel.buffers_size) as u64).to_le_bytes());
        self.command(cmd, &tx);
        self.sent += 1;
    }

    fn wait(
        &mut self,
        waiting: impl Fn() -> Waiting,
        handler: &mut Handler,
    ) -> Result<Outcome, Error> {
        let deadline = Instant::now() + CALL_TIMEOUT;
        loop {
            let mut buf = [0u8; 1024];
            match self.write_read(Some(&mut buf)) {
                Ok(0) => {}
                Ok(n) => {
                    if let Some(outcome) = self.dispatch(&buf[..n], waiting(), handler) {
                        return Ok(outcome);
                    }
                    continue;
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
            self.wait_readable(deadline)?;
        }
    }

    /// A one-way call. Returns once the kernel accepted it.
    pub fn send_oneway(
        &mut self,
        handle: u32,
        code: u32,
        parcel: &Parcel,
        handler: &mut Handler,
    ) -> Result<(), Error> {
        self.flush()?;
        self.send(
            BC_TRANSACTION_SG,
            handle,
            code,
            parcel,
            TF_ONE_WAY | TF_ACCEPT_FDS,
        );
        // Every BC_TRANSACTION/BC_REPLY earns one BR_TRANSACTION_COMPLETE, in
        // order, so ours is the `sent`-th.
        let seq = self.sent;
        // The parcel's buffers must stay alive until the write is consumed.
        self.flush()?;
        match self.wait(|| Waiting::Complete(seq), handler)? {
            Outcome::Complete => Ok(()),
            Outcome::Error(e) => Err(e),
            Outcome::Reply(_) => Err(Error::Protocol("reply to a one-way call")),
        }
    }

    /// A synchronous call; `read` sees the reply after HIDL's status header.
    pub fn call<T>(
        &mut self,
        handle: u32,
        code: u32,
        parcel: &Parcel,
        handler: &mut Handler,
        read: impl FnOnce(&mut Reader) -> T,
    ) -> Result<T, Error> {
        self.flush()?;
        self.send(BC_TRANSACTION_SG, handle, code, parcel, TF_ACCEPT_FDS);
        self.flush()?;
        match self.wait(|| Waiting::Reply, handler)? {
            Outcome::Error(e) => Err(e),
            Outcome::Complete => Err(Error::Protocol("no reply")),
            Outcome::Reply(t) => {
                let result = if t.flags & TF_STATUS_CODE != 0 {
                    let status = self.reader(&t).i32().unwrap_or(-1);
                    Err(Error::Status(status))
                } else {
                    let mut reader = self.reader(&t);
                    let result = match reader.i32() {
                        Some(0) => Ok(read(&mut reader)),
                        Some(ex) => Err(Error::Status(ex)),
                        None => Err(Error::Protocol("empty reply")),
                    };
                    // Take our own references before the reply that carries
                    // the handles is freed.
                    for handle in reader.keep {
                        self.command(BC_INCREFS, &handle.to_le_bytes());
                        self.command(BC_ACQUIRE, &handle.to_le_bytes());
                    }
                    result
                };
                self.command(BC_FREE_BUFFER, &t.data.to_le_bytes());
                self.flush()?;
                result
            }
        }
    }

    /// hwservicemanager's get(); returns a handle we hold a strong reference
    /// to, or None if the service is not registered.
    pub fn get_service(
        &mut self,
        fqname: &str,
        instance: &str,
        handler: &mut Handler,
    ) -> Result<Option<u32>, Error> {
        let mut parcel = Parcel::request(SERVICE_MANAGER);
        parcel.write_string(fqname);
        parcel.write_string(instance);
        self.call(HWSERVICEMANAGER, SM_GET, &parcel, handler, |r| {
            let object = r.object().filter(|o| o.kind == TYPE_HANDLE)?;
            r.keep.push(object.value as u32);
            Some(object.value as u32)
        })
    }

    pub fn release(&mut self, handle: u32) -> Result<(), Error> {
        self.command(BC_RELEASE, &handle.to_le_bytes());
        self.command(BC_DECREFS, &handle.to_le_bytes());
        Ok(self.flush()?)
    }

    /// Asks for BR_DEAD_BINDER with `cookie` when the handle's process dies.
    pub fn link_to_death(&mut self, handle: u32, cookie: u64) -> Result<(), Error> {
        let mut payload = handle.to_le_bytes().to_vec();
        payload.extend_from_slice(&cookie.to_le_bytes());
        self.command(BC_REQUEST_DEATH_NOTIFICATION, &payload);
        Ok(self.flush()?)
    }

    pub fn unlink_to_death(&mut self, handle: u32, cookie: u64) -> Result<(), Error> {
        let mut payload = handle.to_le_bytes().to_vec();
        payload.extend_from_slice(&cookie.to_le_bytes());
        self.command(BC_CLEAR_DEATH_NOTIFICATION, &payload);
        Ok(self.flush()?)
    }
}

impl Drop for Binder {
    fn drop(&mut self) {
        // SAFETY: unmapping exactly the mapping created in open().
        unsafe { libc::munmap(self.mem.base as *mut libc::c_void, self.mem.len) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_the_uapi() {
        assert_eq!(BINDER_WRITE_READ, 0xc030_6201);
        assert_eq!(BC_TRANSACTION_SG, 0x4048_6311);
        assert_eq!(BC_FREE_BUFFER, 0x4008_6303);
        assert_eq!(BC_REQUEST_DEATH_NOTIFICATION, 0x400c_630e);
        assert_eq!(BR_TRANSACTION, 0x8040_7202);
        assert_eq!(BR_DEAD_BINDER, 0x8008_720f);
        assert_eq!(BC_ENTER_LOOPER, 0x630c);
        assert_eq!(TYPE_PTR, 0x7074_2a85);
        assert_eq!(HIDL_DESCRIPTOR_CHAIN, 0x0f43_484e);
    }

    #[test]
    fn strings_are_children_of_their_struct() {
        let mut parcel = Parcel::request("x");
        let mut blob = Blob::new(32);
        blob.string(16, "bb").string(0, "a");
        parcel.write_blob(blob);
        // header "x\0" padded to 4, then three buffer objects.
        assert_eq!(parcel.offsets, vec![4, 44, 84]);
        assert_eq!(parcel.buffers[1], b"a\0");
        assert_eq!(parcel.buffers[2], b"bb\0");
        assert_eq!(parcel.buffers_size, 32 + 8 + 8);
        let parent_offset =
            |obj: usize| u64::from_le_bytes(parcel.data[obj + 32..obj + 40].try_into().unwrap());
        assert_eq!(parent_offset(44), 0);
        assert_eq!(parent_offset(84), 16);
        assert_eq!(&parcel.buffers[0][24..28], &2i32.to_le_bytes());
    }
}
