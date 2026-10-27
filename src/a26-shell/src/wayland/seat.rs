//! wl_seat (touch and keyboard), wl_output, and a minimal data device.
//!
//! Touches are the ones Moon routes to an app; keys come from Moon's
//! on-screen keyboard through a fixed US keymap (`assets/xkb/us.xkb`).
//! Moon repeats held keys itself, so clients are told not to.

use std::collections::HashMap;
use std::ffi::CString;
use std::io::Write as _;
use std::os::fd::{AsFd, FromRawFd, OwnedFd};

use wayland_server::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_server::protocol::wl_output::{self, WlOutput};
use wayland_server::protocol::wl_pointer::WlPointer;
use wayland_server::protocol::wl_seat::{self, WlSeat};
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::protocol::wl_touch::WlTouch;
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

use super::{SurfaceKey, ToplevelId, WlState};
use crate::keyboard::KeyboardInput;

const KEYMAP: &str = include_str!("../../assets/xkb/us.xkb");

pub(crate) struct Seat {
    touches: Vec<WlTouch>,
    keyboards: Vec<WlKeyboard>,
    keymap: Option<(OwnedFd, u32)>,
    /// Each touch's surface and that surface's scale (Moon sends pixels).
    touch_points: HashMap<i32, (WlSurface, f64)>,
    pub(crate) focus: Option<WlSurface>,
}

impl Seat {
    pub(crate) fn new() -> Self {
        let keymap = match keymap_fd() {
            Ok(keymap) => Some(keymap),
            Err(error) => {
                eprintln!("wayland: no keymap for wl_keyboard: {error}");
                None
            }
        };
        Self {
            touches: Vec::new(),
            keyboards: Vec::new(),
            keymap,
            touch_points: HashMap::new(),
            focus: None,
        }
    }

    pub(crate) fn forget_surface(&mut self, key: SurfaceKey) {
        let _ = key;
        self.touch_points
            .retain(|_, (surface, _)| surface.is_alive());
        if self.focus.as_ref().is_some_and(|focus| !focus.is_alive()) {
            self.focus = None;
        }
    }
}

fn keymap_fd() -> std::io::Result<(OwnedFd, u32)> {
    let name = CString::new("moon-keymap").expect("no NUL");
    // SAFETY: memfd_create with a valid name; the fd is owned below.
    let raw = unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) };
    if raw < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: raw is a fresh descriptor that nothing else owns.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut file = std::fs::File::from(fd);
    file.write_all(KEYMAP.as_bytes())?;
    file.write_all(&[0])?;
    Ok((OwnedFd::from(file), KEYMAP.len() as u32 + 1))
}

pub(crate) fn create_globals(handle: &DisplayHandle) {
    handle.create_global::<WlState, WlSeat, ()>(7, ());
    handle.create_global::<WlState, WlOutput, ()>(4, ());
}

impl GlobalDispatch<WlSeat, ()> for WlState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WlSeat>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let seat = data_init.init(resource, ());
        if seat.version() >= 2 {
            seat.name("seat0".into());
        }
        seat.capabilities(wl_seat::Capability::Touch | wl_seat::Capability::Keyboard);
    }
}

impl Dispatch<WlSeat, ()> for WlState {
    fn request(
        state: &mut Self,
        client: &Client,
        _resource: &WlSeat,
        request: wl_seat::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_seat::Request::GetTouch { id } => {
                let touch = data_init.init(id, ());
                state.seat.touches.push(touch);
            }
            wl_seat::Request::GetKeyboard { id } => {
                let keyboard = data_init.init(id, ());
                if let Some((fd, size)) = &state.seat.keymap {
                    keyboard.keymap(wl_keyboard::KeymapFormat::XkbV1, fd.as_fd(), *size);
                }
                if keyboard.version() >= 4 {
                    keyboard.repeat_info(0, 0);
                }
                // A keyboard bound while its client is focused enters at once;
                // Xwayland's is always on its root (see `xwayland_enter`).
                let focus = if super::is_xwayland(client) {
                    state
                        .xwayland_root
                        .and_then(|key| state.surface_by_key(key))
                        .map(|data| data.resource.clone())
                } else {
                    state.seat.focus.clone()
                };
                if let Some(focus) = focus
                    && focus.id().same_client_as(&keyboard.id())
                {
                    let serial = state.next_serial();
                    keyboard.enter(serial, &focus, Vec::new());
                    keyboard.modifiers(serial, 0, 0, 0, 0);
                }
                state.seat.keyboards.push(keyboard);
            }
            wl_seat::Request::GetPointer { id } => {
                // No pointer capability is advertised; a client that asks
                // anyway gets an object that never sends events.
                data_init.init(id, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<WlTouch, ()> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlTouch,
        _request: <WlTouch as Resource>::Request,
        _data: &(),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &WlTouch,
        _data: &(),
    ) {
        state.seat.touches.retain(|touch| touch != resource);
    }
}

impl Dispatch<WlKeyboard, ()> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlKeyboard,
        _request: <WlKeyboard as Resource>::Request,
        _data: &(),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &WlKeyboard,
        _data: &(),
    ) {
        state.seat.keyboards.retain(|keyboard| keyboard != resource);
    }
}

