//! The call screens, as on iPhone: an incoming call (slide to answer on the
//! lock screen, Decline and Accept otherwise; Hold & Accept for a call
//! waiting), and the in-call screen (mute, keypad, speaker, end). Moon
//! draws them rather than the Phone app, so they work locked and straight
//! from screen-off. A swipe up from the bottom tucks the in-call screen
//! away; the status bar's clock then sits in a green pill that brings it
//! back. State comes from `moon-telephonyd` (telephony_client.rs); every
//! command is the user's explicit action.

use std::time::{Duration, Instant};

use crate::gfx::{Canvas, Path, RectF};
use crate::model::{AppId, ShellState, View};
use crate::motion::{Sheet, Spring};
use crate::telephony_client::{Call, CallCommand, Event, TelStore};
use crate::text::{self, Weight};
use crate::ui::{GREEN, RED, SECONDARY, SECONDARY_ALPHA, WHITE};

const SCREEN_W: f32 = 1080.0;
pub const SCREEN_H: f32 = 2340.0;
/// "Call Ended" stays this long before the screen goes.
const ENDED_FOR: Duration = Duration::from_millis(1200);
const TAP_SLOP: f32 = 30.0;

/// The caller's name or number, and the status line under it.
const NAME_BASELINE: f32 = 420.0;
pub const LINE: RectF = RectF::new(0.0, 450.0, SCREEN_W, 80.0);
const BUTTON_RADIUS: f32 = 108.0;
/// Decline / Accept (and Hold & Accept) on an unlocked incoming call.
const ANSWER_Y: f32 = 1930.0;
/// The in-call controls and the end button.
const CONTROL_Y: f32 = 1300.0;
const SWAP_Y: f32 = 1600.0;
const END_Y: f32 = 2010.0;
const COLUMNS: [f32; 3] = [240.0, 540.0, 840.0];
/// In-call keypad: three columns, four rows.
const KEYPAD_TOP: f32 = 820.0;
const KEYPAD_PITCH: f32 = 262.0;
const KEYPAD_RADIUS: f32 = 110.0;
const KEYS: [&str; 12] = ["1", "2", "3", "4", "5", "6", "7", "8", "9", "*", "0", "#"];
const LETTERS: [&str; 12] = [
    "", "ABC", "DEF", "GHI", "JKL", "MNO", "PQRS", "TUV", "WXYZ", "", "+", "",
];

/// Slide to answer: the track and its knob.
pub const TRACK: RectF = RectF::new(90.0, 1880.0, 900.0, 188.0);
pub const KNOB: f32 = 172.0;
const KNOB_INSET: f32 = 8.0;
pub const KNOB_TRAVEL: f32 = TRACK.w - KNOB - 2.0 * KNOB_INSET;

/// The status-bar pill (around the clock) while a call is tucked away.
pub const PILL: RectF = RectF::new(64.0, 20.0, 208.0, 76.0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallButton {
    Accept,
    Decline,
    HoldAccept,
    EndAccept,
    Mute,
    Keypad,
    Speaker,
    Swap,
    End,
    Key(u8),
    HideKeypad,
}

/// What the call screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CallView {
    Incoming {
        call: Call,
        /// Another call is up: Hold & Accept / End & Accept.
        waiting: bool,
        locked: bool,
    },
    InCall {
        call: Call,
        /// The other call, on hold.
        held: Option<Call>,
    },
    Ended {
        call: Call,
    },
}

#[derive(Debug)]
enum CallTouch {
    Button {
        button: Option<CallButton>,
        start: (f32, f32),
    },
    Knob {
        start_x: f32,
        offset: f32,
    },
    Pill {
        start: (f32, f32),
    },
}

#[derive(Debug, Default)]
pub struct CallUi {
    pub store: TelStore,
    pub outbox: Vec<CallCommand>,
    /// The in-call screen is up (false: tucked into the status-bar pill).
    pub shown: bool,
    pub keypad: bool,
    pub keypad_digits: String,
    pub mute: bool,
    pub speaker: bool,
    pub pressed: Option<CallButton>,
    /// The kind of screen on display ("incoming:ID" or "call").
    on_screen: Option<String>,
    /// Its rise and departure.
    pub sheet: Sheet,
    /// The next departure goes up (tucked away), not down.
    tuck: bool,
    /// The last call ended: shown as "Call Ended" for a moment.
    ended: Option<(Call, Instant)>,
    /// The slide-to-answer knob's offset while dragged, or springing back.
    knob_drag: Option<f32>,
    knob_spring: Option<Spring>,
    /// The daemon asked for the screen to come on (an incoming call).
    pub pending_wake: bool,
    /// A ringing call the power key already silenced: a second press
    /// declines it.
    silenced: Option<String>,
    touch: Option<CallTouch>,
}

