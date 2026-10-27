//! wp_presentation: when a commit's content actually reached the panel.
//!
//! A feedback request belongs to the content committed with it. Moon tells
//! the server when a buffer became the one on screen (the flip that showed
//! it in GPU direct mode, the completed copy into the X proxy otherwise);
//! every feedback waiting on that buffer is then answered `presented`.
//! Feedback for a commit without new content, or for a surface that goes
//! away, is answered `discarded`.

use std::collections::HashMap;

use wayland_protocols::wp::presentation_time::server::wp_presentation::{self, WpPresentation};
use wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::{
    Kind, WpPresentationFeedback,
};
use wayland_server::backend::ObjectId;
use wayland_server::protocol::wl_buffer::WlBuffer;
use wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};

use super::{SurfaceKey, WlState};

/// CLOCK_MONOTONIC, the clock every presentation time is on.
const CLOCK_MONOTONIC: u32 = 1;

#[derive(Default)]
pub(crate) struct Feedbacks {
    /// Feedback requested since the surface's last commit.
    pending: HashMap<SurfaceKey, Vec<WpPresentationFeedback>>,
    /// Feedback for committed content, by the buffer that carries it.
    by_buffer: HashMap<ObjectId, Vec<(SurfaceKey, WpPresentationFeedback)>>,
}

impl Feedbacks {
    /// The surface committed: its pending feedback waits for `buffer`, or
    /// is discarded when the commit brought no new buffer.
    pub(crate) fn commit(&mut self, key: SurfaceKey, buffer: Option<&WlBuffer>) {
        let Some(feedbacks) = self.pending.remove(&key) else {
            return;
        };
        match buffer {
            Some(buffer) => self
                .by_buffer
                .entry(buffer.id())
                .or_default()
                .extend(feedbacks.into_iter().map(|feedback| (key, feedback))),
            None => {
                for feedback in feedbacks {
                    feedback.discarded();
                }
            }
        }
    }

    pub(crate) fn forget_surface(&mut self, key: SurfaceKey) {
        for feedback in self.pending.remove(&key).unwrap_or_default() {
            feedback.discarded();
        }
        for feedbacks in self.by_buffer.values_mut() {
            feedbacks.retain(|(owner, feedback)| {
                if *owner == key {
                    feedback.discarded();
                    false
                } else {
                    true
                }
            });
        }
        self.by_buffer.retain(|_, feedbacks| !feedbacks.is_empty());
    }
}

pub(crate) fn create_globals(handle: &DisplayHandle) {
    handle.create_global::<WlState, WpPresentation, ()>(1, ());
}

/// `buffer`'s content is on the panel since `time_ns` (CLOCK_MONOTONIC).
pub(crate) fn presented(
    state: &mut WlState,
    buffer: &WlBuffer,
    time_ns: u64,
    sequence: u64,
    vsync: bool,
) {
    let Some(feedbacks) = state.feedbacks.by_buffer.remove(&buffer.id()) else {
        return;
    };
    let refresh_ns = if state.refresh_mhz > 0 {
        (1_000_000_000_000_u64 / state.refresh_mhz as u64) as u32
    } else {
        0
    };
    let seconds = time_ns / 1_000_000_000;
    let flags = if vsync {
        Kind::Vsync | Kind::HwClock | Kind::HwCompletion
    } else {
        Kind::empty()
    };
    for (_, feedback) in feedbacks {
        if !feedback.is_alive() {
            continue;
        }
        for output in &state.outputs {
            if output.id().same_client_as(&feedback.id()) {
                feedback.sync_output(output);
            }
        }
        feedback.presented(
            (seconds >> 32) as u32,
            seconds as u32,
            (time_ns % 1_000_000_000) as u32,
            refresh_ns,
            (sequence >> 32) as u32,
            sequence as u32,
            flags,
        );
    }
}

impl GlobalDispatch<WpPresentation, ()> for WlState {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<WpPresentation>,
        _data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let presentation = data_init.init(resource, ());
        presentation.clock_id(CLOCK_MONOTONIC);
    }
}

impl Dispatch<WpPresentation, ()> for WlState {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &WpPresentation,
        request: wp_presentation::Request,
        _data: &(),
        _handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let wp_presentation::Request::Feedback { surface, callback } = request {
            let feedback = data_init.init(callback, ());
            match state.surface_key(&surface) {
                Some(key) => state
                    .feedbacks
                    .pending
                    .entry(key)
                    .or_default()
                    .push(feedback),
                None => feedback.discarded(),
            }
        }
    }
}

impl Dispatch<WpPresentationFeedback, ()> for WlState {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &WpPresentationFeedback,
        _request: <WpPresentationFeedback as Resource>::Request,
        _data: &(),
        _handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }
}
