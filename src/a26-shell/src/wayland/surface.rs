//! wl_compositor, wl_surface, wl_region, wl_callback, wl_subcompositor.
//!
//! Surface state is double-buffered as the protocol says: attach, damage
//! and frame requests collect in `pending` and take effect on commit. A
//! subsurface's position takes effect on its parent's commit; its content is
//! applied at its own commit (desynchronized), which is all a full-screen
//! phone app needs.

use std::sync::Arc;

use wayland_server::backend::ClientId;
use wayland_server::protocol::wl_buffer::WlBuffer;
use wayland_server::protocol::wl_callback::WlCallback;
use wayland_server::protocol::wl_compositor::{self, WlCompositor};
use wayland_server::protocol::wl_region::{self, WlRegion};
use wayland_server::protocol::wl_subcompositor::{self, WlSubcompositor};
use wayland_server::protocol::wl_subsurface::{self, WlSubsurface};
use wayland_server::protocol::wl_surface::{self, WlSurface};
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

use super::shm::ShmBuffer;
use super::{Event, Placement, Rect, SurfaceKey, WlState};

pub(crate) struct SurfaceData {
    pub key: SurfaceKey,
    pub resource: WlSurface,
    pending_buffer: Option<Option<WlBuffer>>,
    /// Damage, and whether it is in buffer pixels (damage_buffer) rather
    /// than surface coordinates.
    pending_damage: Vec<(Rect, bool)>,
    pending_callbacks: Vec<WlCallback>,
    pending_scale: Option<i32>,
    /// Buffer pixels per surface unit (wl_surface.set_buffer_scale).
    pub scale: i32,
    /// Committed frame callbacks, waiting until Moon shows the content.
    pub frame_callbacks: Vec<WlCallback>,
    pub buffer: Option<(WlBuffer, Arc<ShmBuffer>)>,
    pub placement: Option<Placement>,
    /// A subsurface's next position, applied on the parent's commit.
    pending_position: Option<(i32, i32)>,
    pub children: Vec<SurfaceKey>,
    /// xdg_surface bookkeeping lives in shell.rs; this only says the
    /// surface may show content (it acked a configure, or is a subsurface).
    pub can_show: bool,
}

pub(crate) fn create_globals(handle: &DisplayHandle) {
    handle.create_global::<WlState, WlCompositor, ()>(6, ());
    handle.create_global::<WlState, WlSubcompositor, ()>(1, ());
}

impl GlobalDispatch<WlCompositor, ()> for WlState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WlCompositor>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WlCompositor, ()> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WlCompositor,
        request: wl_compositor::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                let surface = data_init.init(id, ());
                state.next_key = state.next_key.wrapping_add(1).max(1);
                let key = SurfaceKey(state.next_key);
                state.keys.insert(key, surface.id());
                if surface.version() >= 6 {
                    surface.preferred_buffer_scale(state.scale);
                }
                state.surfaces.insert(
                    surface.id(),
                    SurfaceData {
                        key,
                        resource: surface.clone(),
                        pending_buffer: None,
                        pending_damage: Vec::new(),
                        pending_callbacks: Vec::new(),
                        pending_scale: None,
                        scale: 1,
                        frame_callbacks: Vec::new(),
                        buffer: None,
                        placement: None,
                        pending_position: None,
                        children: Vec::new(),
                        can_show: false,
                    },
                );
            }
            wl_compositor::Request::CreateRegion { id } => {
                data_init.init(id, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<WlRegion, ()> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlRegion,
        _request: wl_region::Request,
        _data: &(),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        // Input and opaque regions do not change anything for full-screen
        // apps; the requests are accepted and ignored.
    }
}

impl Dispatch<WlCallback, ()> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlCallback,
        _request: <WlCallback as Resource>::Request,
        _data: &(),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

impl Dispatch<WlSurface, ()> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &WlSurface,
        request: wl_surface::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let id = resource.id();
        match request {
            wl_surface::Request::Attach { buffer, .. } => {
                if let Some(data) = state.surfaces.get_mut(&id) {
                    data.pending_buffer = Some(buffer);
                }
            }
            wl_surface::Request::Damage {
                x,
                y,
                width,
                height,
            } => {
                if let Some(data) = state.surfaces.get_mut(&id) {
                    data.pending_damage
                        .push((Rect::new(x, y, width, height), false));
                }
            }
            wl_surface::Request::DamageBuffer {
                x,
                y,
                width,
                height,
            } => {
                if let Some(data) = state.surfaces.get_mut(&id) {
                    data.pending_damage.push((Rect::new(x, y, width, height), true));
                }
            }
            wl_surface::Request::SetBufferScale { scale } => {
                if scale < 1 {
                    resource.post_error(wl_surface::Error::InvalidScale, "scale must be positive");
                } else if let Some(data) = state.surfaces.get_mut(&id) {
                    data.pending_scale = Some(scale);
                }
            }
            wl_surface::Request::Frame { callback } => {
                let callback = data_init.init(callback, ());
                if let Some(data) = state.surfaces.get_mut(&id) {
                    data.pending_callbacks.push(callback);
                }
            }
            wl_surface::Request::Commit => commit(state, resource),
            wl_surface::Request::Destroy => {}
            // Opaque/input regions, transform and offset: accepted; Moon
            // shows every buffer untransformed.
            _ => {}
        }
    }

    fn destroyed(state: &mut Self, _client: ClientId, resource: &WlSurface, _data: &()) {
        destroy_surface(state, &resource.id());
    }
}