impl Dispatch<WlPointer, ()> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlPointer,
        _request: <WlPointer as Resource>::Request,
        _data: &(),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}

/// Xwayland's keyboard focus is its root, for good: X decides which X
/// window gets the keys (Moon sets X focus as the window manager), and the
/// keys themselves come through XTEST. Xwayland delivers no key events
/// while it has no Wayland keyboard focus.
pub(crate) fn xwayland_enter(state: &mut WlState, root: &WlSurface) {
    let serial = state.next_serial();
    for keyboard in &state.seat.keyboards {
        if keyboard.id().same_client_as(&root.id()) {
            keyboard.enter(serial, root, Vec::new());
            keyboard.modifiers(serial, 0, 0, 0, 0);
        }
    }
}

pub(crate) fn set_keyboard_focus(
    state: &mut WlState,
    _handle: &DisplayHandle,
    toplevel: Option<ToplevelId>,
) {
    let surface = toplevel
        .and_then(|id| state.toplevels.get(&id))
        .and_then(|toplevel| state.surface_by_key(toplevel.key))
        .map(|data| data.resource.clone());
    if surface == state.seat.focus {
        return;
    }
    if let Some(old) = state.seat.focus.take()
        && old.is_alive()
    {
        let serial = state.next_serial();
        for keyboard in &state.seat.keyboards {
            if keyboard.id().same_client_as(&old.id()) {
                keyboard.leave(serial, &old);
            }
        }
        super::text_input::leave(state, &old);
    }
    if let Some(new) = surface {
        let serial = state.next_serial();
        for keyboard in &state.seat.keyboards {
            if keyboard.id().same_client_as(&new.id()) {
                keyboard.enter(serial, &new, Vec::new());
                keyboard.modifiers(serial, 0, 0, 0, 0);
            }
        }
        super::text_input::enter(state, &new);
        state.seat.focus = Some(new.clone());
        super::clipboard::focus(state, _handle, &new);
    }
}

pub(crate) fn touch_down(state: &mut WlState, key: SurfaceKey, id: i32, x: f64, y: f64, time: u32) {
    let Some((surface, scale)) = state
        .surface_by_key(key)
        .map(|data| (data.resource.clone(), f64::from(data.scale.max(1))))
    else {
        return;
    };
    let serial = state.next_serial();
    for touch in &state.seat.touches {
        if touch.id().same_client_as(&surface.id()) {
            touch.down(serial, time, &surface, id, x / scale, y / scale);
            touch.frame();
        }
    }
    state.seat.touch_points.insert(id, (surface, scale));
}

pub(crate) fn touch_motion(state: &mut WlState, id: i32, x: f64, y: f64, time: u32) {
    let Some((surface, scale)) = state.seat.touch_points.get(&id) else {
        return;
    };
    for touch in &state.seat.touches {
        if touch.id().same_client_as(&surface.id()) {
            touch.motion(time, id, x / scale, y / scale);
            touch.frame();
        }
    }
}

pub(crate) fn touch_up(state: &mut WlState, id: i32, time: u32) {
    let Some((surface, _)) = state.seat.touch_points.remove(&id) else {
        return;
    };
    let serial = state.next_serial();
    for touch in &state.seat.touches {
        if touch.id().same_client_as(&surface.id()) {
            touch.up(serial, time, id);
            touch.frame();
        }
    }
}

/// A gesture of Moon's took the touch over (the home swipe): the client
/// forgets every touch point, as the protocol's cancel says.
pub(crate) fn touch_cancel(state: &mut WlState) {
    let surfaces: Vec<WlSurface> = state
        .seat
        .touch_points
        .drain()
        .map(|(_, (surface, _))| surface)
        .collect();
    for touch in &state.seat.touches {
        if surfaces
            .iter()
            .any(|surface| touch.id().same_client_as(&surface.id()))
        {
            touch.cancel();
        }
    }
}

