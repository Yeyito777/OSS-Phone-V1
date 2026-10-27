//! xdg-shell (toplevels and popups) and xdg-decoration.
//!
//! Every toplevel is full-screen in Moon's app area: Moon sends the size it
//! placed the app at, with the fullscreen state (so clients draw no
//! decorations), activated while the app has focus, and suspended (v6)
//! while it is in the background or the screen is off.

use wayland_protocols::xdg::decoration::zv1::server::zxdg_decoration_manager_v1::{
    self, ZxdgDecorationManagerV1,
};
use wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::{
    self, ZxdgToplevelDecorationV1,
};
use wayland_protocols::xdg::shell::server::xdg_popup::{self, XdgPopup};
use wayland_protocols::xdg::shell::server::xdg_positioner::{self, XdgPositioner};
use wayland_protocols::xdg::shell::server::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::server::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::server::xdg_wm_base::{self, XdgWmBase};
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

use super::surface::{assign_role, drop_role};
use super::{ClientInfo, Event, Placement, SurfaceKey, ToplevelId, WlState};

pub(crate) struct Toplevel {
    pub key: SurfaceKey,
    pub xdg_surface: XdgSurface,
    pub xdg_toplevel: XdgToplevel,
    /// What Moon last asked for: (width, height, activated, suspended).
    pub sent: Option<(i32, i32, bool, bool)>,
    /// Moon's wish, kept until the client's first commit asks for it.
    pub wanted: Option<(i32, i32, bool, bool)>,
    /// Xwayland's root: sized in pixels (scale 1), and not one of Moon's
    /// app windows.
    pub xwayland: bool,
}

/// xdg_surface state per wl_surface.
#[derive(Default)]
pub(crate) struct XdgInfo {
    pub initial_sent: bool,
    pub last_serial: u32,
    pub popup: Option<(XdgSurface, XdgPopup)>,
    /// xdg window geometry: the part of the surface that is the window
    /// (without client-side shadows).
    pub geometry: Option<super::Rect>,
}

#[derive(Default, Clone, Copy)]
pub(crate) struct Positioner {
    size: (i32, i32),
    anchor_rect: (i32, i32, i32, i32),
    anchor: u32,
    gravity: u32,
    offset: (i32, i32),
}

impl Positioner {
    /// The popup's top-left corner relative to the parent's geometry. No
    /// constraint adjustment: Moon's apps are full-screen and popups that
    /// overflow are clipped by their parent.
    fn position(&self) -> (i32, i32) {
        let (ax, ay, aw, ah) = self.anchor_rect;
        // xdg_positioner.anchor / gravity: none, top, bottom, left, right,
        // top_left, bottom_left, top_right, bottom_right.
        let (px, py) = match self.anchor {
            1 => (ax + aw / 2, ay),
            2 => (ax + aw / 2, ay + ah),
            3 => (ax, ay + ah / 2),
            4 => (ax + aw, ay + ah / 2),
            5 => (ax, ay),
            6 => (ax, ay + ah),
            7 => (ax + aw, ay),
            8 => (ax + aw, ay + ah),
            _ => (ax + aw / 2, ay + ah / 2),
        };
        let (w, h) = self.size;
        let (gx, gy) = match self.gravity {
            1 => (-w / 2, -h),
            2 => (-w / 2, 0),
            3 => (-w, -h / 2),
            4 => (0, -h / 2),
            5 => (-w, -h),
            6 => (-w, 0),
            7 => (0, -h),
            8 => (0, 0),
            _ => (-w / 2, -h / 2),
        };
        (px + gx + self.offset.0, py + gy + self.offset.1)
    }
}

pub(crate) fn create_globals(handle: &DisplayHandle) {
    handle.create_global::<WlState, XdgWmBase, ()>(6, ());
    handle.create_global::<WlState, ZxdgDecorationManagerV1, ()>(1, ());
}

impl GlobalDispatch<XdgWmBase, ()> for WlState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<XdgWmBase>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<XdgWmBase, ()> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &XdgWmBase,
        request: xdg_wm_base::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_wm_base::Request::CreatePositioner { id } => {
                data_init.init(id, std::sync::Mutex::new(Positioner::default()));
            }
            xdg_wm_base::Request::GetXdgSurface { id, surface } => {
                let Some(key) = state.surface_key(&surface) else {
                    resource.post_error(xdg_wm_base::Error::InvalidSurfaceState, "unknown surface");
                    return;
                };
                if state
                    .surface_by_key(key)
                    .is_some_and(|data| data.placement.is_some() || data.buffer.is_some())
                {
                    resource.post_error(xdg_wm_base::Error::Role, "surface has a role or buffer");
                    return;
                }
                data_init.init(id, key);
                state.xdg.insert(key, XdgInfo::default());
            }
            xdg_wm_base::Request::Pong { .. } | xdg_wm_base::Request::Destroy => {}
            _ => {}
        }
    }
}

