//! text-input-v3: the app says when a text field has focus and what it
//! holds (a password, a URL, a number); Moon shows its keyboard for it and
//! commits each typed character as text. No text is kept here: every
//! character goes straight out to the client, as the XTEST path does.

use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_manager_v3::{
    self, ZwpTextInputManagerV3,
};
use wayland_protocols::wp::text_input::zv3::server::zwp_text_input_v3::{
    self, ContentPurpose, ZwpTextInputV3,
};
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

use super::{Event, WlState};
use crate::keyboard::{KeyboardInput, KeyboardPurpose};

#[derive(Default)]
pub(crate) struct TextInputs {
    inputs: Vec<Input>,
}

struct Input {
    resource: ZwpTextInputV3,
    /// The surface this input has entered (the focused one), if any.
    entered: Option<WlSurface>,
    pending_enabled: Option<bool>,
    pending_purpose: Option<ContentPurpose>,
    enabled: bool,
    purpose: ContentPurpose,
    commits: u32,
}

impl TextInputs {
    pub(crate) fn forget_surface(&mut self, surface: &WlSurface) {
        for input in &mut self.inputs {
            if input.entered.as_ref() == Some(surface) {
                input.entered = None;
                input.enabled = false;
            }
        }
    }
}

pub(crate) fn create_globals(handle: &DisplayHandle) {
    handle.create_global::<WlState, ZwpTextInputManagerV3, ()>(1, ());
}

fn purpose(value: ContentPurpose) -> KeyboardPurpose {
    match value {
        ContentPurpose::Digits
        | ContentPurpose::Number
        | ContentPurpose::Phone
        | ContentPurpose::Pin => KeyboardPurpose::Number,
        ContentPurpose::Url => KeyboardPurpose::Url,
        ContentPurpose::Password => KeyboardPurpose::Password,
        _ => KeyboardPurpose::Text,
    }
}

impl GlobalDispatch<ZwpTextInputManagerV3, ()> for WlState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwpTextInputManagerV3>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &ZwpTextInputManagerV3,
        request: zwp_text_input_manager_v3::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwp_text_input_manager_v3::Request::GetTextInput { id, .. } = request {
            let resource = data_init.init(id, ());
            let entered = state
                .seat
                .focus
                .clone()
                .filter(|focus| focus.id().same_client_as(&resource.id()));
            if let Some(surface) = &entered {
                resource.enter(surface);
            }
            state.text_input.inputs.push(Input {
                resource,
                entered,
                pending_enabled: None,
                pending_purpose: None,
                enabled: false,
                purpose: ContentPurpose::Normal,
                commits: 0,
            });
        }
    }
}

impl Dispatch<ZwpTextInputV3, ()> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwpTextInputV3,
        request: zwp_text_input_v3::Request,
        _data: &(),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let Some(index) = state
            .text_input
            .inputs
            .iter()
            .position(|input| &input.resource == resource)
        else {
            return;
        };
        let input = &mut state.text_input.inputs[index];
        match request {
            zwp_text_input_v3::Request::Enable => {
                input.pending_enabled = Some(true);
                // Enable resets the state to its defaults.
                input.pending_purpose = Some(ContentPurpose::Normal);
            }
            zwp_text_input_v3::Request::Disable => input.pending_enabled = Some(false),
            zwp_text_input_v3::Request::SetContentType { purpose, .. } => {
                if let Ok(purpose) = purpose.into_result() {
                    input.pending_purpose = Some(purpose);
                }
            }
            zwp_text_input_v3::Request::Commit => {
                input.commits = input.commits.wrapping_add(1);
                let was_enabled = input.enabled;
                let old_purpose = input.purpose;
                if let Some(enabled) = input.pending_enabled.take() {
                    input.enabled = enabled;
                }
                if let Some(purpose) = input.pending_purpose.take() {
                    input.purpose = purpose;
                }
                // Only the focused surface's input drives the keyboard.
                let entered = input.entered.clone();
                let changed =
                    was_enabled != input.enabled || (input.enabled && old_purpose != input.purpose);
                let (enabled, content) = (input.enabled, input.purpose);
                if let Some(surface) = entered
                    && changed
                {
                    let toplevel = state
                        .surface_key(&surface)
                        .and_then(|key| state.toplevel_of(key));
                    state.outbox.push(Event::TextInput {
                        toplevel,
                        enabled,
                        purpose: purpose(content),
                    });
                }
            }
            // Surrounding text and the cursor rectangle would matter for
            // autocorrect and keyboard placement; Moon has neither yet.
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &ZwpTextInputV3,
        _data: &(),
    ) {
        if let Some(index) = state
            .text_input
            .inputs
            .iter()
            .position(|input| &input.resource == resource)
        {
            let input = state.text_input.inputs.remove(index);
            if input.enabled && input.entered.is_some() {
                state.outbox.push(Event::TextInput {
                    toplevel: None,
                    enabled: false,
                    purpose: KeyboardPurpose::Text,
                });
            }
        }
    }
}

pub(crate) fn enter(state: &mut WlState, surface: &WlSurface) {
    for input in &mut state.text_input.inputs {
        if input.resource.id().same_client_as(&surface.id()) {
            input.resource.enter(surface);
            input.entered = Some(surface.clone());
        }
    }
}

pub(crate) fn leave(state: &mut WlState, surface: &WlSurface) {
    let mut was_enabled = false;
    for input in &mut state.text_input.inputs {
        if input.entered.as_ref() == Some(surface) {
            input.resource.leave(surface);
            input.entered = None;
            was_enabled |= input.enabled;
            // The client must enable again after the next enter.
            input.enabled = false;
        }
    }
    if was_enabled {
        state.outbox.push(Event::TextInput {
            toplevel: None,
            enabled: false,
            purpose: KeyboardPurpose::Text,
        });
    }
}

/// Commits one key as text to the focused, enabled text input. Enter is
/// left to wl_keyboard (text-input-v3 has no "submit").
pub(crate) fn deliver(state: &mut WlState, key: KeyboardInput) -> bool {
    let Some(input) = state
        .text_input
        .inputs
        .iter()
        .find(|input| input.enabled && input.entered.is_some())
    else {
        return false;
    };
    match key {
        KeyboardInput::Character(character) => {
            input.resource.commit_string(Some(character.to_string()));
        }
        KeyboardInput::Backspace => input.resource.delete_surrounding_text(1, 0),
        KeyboardInput::Enter => return false,
    }
    input.resource.done(input.commits);
    true
}