impl CallUi {
    /// The screen to show, if any.
    pub fn view(&self, locked: bool) -> Option<CallView> {
        let calls = self.store.calls();
        if let Some(call) = calls.iter().find(|c| c.ringing()) {
            let waiting = calls.iter().any(|c| !c.ringing());
            return Some(CallView::Incoming {
                call: call.clone(),
                waiting,
                locked,
            });
        }
        if !self.shown {
            return None;
        }
        let current = calls
            .iter()
            .find(|c| c.state != "held")
            .or_else(|| calls.first());
        if let Some(call) = current {
            let held = calls
                .iter()
                .find(|c| c.id != call.id && c.state == "held")
                .cloned();
            return Some(CallView::InCall {
                call: call.clone(),
                held,
            });
        }
        self.ended
            .as_ref()
            .map(|(call, _)| CallView::Ended { call: call.clone() })
    }

    /// A call is up (or ringing) but its screen is tucked away.
    pub fn pill(&self) -> bool {
        !self.shown && !self.store.calls().is_empty()
    }

    pub fn ringing(&self) -> Option<&Call> {
        self.store.calls().iter().find(|c| c.ringing())
    }

    /// The slide-to-answer knob's offset along its track.
    pub fn knob_offset(&self, now: Instant) -> f32 {
        if let Some(offset) = self.knob_drag {
            return offset;
        }
        self.knob_spring
            .map_or(0.0, |spring| spring.value(now).clamp(0.0, KNOB_TRAVEL))
    }
}

/// `ctl state` telephony summary, for tests and diagnostics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicCalls {
    pub connected: bool,
    pub calls: usize,
    pub screen: Option<&'static str>,
    pub pill: bool,
    pub mute: bool,
    pub speaker: bool,
    pub keypad: bool,
}

impl ShellState {
    pub fn calls_public(&self) -> PublicCalls {
        let calls = &self.calls;
        PublicCalls {
            connected: calls.store.connected,
            calls: calls.store.calls().len(),
            screen: calls
                .view(self.view == View::Locked)
                .map(|view| match view {
                    CallView::Incoming { .. } => "incoming",
                    CallView::InCall { .. } => "in_call",
                    CallView::Ended { .. } => "ended",
                }),
            pill: calls.pill(),
            mute: calls.mute,
            speaker: calls.speaker,
            keypad: calls.keypad,
        }
    }

    pub fn call_view(&self) -> Option<CallView> {
        self.calls.view(self.view == View::Locked)
    }

    /// Applies a telephony event: the store, then what the user sees.
    pub fn telephony_event(&mut self, event: Event) {
        let now = Instant::now();
        let before = self.calls.store.calls().to_vec();
        self.calls.store.apply(&event);
        match &event {
            Event::CallIncoming { .. } => {
                self.calls.pending_wake = true;
            }
            Event::CallEnded { call, .. } => {
                let last = self.calls.store.calls().is_empty();
                let was_up = before.iter().any(|c| c.id == call.id && !c.ringing());
                if last && was_up && self.calls.shown {
                    let mut ended = before
                        .iter()
                        .find(|c| c.id == call.id)
                        .cloned()
                        .unwrap_or_else(|| call.clone());
                    ended.state = "ended".into();
                    self.calls.ended = Some((ended, now));
                }
            }
            _ => {}
        }
        let calls = self.calls.store.calls();
        if calls.is_empty() {
            // The last call is over: audio and the keypad reset, as the
            // daemon resets its audio route.
            self.calls.mute = false;
            self.calls.speaker = false;
            self.calls.keypad = false;
            self.calls.keypad_digits.clear();
            self.calls.silenced = None;
            if self.calls.ended.is_none() {
                self.calls.shown = false;
            }
        }
        let new_call = calls.iter().any(|c| !before.iter().any(|b| b.id == c.id));
        if new_call {
            // A call that starts (ringing, or dialed from the Phone app)
            // brings its screen up.
            self.calls.shown = true;
            self.calls.ended = None;
        }
        self.sync_call_screen(now);
        self.redraw = true;
    }

    pub fn telephony_connected(&mut self, connected: bool) {
        if self.calls.store.connected != connected {
            self.calls.store.connected = connected;
            if !connected {
                self.calls.ended = None;
            }
            self.sync_call_screen(Instant::now());
            self.redraw = true;
        }
    }

    /// Notes when the call screen appears or changes kind; over an app it
    /// needs the app's snapshot beneath it while it rises.
    fn sync_call_screen(&mut self, now: Instant) {
        let kind = self.call_view().map(|view| match view {
            CallView::Incoming { call, .. } => format!("incoming:{}", call.id),
            CallView::InCall { .. } | CallView::Ended { .. } => "call".to_owned(),
        });
        match (kind, &self.calls.on_screen) {
            (Some(kind), Some(shown)) if *shown == kind => {}
            (Some(kind), previous) => {
                // A ringing call answered keeps the screen in place.
                if previous.is_none() {
                    if let Some(app) = AppId::from_view(self.view)
                        && self.screen_awake
                        && !self.alerts_cover_app(now)
                    {
                        self.pending_capture = Some(app);
                    }
                    self.calls.sheet.show(now, self.screen_awake);
                }
                self.calls.on_screen = Some(kind);
                self.calls.pressed = None;
                self.calls.knob_drag = None;
                self.calls.knob_spring = None;
                self.calls.touch = None;
                self.keyboard.hide();
            }
            (None, Some(_)) => {
                self.calls.on_screen = None;
                self.calls.pressed = None;
                self.calls.touch = None;
                let up = std::mem::take(&mut self.calls.tuck);
                self.calls.sheet.hide(now, up, self.screen_awake);
            }
            (None, None) => {}
        }
    }