/// Linux input key codes (evdev) for the US layout, and whether Shift is
/// needed.
fn us_key(character: char) -> Option<(u32, bool)> {
    const ROW_NUMBERS: &str = "1234567890";
    const SHIFTED_NUMBERS: &str = "!@#$%^&*()";
    const ROW_Q: &str = "qwertyuiop";
    const ROW_A: &str = "asdfghjkl";
    const ROW_Z: &str = "zxcvbnm";
    let lower = character.to_ascii_lowercase();
    let shifted = character.is_ascii_uppercase();
    let index = |row: &str, c: char| row.chars().position(|r| r == c).map(|i| i as u32);
    if let Some(i) = index(ROW_NUMBERS, character) {
        return Some((2 + i, false));
    }
    if let Some(i) = index(SHIFTED_NUMBERS, character) {
        return Some((2 + i, true));
    }
    if let Some(i) = index(ROW_Q, lower) {
        return Some((16 + i, shifted));
    }
    if let Some(i) = index(ROW_A, lower) {
        return Some((30 + i, shifted));
    }
    if let Some(i) = index(ROW_Z, lower) {
        return Some((44 + i, shifted));
    }
    Some(match character {
        ' ' => (57, false),
        '-' => (12, false),
        '_' => (12, true),
        '=' => (13, false),
        '+' => (13, true),
        '[' => (26, false),
        '{' => (26, true),
        ']' => (27, false),
        '}' => (27, true),
        ';' => (39, false),
        ':' => (39, true),
        '\'' => (40, false),
        '"' => (40, true),
        '`' => (41, false),
        '~' => (41, true),
        '\\' => (43, false),
        '|' => (43, true),
        ',' => (51, false),
        '<' => (51, true),
        '.' => (52, false),
        '>' => (52, true),
        '/' => (53, false),
        '?' => (53, true),
        _ => return None,
    })
}

const KEY_BACKSPACE: u32 = 14;
const KEY_ENTER: u32 = 28;
/// The Shift modifier's bit in the keymap's modifier mask.
const SHIFT_MASK: u32 = 1;

pub(crate) fn deliver_key(state: &mut WlState, input: KeyboardInput, time: u32) -> bool {
    let Some(focus) = state.seat.focus.clone() else {
        return false;
    };
    let (code, shift) = match input {
        KeyboardInput::Character(character) => match us_key(character) {
            Some(key) => key,
            None => return false,
        },
        KeyboardInput::Backspace => (KEY_BACKSPACE, false),
        KeyboardInput::Enter => (KEY_ENTER, false),
    };
    let keyboards: Vec<WlKeyboard> = state
        .seat
        .keyboards
        .iter()
        .filter(|keyboard| keyboard.id().same_client_as(&focus.id()))
        .cloned()
        .collect();
    if keyboards.is_empty() {
        return false;
    }
    for keyboard in &keyboards {
        if shift {
            let serial = state.next_serial();
            keyboard.modifiers(serial, SHIFT_MASK, 0, 0, 0);
        }
        let serial = state.next_serial();
        keyboard.key(serial, time, code, wl_keyboard::KeyState::Pressed);
        let serial = state.next_serial();
        keyboard.key(serial, time, code, wl_keyboard::KeyState::Released);
        if shift {
            let serial = state.next_serial();
            keyboard.modifiers(serial, 0, 0, 0, 0);
        }
    }
    true
}

impl GlobalDispatch<WlOutput, ()> for WlState {
    fn bind(
        state: &mut Self,
        _handle: &DisplayHandle,
        client: &Client,
        resource: New<WlOutput>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let output = data_init.init(resource, ());
        state.outputs.push(output.clone());
        output.geometry(
            0,
            0,
            71,
            154,
            wl_output::Subpixel::Unknown,
            "Samsung".into(),
            "Galaxy A26".into(),
            wl_output::Transform::Normal,
        );
        output.mode(
            wl_output::Mode::Current | wl_output::Mode::Preferred,
            state.output_size.0,
            state.output_size.1,
            state.refresh_mhz,
        );
        if output.version() >= 2 {
            output.scale(if super::is_xwayland(client) {
                1
            } else {
                state.scale
            });
        }
        if output.version() >= 4 {
            output.name("DSI-1".into());
            output.description("Galaxy A26 panel".into());
        }
        if output.version() >= 2 {
            output.done();
        }
    }
}

impl Dispatch<WlOutput, ()> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlOutput,
        _request: wl_output::Request,
        _data: &(),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &WlOutput,
        _data: &(),
    ) {
        state.outputs.retain(|output| output != resource);
    }
}