fn destroy_surface(state: &mut WlState, id: &wayland_server::backend::ObjectId) {
    let Some(data) = state.surfaces.remove(id) else {
        return;
    };
    state.keys.remove(&data.key);
    let toplevel = match data.placement {
        Some(Placement::Toplevel(toplevel)) => Some(toplevel),
        _ => None,
    };
    if let Some(toplevel) = toplevel {
        state.toplevels.remove(&toplevel);
    }
    if let Some(Placement::Popup { parent, .. } | Placement::Subsurface { parent, .. }) =
        data.placement
        && let Some(parent) = state.surface_by_key_mut(parent)
    {
        parent.children.retain(|child| *child != data.key);
    }
    state.seat.forget_surface(data.key);
    state.text_input.forget_surface(&data.resource);
    if data.placement.is_some() {
        state.outbox.push(Event::Destroyed {
            surface: data.key,
            toplevel,
        });
    }
}

/// A surface lost its role (xdg_surface or wl_subsurface destroyed): its
/// window goes, the wl_surface stays.
pub(crate) fn drop_role(state: &mut WlState, key: SurfaceKey) {
    let Some(data) = state.surface_by_key_mut(key) else {
        return;
    };
    let placement = data.placement.take();
    data.can_show = false;
    data.buffer = None;
    let toplevel = match placement {
        Some(Placement::Toplevel(toplevel)) => Some(toplevel),
        _ => None,
    };
    if let Some(toplevel) = toplevel {
        state.toplevels.remove(&toplevel);
    }
    if let Some(Placement::Popup { parent, .. } | Placement::Subsurface { parent, .. }) = placement
        && let Some(parent) = state.surface_by_key_mut(parent)
    {
        parent.children.retain(|child| *child != key);
    }
    if placement.is_some() {
        state.outbox.push(Event::Destroyed {
            surface: key,
            toplevel,
        });
    }
}

pub(crate) fn assign_role(state: &mut WlState, key: SurfaceKey, placement: Placement) {
    if let Placement::Popup { parent, .. } | Placement::Subsurface { parent, .. } = placement
        && let Some(parent) = state.surface_by_key_mut(parent)
    {
        parent.children.push(key);
    }
    if let Some(data) = state.surface_by_key_mut(key) {
        data.placement = Some(placement);
    }
    // The surface is on the panel's output: clients pick their scale from it.
    if let Some(surface) = state.surface_by_key(key).map(|data| data.resource.clone()) {
        for output in &state.outputs {
            if output.id().same_client_as(&surface.id()) {
                surface.enter(output);
            }
        }
    }
    if !matches!(placement, Placement::Toplevel(_)) {
        state.outbox.push(Event::SurfaceCreated {
            surface: key,
            placement: state.placement_px(placement),
        });
    }
}