    /// The call screen is up, or still leaving.
    pub fn calls_cover(&self) -> bool {
        self.calls.sheet.visible()
    }

    pub fn calls_animating(&self, now: Instant) -> bool {
        self.calls.sheet.animating(now)
            || self
                .calls
                .knob_spring
                .is_some_and(|spring| !spring.settled(now, 0.5))
    }

    /// When the call screen must redraw by itself: the call timer's next
    /// second, or the end of "Call Ended".
    pub fn calls_deadline(&self, now: Instant) -> Option<Instant> {
        if let Some((_, at)) = &self.calls.ended {
            return Some(*at + ENDED_FOR);
        }
        let connected = self.call_view().and_then(|view| match view {
            CallView::InCall { call, .. } => call.connected_at,
            _ => None,
        })?;
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        let since = unix - connected as f64;
        let next = 1.0 - since.rem_euclid(1.0);
        Some(now + Duration::from_secs_f64(next.max(0.01)))
    }

    pub fn calls_tick(&mut self, now: Instant) {
        if self.calls.sheet.tick(now) {
            self.redraw = true;
        }
        if self
            .calls
            .ended
            .as_ref()
            .is_some_and(|(_, at)| now >= *at + ENDED_FOR)
        {
            self.calls.ended = None;
            if self.calls.store.calls().is_empty() {
                self.calls.shown = false;
            }
            self.sync_call_screen(now);
            self.redraw = true;
        }
        if let Some(spring) = self.calls.knob_spring
            && spring.settled(now, 0.5)
        {
            self.calls.knob_spring = None;
        }
    }

    /// The power key while a call rings silences it (as on iPhone); a
    /// second press declines. Returns true when it did either.
    pub fn calls_power_key(&mut self) -> bool {
        let Some(call) = self.calls.ringing().cloned() else {
            return false;
        };
        if self.calls.silenced.as_deref() == Some(call.id.as_str()) {
            self.call_command(CallCommand::Reject);
            self.last_action = "call_declined".into();
        } else {
            // The ringtone is the ringing notification: dismissing it
            // stops the sound and keeps the call ringing on screen.
            let ringing = self
                .alerts
                .store
                .notifications
                .iter()
                .find(|n| n.app == "phone" && n.ringing)
                .map(|n| n.id.clone());
            if let Some(id) = ringing {
                self.alerts
                    .outbox
                    .push(crate::alerts::Outgoing::Dismiss(id));
            }
            self.calls.silenced = Some(call.id);
            self.last_action = "call_silenced".into();
        }
        true
    }

    fn call_command(&mut self, command: CallCommand) {
        self.calls.outbox.push(command);
        self.redraw = true;
    }

    /// A touch went down. Returns true when the call screen (or its pill)
    /// owns it.
    pub fn call_pointer_begin(&mut self, x: f32, y: f32) -> bool {
        let now = Instant::now();
        let Some(view) = self.call_view() else {
            if self.calls.pill() && PILL.inset(-24.0).contains(x, y) {
                self.calls.touch = Some(CallTouch::Pill { start: (x, y) });
                return true;
            }
            return false;
        };
        if let CallView::Incoming {
            locked: true,
            waiting: false,
            ..
        } = view
        {
            let knob = knob_rect(self.calls.knob_offset(now));
            if knob.inset(-30.0).contains(x, y) {
                self.calls.knob_spring = None;
                self.calls.knob_drag = Some(self.calls.knob_offset(now));
                self.calls.touch = Some(CallTouch::Knob {
                    start_x: x,
                    offset: self.calls.knob_offset(now),
                });
                self.redraw = true;
                return true;
            }
        }
        let button = button_at(&view, self.calls.keypad, x, y);
        self.calls.pressed = button;
        self.calls.touch = Some(CallTouch::Button {
            button,
            start: (x, y),
        });
        self.redraw = true;
        true
    }

    pub fn call_touch_active(&self) -> bool {
        self.calls.touch.is_some()
    }

    pub fn call_pointer_move(&mut self, x: f32, y: f32) {
        match self.calls.touch.as_mut() {
            Some(CallTouch::Knob { start_x, offset }) => {
                let value = (*offset + x - *start_x).clamp(0.0, KNOB_TRAVEL);
                self.calls.knob_drag = Some(value);
                self.redraw = true;
            }
            Some(CallTouch::Button { button, start }) => {
                if button.is_some()
                    && ((x - start.0).abs() > TAP_SLOP * 2.0
                        || (y - start.1).abs() > TAP_SLOP * 2.0)
                {
                    *button = None;
                    self.calls.pressed = None;
                    self.redraw = true;
                }
            }
            Some(CallTouch::Pill { .. }) | None => {}
        }
    }