impl Dispatch<XdgPositioner, std::sync::Mutex<Positioner>> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &XdgPositioner,
        request: xdg_positioner::Request,
        data: &std::sync::Mutex<Positioner>,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let mut positioner = data.lock().unwrap_or_else(|poison| poison.into_inner());
        match request {
            xdg_positioner::Request::SetSize { width, height } => positioner.size = (width, height),
            xdg_positioner::Request::SetAnchorRect {
                x,
                y,
                width,
                height,
            } => positioner.anchor_rect = (x, y, width, height),
            xdg_positioner::Request::SetAnchor { anchor } => {
                positioner.anchor = u32::from(anchor);
            }
            xdg_positioner::Request::SetGravity { gravity } => {
                positioner.gravity = u32::from(gravity);
            }
            xdg_positioner::Request::SetOffset { x, y } => positioner.offset = (x, y),
            _ => {}
        }
    }
}

impl Dispatch<XdgSurface, SurfaceKey> for WlState {
    fn request(
        state: &mut Self,
        client: &Client,
        resource: &XdgSurface,
        request: xdg_surface::Request,
        key: &SurfaceKey,
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let key = *key;
        match request {
            xdg_surface::Request::GetToplevel { id } => {
                let toplevel = data_init.init(id, key);
                state.next_toplevel = state.next_toplevel.wrapping_add(1).max(1);
                let id = ToplevelId(state.next_toplevel);
                let pid = client.get_data::<ClientInfo>().and_then(|info| info.pid);
                let xwayland = super::is_xwayland(client);
                state.toplevels.insert(
                    id,
                    Toplevel {
                        key,
                        xdg_surface: resource.clone(),
                        xdg_toplevel: toplevel,
                        sent: None,
                        wanted: None,
                        xwayland,
                    },
                );
                assign_role(state, key, Placement::Toplevel(id));
                if xwayland {
                    state.xwayland_root = Some(key);
                    if let Some(root) = state.surface_by_key(key).map(|data| data.resource.clone()) {
                        super::seat::xwayland_enter(state, &root);
                    }
                } else {
                    state.outbox.push(Event::ToplevelCreated {
                        id,
                        surface: key,
                        pid,
                    });
                }
            }
            xdg_surface::Request::GetPopup {
                id,
                parent,
                positioner,
            } => {
                let popup = data_init.init(id, key);
                let place = positioner
                    .data::<std::sync::Mutex<Positioner>>()
                    .map(|data| *data.lock().unwrap_or_else(|poison| poison.into_inner()))
                    .unwrap_or_default();
                let parent_key = parent
                    .as_ref()
                    .and_then(|parent| parent.data::<SurfaceKey>());
                let Some(parent_key) = parent_key.copied() else {
                    resource.post_error(xdg_surface::Error::NotConstructed, "popups need a parent");
                    return;
                };
                let (x, y) = place.position();
                let (w, h) = place.size;
                if let Some(info) = state.xdg.get_mut(&key) {
                    info.popup = Some((resource.clone(), popup.clone()));
                }
                popup.configure(x, y, w, h);
                assign_role(
                    state,
                    key,
                    Placement::Popup {
                        parent: parent_key,
                        x,
                        y,
                    },
                );
            }
            xdg_surface::Request::AckConfigure { serial } => {
                let acked = state
                    .xdg
                    .get(&key)
                    .is_some_and(|info| info.initial_sent && serial <= info.last_serial);
                if acked && let Some(data) = state.surface_by_key_mut(key) {
                    data.can_show = true;
                }
            }
            xdg_surface::Request::SetWindowGeometry {
                x,
                y,
                width,
                height,
            } => {
                if let Some(info) = state.xdg.get_mut(&key)
                    && width > 0
                    && height > 0
                {
                    info.geometry = Some(super::Rect::new(x, y, width, height));
                }
            }
            xdg_surface::Request::Destroy => {
                state.xdg.remove(&key);
                drop_role(state, key);
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgToplevel, SurfaceKey> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &XdgToplevel,
        request: xdg_toplevel::Request,
        key: &SurfaceKey,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let toplevel = state.toplevel_of(*key);
        match request {
            xdg_toplevel::Request::SetTitle { title } => {
                if let Some(id) = toplevel {
                    state.outbox.push(Event::ToplevelTitle { id, title });
                }
            }
            xdg_toplevel::Request::Destroy => drop_role(state, *key),
            // Moon owns placement: move, resize, maximize, fullscreen and
            // minimize requests have nothing to change on a phone.
            _ => {}
        }
    }
}

impl Dispatch<XdgPopup, SurfaceKey> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &XdgPopup,
        request: xdg_popup::Request,
        key: &SurfaceKey,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            xdg_popup::Request::Destroy => {
                if let Some(info) = state.xdg.get_mut(key) {
                    info.popup = None;
                }
                drop_role(state, *key);
            }
            xdg_popup::Request::Grab { .. } => {}
            xdg_popup::Request::Reposition { positioner, token } => {
                let place = positioner
                    .data::<std::sync::Mutex<Positioner>>()
                    .map(|data| *data.lock().unwrap_or_else(|poison| poison.into_inner()))
                    .unwrap_or_default();
                let (x, y) = place.position();
                let (w, h) = place.size;
                if resource.version() >= 3 {
                    resource.repositioned(token);
                }
                resource.configure(x, y, w, h);
                let serial = state.next_serial();
                if let Some(info) = state.xdg.get_mut(key) {
                    info.last_serial = serial;
                    if let Some((surface, _)) = &info.popup {
                        surface.configure(serial);
                    }
                }
                if let Some(Placement::Popup { parent, .. }) =
                    state.surface_by_key(*key).and_then(|data| data.placement)
                {
                    let placement = Placement::Popup { parent, x, y };
                    if let Some(data) = state.surface_by_key_mut(*key) {
                        data.placement = Some(placement);
                    }
                    let placement = state.placement_px(placement);
                    state.outbox.push(Event::SurfaceMoved {
                        surface: *key,
                        placement,
                    });
                }
            }
            _ => {}
        }
    }
}

