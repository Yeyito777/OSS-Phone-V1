//! wl_shm: shared-memory buffers.
//!
//! Moon never maps client memory itself. A pool's fd is handed to whoever
//! shows the buffer (milestone 1: Xorg, through MIT-SHM `AttachFd`), which
//! reads the pixels in place; a buffer is released once that copy is done.

use std::os::fd::OwnedFd;
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
            data_init.init(
                id,
                Arc::new(ShmPool {
                    id: NEXT_POOL.fetch_add(1, Ordering::Relaxed),
                    fd,
                    size: Mutex::new((size, 0)),
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
                *current = (size, current.1.wrapping_add(1));
            }
            wl_shm_pool::Request::Destroy => {}
            _ => {}
        }
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