    pub fn call_pointer_end(&mut self, x: f32, y: f32) {
        let now = Instant::now();
        let touch = self.calls.touch.take();
        self.calls.pressed = None;
        self.redraw = true;
        match touch {
            Some(CallTouch::Knob { .. }) => {
                let offset = self.calls.knob_drag.take().unwrap_or(0.0);
                if offset >= KNOB_TRAVEL * 0.8 {
                    self.calls.knob_drag = Some(KNOB_TRAVEL);
                    self.call_command(CallCommand::Answer);
                    self.last_action = "call_answered".into();
                } else {
                    self.calls.knob_spring = Some(Spring::new(offset, 0.0, 0.0, now, 0.35, 0.86));
                }
            }
            Some(CallTouch::Pill { start }) => {
                if (x - start.0).abs() <= TAP_SLOP && (y - start.1).abs() <= TAP_SLOP {
                    self.calls.shown = true;
                    self.sync_call_screen(now);
                    self.last_action = "call_screen_shown".into();
                }
            }
            Some(CallTouch::Button { button, start }) => {
                let rise = start.1 - y;
                let tuck = start.1 >= SCREEN_H - 180.0
                    && rise >= 250.0
                    && matches!(self.call_view(), Some(CallView::InCall { .. }));
                if tuck {
                    self.calls.shown = false;
                    self.calls.tuck = true;
                    self.calls.keypad = false;
                    self.sync_call_screen(now);
                    self.last_action = "call_screen_tucked".into();
                    return;
                }
                if let Some(button) = button {
                    self.press_call_button(button);
                }
            }
            None => {}
        }
    }

    pub fn call_cancel_touch(&mut self) {
        self.calls.touch = None;
        self.calls.pressed = None;
        if self.calls.knob_drag.take().is_some() {
            self.calls.knob_spring = None;
        }
    }

    fn press_call_button(&mut self, button: CallButton) {
        let calls = &mut self.calls;
        match button {
            CallButton::Accept | CallButton::HoldAccept => {
                calls.outbox.push(CallCommand::Answer);
                self.last_action = "call_answered".into();
            }
            CallButton::EndAccept => {
                // End the call that is up, then answer the waiting one.
                let active = calls
                    .store
                    .calls()
                    .iter()
                    .find(|c| !c.ringing() && c.state != "held")
                    .map(|c| c.id.clone());
                calls.outbox.push(CallCommand::Hangup(active));
                calls.outbox.push(CallCommand::Answer);
                self.last_action = "call_end_and_answer".into();
            }
            CallButton::Decline => {
                calls.outbox.push(CallCommand::Reject);
                self.last_action = "call_declined".into();
            }
            CallButton::End => {
                let current = match calls.view(false) {
                    Some(CallView::InCall { call, .. }) => Some(call.id),
                    _ => None,
                };
                calls.outbox.push(CallCommand::Hangup(current));
                self.last_action = "call_ended".into();
            }
            CallButton::Mute | CallButton::Speaker => {
                if button == CallButton::Mute {
                    calls.mute = !calls.mute;
                } else {
                    calls.speaker = !calls.speaker;
                }
                calls.outbox.push(CallCommand::Audio {
                    speaker: calls.speaker,
                    mute: calls.mute,
                });
            }
            CallButton::Keypad => calls.keypad = true,
            CallButton::HideKeypad => calls.keypad = false,
            CallButton::Swap => calls.outbox.push(CallCommand::Swap),
            CallButton::Key(index) => {
                let key = KEYS[usize::from(index)];
                calls.keypad_digits.push_str(key);
                if calls.keypad_digits.len() > 24 {
                    calls.keypad_digits.drain(..1);
                }
                calls.outbox.push(CallCommand::Dtmf(key.to_owned()));
            }
        }
        self.redraw = true;
    }
}

fn knob_rect(offset: f32) -> RectF {
    RectF::new(
        TRACK.x + KNOB_INSET + offset,
        TRACK.y + (TRACK.h - KNOB) / 2.0,
        KNOB,
        KNOB,
    )
}

/// The knob's top-left corner on screen, for the compositor.
pub fn knob_origin(offset: f32) -> (f32, f32) {
    let rect = knob_rect(offset);
    (rect.x, rect.y)
}