/// The first commit of an xdg_surface without a configure gets one.
pub(crate) fn on_commit(state: &mut WlState, surface: &WlSurface) {
    let Some(key) = state.surface_key(surface) else {
        return;
    };
    let Some(info) = state.xdg.get(&key) else {
        return;
    };
    if info.initial_sent {
        return;
    }
    let toplevel = state
        .toplevels
        .iter()
        .find(|(_, toplevel)| toplevel.key == key)
        .map(|(id, toplevel)| (*id, toplevel.wanted));
    let popup = info.popup.clone();
    // Xwayland's root is never configured, so Xwayland never maps it and
    // never copies the root into buffers for it (two, alternating, a CPU
    // copy of every damaged frame): Moon shows the X root through the
    // mirror. X input and rendering do not need the root mapped.
    if let Some((id, _)) = toplevel
        && state.toplevels.get(&id).is_some_and(|t| t.xwayland)
    {
        return;
    }
    if let Some((id, wanted)) = toplevel {
        let (width, height, activated, suspended) =
            wanted.unwrap_or((state.toplevel_size.0, state.toplevel_size.1, false, false));
        if let Some(info) = state.xdg.get_mut(&key) {
            info.initial_sent = true;
        }
        configure(state, id, width, height, activated, suspended);
    } else if let Some((xdg_surface, _)) = popup {
        let serial = state.next_serial();
        if let Some(info) = state.xdg.get_mut(&key) {
            info.initial_sent = true;
            info.last_serial = serial;
        }
        xdg_surface.configure(serial);
    }
}

pub(crate) fn configure(
    state: &mut WlState,
    id: ToplevelId,
    width: i32,
    height: i32,
    activated: bool,
    suspended: bool,
) {
    let wish = (width, height, activated, suspended);
    let Some(toplevel) = state.toplevels.get_mut(&id) else {
        return;
    };
    toplevel.wanted = Some(wish);
    let key = toplevel.key;
    // Before the client's first commit there is nothing to configure yet;
    // that commit asks for `wanted`.
    if !state.xdg.get(&key).is_some_and(|info| info.initial_sent) {
        return;
    }
    let toplevel = state.toplevels.get_mut(&id).expect("checked above");
    if toplevel.sent == Some(wish) {
        return;
    }
    toplevel.sent = Some(wish);
    let mut states: Vec<u8> = Vec::new();
    let mut push =
        |value: xdg_toplevel::State| states.extend_from_slice(&(value as u32).to_ne_bytes());
    push(xdg_toplevel::State::Fullscreen);
    if activated {
        push(xdg_toplevel::State::Activated);
    }
    if suspended && toplevel.xdg_toplevel.version() >= 6 {
        push(xdg_toplevel::State::Suspended);
    }
    // Moon places windows in pixels; clients are told surface units.
    let scale = if toplevel.xwayland { 1 } else { state.scale };
    toplevel.xdg_toplevel.configure(
        (width + scale - 1) / scale,
        (height + scale - 1) / scale,
        states,
    );
    let xdg_surface = toplevel.xdg_surface.clone();
    let serial = state.next_serial();
    if let Some(info) = state.xdg.get_mut(&key) {
        info.last_serial = serial;
    }
    xdg_surface.configure(serial);
}

impl GlobalDispatch<ZxdgDecorationManagerV1, ()> for WlState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZxdgDecorationManagerV1>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZxdgDecorationManagerV1, ()> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZxdgDecorationManagerV1,
        request: zxdg_decoration_manager_v1::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let zxdg_decoration_manager_v1::Request::GetToplevelDecoration { id, .. } = request {
            let decoration = data_init.init(id, ());
            // Phone apps are full-screen: nobody draws decorations.
            decoration.configure(zxdg_toplevel_decoration_v1::Mode::ServerSide);
        }
    }
}

impl Dispatch<ZxdgToplevelDecorationV1, ()> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        resource: &ZxdgToplevelDecorationV1,
        request: zxdg_toplevel_decoration_v1::Request,
        _data: &(),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            zxdg_toplevel_decoration_v1::Request::SetMode { .. }
            | zxdg_toplevel_decoration_v1::Request::UnsetMode => {
                resource.configure(zxdg_toplevel_decoration_v1::Mode::ServerSide);
            }
            _ => {}
        }
    }
}