fn commit(state: &mut WlState, resource: &WlSurface) {
    let id = resource.id();
    // xdg-shell: the first commit asks for the first configure.
    super::shell::on_commit(state, resource);
    let Some(data) = state.surfaces.get_mut(&id) else {
        return;
    };
    let key = data.key;
    let callbacks = std::mem::take(&mut data.pending_callbacks);
    data.frame_callbacks.extend(callbacks);
    let damage = std::mem::take(&mut data.pending_damage);
    let attached = data.pending_buffer.take();
    if let Some(scale) = data.pending_scale.take() {
        data.scale = scale;
    }
    let scale = data.scale;
    let mut event = None;
    if data.placement.is_some() && data.can_show {
        match attached {
            Some(Some(buffer)) => match buffer.data::<Arc<ShmBuffer>>() {
                Some(shm) => {
                    let (width, height) = (shm.width, shm.height);
                    let full = data.buffer.is_none();
                    let mut rects: Vec<Rect> = if full {
                        vec![Rect::new(0, 0, width, height)]
                    } else {
                        damage
                            .iter()
                            .map(|(rect, in_buffer)| {
                                if *in_buffer {
                                    *rect
                                } else {
                                    rect.scaled(scale)
                                }
                            })
                            .filter_map(|rect| rect.clip(width, height))
                            .collect()
                    };
                    if rects.len() > 16 {
                        // Many small rectangles: one bounding box is cheaper
                        // than many copies.
                        rects = vec![bounds(&rects)];
                    }
                    data.buffer = Some((buffer.clone(), shm.clone()));
                    event = Some(Event::Commit {
                        surface: key,
                        buffer: shm.clone(),
                        resource: buffer,
                        damage: rects,
                        geometry: state
                            .xdg
                            .get(&key)
                            .and_then(|info| info.geometry)
                            .map(|rect| rect.scaled(scale)),
                    });
                }
                None => {
                    // Not a wl_shm buffer (no other kind is advertised).
                    resource.post_error(
                        wl_surface::Error::InvalidSize,
                        "only wl_shm buffers are supported",
                    );
                }
            },
            Some(None) => {
                if data.buffer.take().is_some() {
                    event = Some(Event::Unmapped { surface: key });
                }
            }
            None => {}
        }
    } else if let Some(Some(buffer)) = attached {
        // No role yet (or not configured): nothing shows it, so give the
        // buffer straight back.
        buffer.release();
    }
    let children = data.children.clone();
    if let Some(event) = event {
        state.outbox.push(event);
    }
    // Subsurface positions take effect with the parent's commit.
    for child in children {
        let Some(child_data) = state.surface_by_key_mut(child) else {
            continue;
        };
        let Some((x, y)) = child_data.pending_position.take() else {
            continue;
        };
        if let Some(Placement::Subsurface { parent, .. }) = child_data.placement {
            let placement = Placement::Subsurface { parent, x, y };
            child_data.placement = Some(placement);
            let placement = state.placement_px(placement);
            state.outbox.push(Event::SurfaceMoved {
                surface: child,
                placement,
            });
        }
    }
}

fn bounds(rects: &[Rect]) -> Rect {
    let x0 = rects.iter().map(|rect| rect.x).min().unwrap_or(0);
    let y0 = rects.iter().map(|rect| rect.y).min().unwrap_or(0);
    let x1 = rects
        .iter()
        .map(|rect| rect.x + rect.width)
        .max()
        .unwrap_or(0);
    let y1 = rects
        .iter()
        .map(|rect| rect.y + rect.height)
        .max()
        .unwrap_or(0);
    Rect::new(x0, y0, x1 - x0, y1 - y0)
}

/// Fires the committed frame callbacks of a surface and its descendants.
pub(crate) fn frame_done(state: &mut WlState, key: SurfaceKey, time_ms: u32) {
    let mut queue = vec![key];
    let mut guard = 0;
    while let Some(key) = queue.pop() {
        guard += 1;
        if guard > 64 {
            break;
        }
        let Some(data) = state.surface_by_key_mut(key) else {
            continue;
        };
        for callback in data.frame_callbacks.drain(..) {
            callback.done(time_ms);
        }
        queue.extend(data.children.iter().copied());
    }
}

impl GlobalDispatch<WlSubcompositor, ()> for WlState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WlSubcompositor>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WlSubcompositor, ()> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &WlSubcompositor,
        request: wl_subcompositor::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let wl_subcompositor::Request::GetSubsurface {
            id,
            surface,
            parent,
        } = request
        {
            let (Some(key), Some(parent_key)) =
                (state.surface_key(&surface), state.surface_key(&parent))
            else {
                resource.post_error(wl_subcompositor::Error::BadSurface, "unknown surface");
                return;
            };
            if state
                .surface_by_key(key)
                .is_some_and(|data| data.placement.is_some())
            {
                resource.post_error(wl_subcompositor::Error::BadSurface, "surface has a role");
                return;
            }
            data_init.init(id, key);
            if let Some(data) = state.surface_by_key_mut(key) {
                data.can_show = true;
            }
            assign_role(
                state,
                key,
                Placement::Subsurface {
                    parent: parent_key,
                    x: 0,
                    y: 0,
                },
            );
        }
    }
}

impl Dispatch<WlSubsurface, SurfaceKey> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WlSubsurface,
        request: wl_subsurface::Request,
        key: &SurfaceKey,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_subsurface::Request::SetPosition { x, y } => {
                if let Some(data) = state.surface_by_key_mut(*key) {
                    data.pending_position = Some((x, y));
                }
            }
            wl_subsurface::Request::Destroy => drop_role(state, *key),
            // Stacking and sync mode: subsurfaces stack above their parent
            // in creation order and apply their content at their own commit.
            _ => {}
        }
    }
}