fn near(x: f32, y: f32, (cx, cy): (f32, f32), radius: f32) -> bool {
    (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
}

fn keypad_center(index: usize) -> (f32, f32) {
    (
        COLUMNS[index % 3],
        KEYPAD_TOP + (index / 3) as f32 * KEYPAD_PITCH,
    )
}

/// The buttons of a call screen, with their centers.
fn buttons(view: &CallView, keypad: bool) -> Vec<(CallButton, (f32, f32))> {
    match view {
        CallView::Incoming { waiting: true, .. } => vec![
            (CallButton::Decline, (COLUMNS[0], ANSWER_Y)),
            (CallButton::EndAccept, (COLUMNS[1], ANSWER_Y - 340.0)),
            (CallButton::HoldAccept, (COLUMNS[2], ANSWER_Y)),
        ],
        CallView::Incoming { locked: true, .. } => Vec::new(),
        CallView::Incoming { .. } => vec![
            (CallButton::Decline, (270.0, ANSWER_Y)),
            (CallButton::Accept, (810.0, ANSWER_Y)),
        ],
        CallView::InCall { held, .. } => {
            let mut list = Vec::new();
            if keypad {
                for index in 0..12 {
                    list.push((CallButton::Key(index as u8), keypad_center(index)));
                }
                list.push((CallButton::HideKeypad, (COLUMNS[2], END_Y)));
            } else {
                list.push((CallButton::Mute, (COLUMNS[0], CONTROL_Y)));
                list.push((CallButton::Keypad, (COLUMNS[1], CONTROL_Y)));
                list.push((CallButton::Speaker, (COLUMNS[2], CONTROL_Y)));
                if held.is_some() {
                    list.push((CallButton::Swap, (COLUMNS[1], SWAP_Y)));
                }
            }
            list.push((CallButton::End, (COLUMNS[1], END_Y)));
            list
        }
        CallView::Ended { .. } => Vec::new(),
    }
}

pub fn button_at(view: &CallView, keypad: bool, x: f32, y: f32) -> Option<CallButton> {
    buttons(view, keypad)
        .into_iter()
        .find(|(button, center)| {
            let radius = match button {
                CallButton::HideKeypad => 110.0,
                CallButton::Key(_) => KEYPAD_RADIUS + 16.0,
                _ => BUTTON_RADIUS + 16.0,
            };
            near(x, y, *center, radius)
        })
        .map(|(button, _)| button)
}

/// "+16475551234" as "+1 (647) 555-1234"; other numbers as they are.
pub fn format_number(number: &str) -> String {
    let digits: String = number.chars().filter(char::is_ascii_digit).collect();
    let nanp = |d: &str| format!("({}) {}-{}", &d[..3], &d[3..6], &d[6..]);
    if number.starts_with("+1") && digits.len() == 11 {
        format!("+1 {}", nanp(&digits[1..]))
    } else if !number.starts_with('+') && digits.len() == 10 && digits == number {
        nanp(&digits)
    } else {
        number.to_owned()
    }
}

pub fn caller(call: &Call) -> String {
    match (&call.name, &call.number) {
        (Some(name), _) if !name.is_empty() => name.clone(),
        (_, Some(number)) if !number.is_empty() => format_number(number),
        _ => "Unknown".to_owned(),
    }
}

/// The line under the caller: what the call is doing, or how long it has
/// been up.
pub fn status_line(view: &CallView, now_unix: i64) -> String {
    match view {
        CallView::Incoming { waiting: true, .. } => "call waiting".into(),
        CallView::Incoming { .. } => "incoming call".into(),
        CallView::Ended { .. } => "Call Ended".into(),
        CallView::InCall { call, .. } => match call.state.as_str() {
            "dialing" => "calling…".into(),
            "alerting" => "ringing…".into(),
            "held" => "on hold".into(),
            _ => call
                .connected_at
                .map(|at| duration_text((now_unix - at).max(0) as u64))
                .unwrap_or_default(),
        },
    }
}

pub fn duration_text(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// What the call layer shows; it is redrawn only when this changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallKey {
    pub view: CallView,
    pub keypad: bool,
    pub digits: String,
    pub mute: bool,
    pub speaker: bool,
    pub pressed: Option<CallButton>,
}

/// The call screen, over its darkened wallpaper (the status line and the
/// slide knob are separate layers).
pub fn draw_call_screen(canvas: &mut Canvas, key: &CallKey) {
    canvas.clear(0);
    let cx = SCREEN_W / 2.0;
    let title = match (&key.view, key.keypad) {
        (CallView::InCall { .. }, true) if !key.digits.is_empty() => key.digits.clone(),
        (CallView::Incoming { call, .. }, _)
        | (CallView::InCall { call, .. }, _)
        | (CallView::Ended { call }, _) => caller(call),
    };
    let size = 92.0;
    let fitted = text::fit(&title, Weight::Regular, size, SCREEN_W - 120.0);
    text::draw_centered(
        canvas,
        &fitted,
        cx,
        NAME_BASELINE,
        Weight::Regular,
        size,
        WHITE,
        1.0,
    );
    if let CallView::InCall {
        held: Some(held), ..
    } = &key.view
    {
        let line = format!("{} · on hold", caller(held));
        let line = text::fit(&line, Weight::Regular, 42.0, SCREEN_W - 160.0);
        text::draw_centered(
            canvas,
            &line,
            cx,
            600.0,
            Weight::Regular,
            42.0,
            SECONDARY,
            SECONDARY_ALPHA,
        );
    }
    match &key.view {
        CallView::Incoming {
            locked: true,
            waiting: false,
            ..
        } => {
            canvas.fill_rrect(TRACK, TRACK.h / 2.0, WHITE, 0.16);
            let text_x = TRACK.x + KNOB + KNOB_INSET + (TRACK.w - KNOB - KNOB_INSET) / 2.0;
            text::draw_centered(
                canvas,
                "slide to answer",
                text_x,
                text::centered_baseline(TRACK.y + TRACK.h / 2.0, Weight::Regular, 54.0),
                Weight::Regular,
                54.0,
                WHITE,
                0.72,
            );
        }
        CallView::Ended { .. } => {
            // The controls stay, dimmed, while "Call Ended" shows.
            let ghost = CallView::InCall {
                call: match &key.view {
                    CallView::Ended { call } => call.clone(),
                    _ => unreachable!(),
                },
                held: None,
            };
            draw_buttons(canvas, key, &ghost, 0.35);
        }
        view => draw_buttons(canvas, key, view, 1.0),
    }
}

fn draw_buttons(canvas: &mut Canvas, key: &CallKey, view: &CallView, alpha: f32) {
    for (button, (cx, cy)) in buttons(view, key.keypad) {
        let pressed = key.pressed == Some(button);
        let dim = if pressed { 0.6 } else { 1.0 };
        match button {
            CallButton::Accept | CallButton::HoldAccept | CallButton::EndAccept => {
                canvas.fill_circle(cx, cy, BUTTON_RADIUS, GREEN, alpha * dim);
                draw_handset(canvas, cx, cy, 1.25, false, WHITE);
                let label = match button {
                    CallButton::Accept => "Accept",
                    CallButton::HoldAccept => "Hold & Accept",
                    _ => "End & Accept",
                };
                draw_label(canvas, label, cx, cy, alpha);
            }
            CallButton::Decline | CallButton::End => {
                canvas.fill_circle(cx, cy, BUTTON_RADIUS, RED, alpha * dim);
                draw_handset(canvas, cx, cy, 1.25, true, WHITE);
                if button == CallButton::Decline {
                    draw_label(canvas, "Decline", cx, cy, alpha);
                }
            }
            CallButton::Mute | CallButton::Keypad | CallButton::Speaker | CallButton::Swap => {
                let on = match button {
                    CallButton::Mute => key.mute,
                    CallButton::Speaker => key.speaker,
                    _ => false,
                };
                let (fill, fill_alpha, ink) = if on {
                    (WHITE, 1.0, 0x000000)
                } else {
                    (WHITE, if pressed { 0.42 } else { 0.18 }, WHITE)
                };
                canvas.fill_circle(cx, cy, BUTTON_RADIUS, fill, fill_alpha * alpha);
                match button {
                    CallButton::Mute => draw_microphone(canvas, cx, cy, ink),
                    CallButton::Keypad => draw_keypad_glyph(canvas, cx, cy, ink),
                    CallButton::Speaker => crate::ui::draw_speaker(canvas, cx - 6.0, cy, 70, ink),
                    _ => draw_swap(canvas, cx, cy, ink),
                }
                let label = match button {
                    CallButton::Mute => "mute",
                    CallButton::Keypad => "keypad",
                    CallButton::Speaker => "speaker",
                    _ => "swap",
                };
                draw_label(canvas, label, cx, cy, alpha);
            }
            CallButton::Key(index) => {
                let index = usize::from(index);
                canvas.fill_circle(
                    cx,
                    cy,
                    KEYPAD_RADIUS,
                    WHITE,
                    if pressed { 0.48 } else { 0.17 },
                );
                let letters = LETTERS[index];
                let symbol = KEYS[index];
                let (size, baseline) = match symbol {
                    "*" => (130.0, cy + 58.0),
                    "#" => (
                        92.0,
                        text::centered_baseline(cy, Weight::DisplayLight, 92.0),
                    ),
                    _ if letters.is_empty() => (
                        104.0,
                        text::centered_baseline(cy, Weight::DisplayLight, 104.0),
                    ),
                    _ => (104.0, cy + 22.0),
                };
                text::draw_centered(
                    canvas,
                    symbol,
                    cx,
                    baseline,
                    Weight::DisplayLight,
                    size,
                    WHITE,
                    1.0,
                );
                if !letters.is_empty() {
                    let tracking = 5.0;
                    let width = text::measure_tracked(letters, Weight::SemiBold, 27.0, tracking);
                    text::draw_tracked(
                        canvas,
                        letters,
                        cx - width / 2.0,
                        cy + 66.0,
                        Weight::SemiBold,
                        27.0,
                        tracking,
                        WHITE,
                        1.0,
                    );
                }
            }
            CallButton::HideKeypad => {
                text::draw_centered(
                    canvas,
                    "Hide",
                    cx,
                    text::centered_baseline(cy, Weight::Regular, 52.0),
                    Weight::Regular,
                    52.0,
                    WHITE,
                    if pressed { 0.5 } else { 1.0 },
                );
            }
        }
    }
}

fn draw_label(canvas: &mut Canvas, label: &str, cx: f32, cy: f32, alpha: f32) {
    text::draw_centered(
        canvas,
        label,
        cx,
        cy + BUTTON_RADIUS + 62.0,
        Weight::Regular,
        40.0,
        WHITE,
        alpha,
    );
}

/// The status line under the caller (its own layer: the call timer
/// redraws it every second).
pub fn draw_status_line(canvas: &mut Canvas, line: &str) {
    canvas.clear(0);
    text::draw_centered(
        canvas,
        line,
        SCREEN_W / 2.0,
        text::centered_baseline(LINE.h / 2.0, Weight::Regular, 46.0),
        Weight::Regular,
        46.0,
        SECONDARY,
        SECONDARY_ALPHA,
    );
}

/// The slide-to-answer knob (its own layer, moved by the compositor).
pub fn draw_knob(canvas: &mut Canvas) {
    canvas.clear(0);
    let r = KNOB / 2.0;
    canvas.fill_circle(r, r, r, GREEN, 1.0);
    draw_handset(canvas, r, r, 1.05, false, WHITE);
}

/// A telephone handset, tilted as the iPhone's phone glyph (answer) or
/// turned to lie flat with its pieces down (hang up).
pub fn draw_handset(canvas: &mut Canvas, cx: f32, cy: f32, scale: f32, down: bool, color: u32) {
    // One outline: a spine along an arc around the lower left, thickening
    // inward into the ear- and mouthpiece at its ends.
    let s = scale;
    let radius = 50.0 * s;
    let (spine, pad) = (15.0 * s, 34.0 * s);
    let (from, to) = (200_f32.to_radians(), 70_f32.to_radians());
    let turn = if down { 135_f32.to_radians() } else { 0.0 };
    let (sin_t, cos_t) = turn.sin_cos();
    // The arc's center, placed so the glyph sits centered on (cx, cy).
    let (ox, oy) = (0.32 * radius, -0.32 * radius);
    let place = |x: f32, y: f32| {
        let (x, y) = (x + ox, y + oy);
        (cx + x * cos_t - y * sin_t, cy + x * sin_t + y * cos_t)
    };
    let smooth = |e0: f32, e1: f32, x: f32| {
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    const STEPS: usize = 64;
    let mut outer = Vec::with_capacity(STEPS + 1);
    let mut inner = Vec::with_capacity(STEPS + 1);
    for step in 0..=STEPS {
        let u = step as f32 / STEPS as f32;
        let angle = from + (to - from) * u;
        let (sin, cos) = angle.sin_cos();
        let piece = smooth(0.30, 0.17, u) + smooth(0.70, 0.83, u);
        let thickness = spine + (pad - spine) * piece;
        // Rounded corners at both ends.
        let corner = 7.0 * s;
        let from_end = u.min(1.0 - u) * (from - to).abs() * radius;
        let round = if from_end < corner {
            corner
                - (corner * corner - (corner - from_end).powi(2))
                    .max(0.0)
                    .sqrt()
        } else {
            0.0
        };
        let out = radius + spine / 2.0 - round;
        let inn = radius + spine / 2.0 - thickness + round;
        outer.push(place(cos * out, sin * out));
        inner.push(place(cos * inn, sin * inn));
    }
    let mut outline = outer;
    outline.extend(inner.into_iter().rev());
    let mut path = Path::new();
    path.polygon(&outline);
    canvas.fill_path(&path, color, 1.0);
}

fn draw_microphone(canvas: &mut Canvas, cx: f32, cy: f32, ink: u32) {
    canvas.fill_rrect(RectF::new(cx - 17.0, cy - 50.0, 34.0, 62.0), 17.0, ink, 1.0);
    canvas.ring_arc(cx, cy - 4.0, 32.0, 6.0, 0.0, std::f32::consts::PI, ink, 1.0);
    let mut stem = Path::new();
    stem.stroke_segment((cx, cy + 28.0), (cx, cy + 46.0), 6.0);
    stem.stroke_segment((cx - 18.0, cy + 46.0), (cx + 18.0, cy + 46.0), 6.0);
    canvas.fill_path(&stem, ink, 1.0);
}

fn draw_keypad_glyph(canvas: &mut Canvas, cx: f32, cy: f32, ink: u32) {
    for row in 0..4 {
        for column in 0..3 {
            if row == 3 && column != 1 {
                continue;
            }
            canvas.fill_circle(
                cx + (column as f32 - 1.0) * 26.0,
                cy - 39.0 + row as f32 * 26.0,
                9.0,
                ink,
                1.0,
            );
        }
    }
}

fn draw_swap(canvas: &mut Canvas, cx: f32, cy: f32, ink: u32) {
    let mut arrows = Path::new();
    arrows.stroke_segment((cx - 34.0, cy - 16.0), (cx + 30.0, cy - 16.0), 7.0);
    arrows.polygon(&[
        (cx + 40.0, cy - 16.0),
        (cx + 20.0, cy - 32.0),
        (cx + 20.0, cy),
    ]);
    arrows.stroke_segment((cx + 34.0, cy + 18.0), (cx - 30.0, cy + 18.0), 7.0);
    arrows.polygon(&[
        (cx - 40.0, cy + 18.0),
        (cx - 20.0, cy + 2.0),
        (cx - 20.0, cy + 34.0),
    ]);
    canvas.fill_path(&arrows, ink, 1.0);
}

/// The status bar's green pill behind the clock while a call is tucked
/// away.
pub fn draw_pill(canvas: &mut Canvas) {
    canvas.fill_rrect(PILL, PILL.h / 2.0, GREEN, 1.0);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telephony_client::TelState;

    fn call(id: &str, state: &str) -> Call {
        Call {
            id: id.into(),
            number: Some("+16475551234".into()),
            name: None,
            state: state.into(),
            direction: "incoming".into(),
            started_at: 1,
            connected_at: (state == "active").then_some(10),
        }
    }

    fn with_calls(state: &mut ShellState, calls: Vec<Call>) {
        state.telephony_connected(true);
        state.telephony_event(Event::CallsChanged { calls });
    }

    #[test]
    fn numbers_and_durations_read_like_ios() {
        assert_eq!(format_number("+16475551234"), "+1 (647) 555-1234");
        assert_eq!(format_number("6475551234"), "(647) 555-1234");
        assert_eq!(format_number("32665"), "32665");
        assert_eq!(format_number("+442071234567"), "+442071234567");
        assert_eq!(duration_text(65), "1:05");
        assert_eq!(duration_text(3725), "1:02:05");
    }

    #[test]
    fn an_incoming_call_wakes_and_answers_from_the_lock_screen() {
        let mut state = ShellState::new(true, 50);
        state.telephony_connected(true);
        state.telephony_event(Event::Snapshot {
            state: TelState::default(),
        });
        state.telephony_event(Event::CallIncoming {
            call: call("c1", "incoming"),
        });
        assert!(state.calls.pending_wake);
        assert!(matches!(
            state.call_view(),
            Some(CallView::Incoming { locked: true, .. })
        ));
        // A short slide springs back; a full one answers.
        let (kx, ky) = knob_origin(0.0);
        assert!(state.call_pointer_begin(kx + 80.0, ky + 80.0));
        state.call_pointer_move(kx + 280.0, ky + 80.0);
        state.call_pointer_end(kx + 280.0, ky + 80.0);
        assert!(state.calls.outbox.is_empty());
        state.calls_tick(Instant::now() + Duration::from_secs(2));
        assert!(state.call_pointer_begin(kx + 80.0, ky + 80.0));
        state.call_pointer_move(kx + 80.0 + KNOB_TRAVEL, ky + 80.0);
        state.call_pointer_end(kx + 80.0 + KNOB_TRAVEL, ky + 80.0);
        assert_eq!(state.calls.outbox, vec![CallCommand::Answer]);
        state.calls.outbox.clear();
        with_calls(&mut state, vec![call("c1", "active")]);
        assert!(matches!(state.call_view(), Some(CallView::InCall { .. })));
        // Mute, then end.
        assert!(state.call_pointer_begin(COLUMNS[0], CONTROL_Y));
        state.call_pointer_end(COLUMNS[0], CONTROL_Y);
        assert!(state.calls.mute);
        assert!(state.call_pointer_begin(COLUMNS[1], END_Y));
        state.call_pointer_end(COLUMNS[1], END_Y);
        assert_eq!(
            state.calls.outbox,
            vec![
                CallCommand::Audio {
                    speaker: false,
                    mute: true
                },
                CallCommand::Hangup(Some("c1".into()))
            ]
        );
        state.telephony_event(Event::CallEnded {
            call: call("c1", "ended"),
        });
        assert!(matches!(state.call_view(), Some(CallView::Ended { .. })));
        assert!(!state.calls.mute);
        state.calls_tick(Instant::now() + ENDED_FOR + Duration::from_millis(10));
        assert!(state.call_view().is_none());
    }

    #[test]
    fn unlocked_calls_use_buttons_and_tuck_into_the_pill() {
        let mut state = ShellState::new(false, 50);
        assert_eq!(state.view, View::Launcher);
        with_calls(&mut state, vec![call("c2", "incoming")]);
        assert!(state.call_pointer_begin(270.0, ANSWER_Y));
        state.call_pointer_end(270.0, ANSWER_Y);
        assert_eq!(state.calls.outbox, vec![CallCommand::Reject]);
        state.calls.outbox.clear();
        with_calls(&mut state, vec![call("c3", "active")]);
        // A swipe up from the bottom edge tucks the screen away.
        assert!(state.call_pointer_begin(540.0, SCREEN_H - 40.0));
        state.call_pointer_end(540.0, SCREEN_H - 500.0);
        assert!(state.call_view().is_none());
        assert!(state.calls.pill());
        let (px, py) = PILL.center();
        assert!(state.call_pointer_begin(px, py));
        state.call_pointer_end(px, py);
        assert!(matches!(state.call_view(), Some(CallView::InCall { .. })));
        // The keypad sends tones.
        assert!(state.call_pointer_begin(COLUMNS[1], CONTROL_Y));
        state.call_pointer_end(COLUMNS[1], CONTROL_Y);
        assert!(state.calls.keypad);
        let (kx, ky) = keypad_center(11);
        assert!(state.call_pointer_begin(kx, ky));
        state.call_pointer_end(kx, ky);
        assert_eq!(state.calls.outbox, vec![CallCommand::Dtmf("#".into())]);
        assert_eq!(state.calls.keypad_digits, "#");
    }

    #[test]
    fn the_power_key_silences_then_declines() {
        let mut state = ShellState::new(true, 50);
        with_calls(&mut state, vec![call("c4", "incoming")]);
        assert!(state.calls_power_key());
        assert!(state.calls.outbox.is_empty());
        assert!(state.calls_power_key());
        assert_eq!(state.calls.outbox, vec![CallCommand::Reject]);
    }
}
