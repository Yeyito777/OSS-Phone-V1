//! The clipboard: wl_data_device selection between Wayland apps.
//!
//! An app sets the selection with a data source; the app with keyboard
//! focus is offered it, and reading it passes the reader's pipe straight to
//! the source's owner. Moon never sees the contents. Drag and drop is not
//! offered on a phone.

use std::os::fd::AsFd as _;
use std::sync::Mutex;

use wayland_server::protocol::wl_data_device::{self, WlDataDevice};
use wayland_server::protocol::wl_data_device_manager::{self, WlDataDeviceManager};
use wayland_server::protocol::wl_data_offer::{self, WlDataOffer};
use wayland_server::protocol::wl_data_source::{self, WlDataSource};
use wayland_server::protocol::wl_surface::WlSurface;
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

use super::WlState;

#[derive(Default)]
pub(crate) struct Clipboard {
    devices: Vec<WlDataDevice>,
    selection: Option<WlDataSource>,
}

/// A source's MIME types, as the app offered them.
#[derive(Default)]
pub(crate) struct SourceData {
    mimes: Mutex<Vec<String>>,
}

pub(crate) fn create_globals(handle: &DisplayHandle) {
    handle.create_global::<WlState, WlDataDeviceManager, ()>(3, ());
}

/// Offers the selection to `devices` (all of one client), or tells them
/// there is none.
fn offer_selection(state: &WlState, handle: &DisplayHandle, devices: &[WlDataDevice]) {
    let source = state.clipboard.selection.as_ref().filter(|s| s.is_alive());
    for device in devices {
        let Some(source) = source else {
            device.selection(None);
            continue;
        };
        let Ok(client) = handle.get_client(device.id()) else {
            continue;
        };
        let Ok(offer) = client.create_resource::<WlDataOffer, WlDataSource, WlState>(
            handle,
            device.version(),
            source.clone(),
        ) else {
            continue;
        };
        device.data_offer(&offer);
        if let Some(data) = source.data::<SourceData>() {
            for mime in data.mimes.lock().unwrap_or_else(|p| p.into_inner()).iter() {
                offer.offer(mime.clone());
            }
        }
        device.selection(Some(&offer));
    }
}

/// Keyboard focus moved to `surface`: its client is offered the selection.
pub(crate) fn focus(state: &mut WlState, handle: &DisplayHandle, surface: &WlSurface) {
    let devices: Vec<WlDataDevice> = state
        .clipboard
        .devices
        .iter()
        .filter(|device| device.id().same_client_as(&surface.id()))
        .cloned()
        .collect();
    offer_selection(state, handle, &devices);
}

fn focused_devices(state: &WlState) -> Vec<WlDataDevice> {
    let Some(focus) = state.seat.focus.as_ref() else {
        return Vec::new();
    };
    state
        .clipboard
        .devices
        .iter()
        .filter(|device| device.id().same_client_as(&focus.id()))
        .cloned()
        .collect()
}

impl GlobalDispatch<WlDataDeviceManager, ()> for WlState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WlDataDeviceManager>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<WlDataDeviceManager, ()> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WlDataDeviceManager,
        request: wl_data_device_manager::Request,
        _data: &(),
        handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_data_device_manager::Request::CreateDataSource { id } => {
                data_init.init(id, SourceData::default());
            }
            wl_data_device_manager::Request::GetDataDevice { id, .. } => {
                let device = data_init.init(id, ());
                state.clipboard.devices.push(device.clone());
                let focused = state
                    .seat
                    .focus
                    .as_ref()
                    .is_some_and(|focus| focus.id().same_client_as(&device.id()));
                if focused {
                    offer_selection(state, handle, &[device]);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataSource, SourceData> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &WlDataSource,
        request: wl_data_source::Request,
        data: &SourceData,
        handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_data_source::Request::Offer { mime_type } => {
                data.mimes
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .push(mime_type);
            }
            wl_data_source::Request::Destroy => {
                if state.clipboard.selection.as_ref() == Some(resource) {
                    state.clipboard.selection = None;
                    let devices = focused_devices(state);
                    offer_selection(state, handle, &devices);
                }
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &WlDataSource,
        _data: &SourceData,
    ) {
        // The owner went away (or destroyed it): the selection is gone. The
        // focused app hears of it at its next focus change.
        if state.clipboard.selection.as_ref() == Some(resource) {
            state.clipboard.selection = None;
        }
    }
}

impl Dispatch<WlDataDevice, ()> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &WlDataDevice,
        request: wl_data_device::Request,
        _data: &(),
        handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            wl_data_device::Request::SetSelection { source, .. } => {
                if let Some(old) = state.clipboard.selection.take()
                    && Some(&old) != source.as_ref()
                    && old.is_alive()
                {
                    old.cancelled();
                }
                state.clipboard.selection = source;
                let devices = focused_devices(state);
                offer_selection(state, handle, &devices);
            }
            wl_data_device::Request::StartDrag { source, .. } => {
                // No drag and drop on a phone: the drag ends at once.
                if let Some(source) = source {
                    source.cancelled();
                }
            }
            wl_data_device::Request::Release => {
                state.clipboard.devices.retain(|device| device != resource);
            }
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: wayland_server::backend::ClientId,
        resource: &WlDataDevice,
        _data: &(),
    ) {
        state.clipboard.devices.retain(|device| device != resource);
    }
}

impl Dispatch<WlDataOffer, WlDataSource> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WlDataOffer,
        request: wl_data_offer::Request,
        source: &WlDataSource,
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let wl_data_offer::Request::Receive { mime_type, fd } = request
            && source.is_alive()
        {
            // The reader's pipe goes to the source's owner; our copy of
            // the fd closes when `fd` drops.
            source.send(mime_type, fd.as_fd());
        }
    }
}
