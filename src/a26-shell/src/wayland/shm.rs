//! wl_shm: shared-memory buffers.
//!
//! Moon never maps client memory itself. A pool's fd is handed to whoever
//! shows the buffer (milestone 1: Xorg, through MIT-SHM `AttachFd`), which
//! reads the pixels in place; a buffer is released once that copy is done.

use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use wayland_server::protocol::wl_buffer::{self, WlBuffer};
use wayland_server::protocol::wl_shm::{self, WlShm};
use wayland_server::protocol::wl_shm_pool::{self, WlShmPool};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

use super::WlState;

static NEXT_POOL: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct ShmPool {
    /// Unique for Moon's lifetime: the key of the pool's MIT-SHM segment.
    pub id: u64,
    pub fd: OwnedFd,
    /// (size, generation): a resize bumps the generation, and a segment
    /// attached at an older size must be attached again.
    pub size: Mutex<(i32, u32)>,
    /// The file cannot shrink (F_SEAL_SHRINK): safe for moon-compositor,
    /// a separate process that would die of SIGBUS, to map.
    pub sealed: bool,
}

impl ShmPool {
    pub fn size(&self) -> (i32, u32) {
        *self
            .size
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

#[derive(Debug)]
pub struct ShmBuffer {
    pub pool: Arc<ShmPool>,
    pub offset: i32,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
}

pub(crate) fn create_globals(handle: &DisplayHandle) {
    handle.create_global::<WlState, WlShm, ()>(1, ());
}

impl GlobalDispatch<WlShm, ()> for WlState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WlShm>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let shm = data_init.init(resource, ());
        shm.format(wl_shm::Format::Argb8888);
        shm.format(wl_shm::Format::Xrgb8888);
    }
}

impl Dispatch<WlShm, ()> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        resource: &WlShm,
        request: wl_shm::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let wl_shm::Request::CreatePool { id, fd, size } = request {
            if size <= 0 {
                resource.post_error(wl_shm::Error::InvalidStride, "pool size must be positive");
                return;
            }
            // Whoever maps the pool (Xorg now, moon-compositor later) takes
            // SIGBUS if the file is smaller than the pool or shrinks later:
            // refuse the first, and seal against the second where the client
            // allowed sealing (memfd with MFD_ALLOW_SEALING).
            if file_size(&fd).is_none_or(|actual| actual < i64::from(size)) {
                resource.post_error(wl_shm::Error::InvalidFd, "pool larger than its file");
                return;
            }
            let sealed = seal_shrink(&fd);
            data_init.init(
                id,
                Arc::new(ShmPool {
                    id: NEXT_POOL.fetch_add(1, Ordering::Relaxed),
                    fd,
                    size: Mutex::new((size, 0)),
                    sealed,
                }),
            );
        }
    }
}

impl Dispatch<WlShmPool, Arc<ShmPool>> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        resource: &WlShmPool,
        request: wl_shm_pool::Request,
        pool: &Arc<ShmPool>,
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_shm_pool::Request::CreateBuffer {
                id,
                offset,
                width,
                height,
                stride,
                format,
            } => {
                match format.into_result() {
                    Ok(wl_shm::Format::Argb8888 | wl_shm::Format::Xrgb8888) => {}
                    _ => {
                        resource.post_error(wl_shm::Error::InvalidFormat, "unsupported format");
                        return;
                    }
                }
                let (size, _) = pool.size();
                let valid = width > 0
                    && height > 0
                    && offset >= 0
                    && stride >= width.saturating_mul(4)
                    && stride % 4 == 0
                    && i64::from(offset) + i64::from(stride) * i64::from(height) <= i64::from(size);
                if !valid {
                    resource.post_error(wl_shm::Error::InvalidStride, "buffer outside its pool");
                    return;
                }
                data_init.init(
                    id,
                    Arc::new(ShmBuffer {
                        pool: pool.clone(),
                        offset,
                        width,
                        height,
                        stride,
                    }),
                );
            }
            wl_shm_pool::Request::Resize { size } => {
                let mut current = pool
                    .size
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                if size < current.0 {
                    resource.post_error(wl_shm::Error::InvalidFd, "pools cannot shrink");
                    return;
                }
                if file_size(&pool.fd).is_none_or(|actual| actual < i64::from(size)) {
                    resource.post_error(wl_shm::Error::InvalidFd, "pool larger than its file");
                    return;
                }
                *current = (size, current.1.wrapping_add(1));
            }
            wl_shm_pool::Request::Destroy => {}
            _ => {}
        }
    }
}

fn file_size(fd: &OwnedFd) -> Option<i64> {
    // SAFETY: fstat fills one stat for a valid descriptor.
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::fstat(fd.as_raw_fd(), &mut stat) };
    (result == 0).then_some(stat.st_size)
}

/// Best effort: a client that did not allow sealing keeps an unsealed pool.
/// Returns whether the file is now sealed against shrinking.
fn seal_shrink(fd: &OwnedFd) -> bool {
    // SAFETY: F_ADD_SEALS / F_GET_SEALS on a valid descriptor; failure
    // changes nothing.
    unsafe {
        libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, libc::F_SEAL_SHRINK);
        let seals = libc::fcntl(fd.as_raw_fd(), libc::F_GET_SEALS);
        seals >= 0 && seals & libc::F_SEAL_SHRINK != 0
    }
}

impl Dispatch<WlBuffer, Arc<ShmBuffer>> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlBuffer,
        _request: wl_buffer::Request,
        _data: &Arc<ShmBuffer>,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Destroy only; the copy Moon may still hold keeps the pool alive.
    }
}
