//! Interactive motion state: the finger-following home gesture, animated app
//! cards, the app switcher, the lock-screen drag, and the power fade.
//!
//! Everything here is pure geometry and physics over time, so it is unit
//! tested without X. `model.rs` owns these values; `ui.rs` turns them into
//! compositor frames.

use std::time::{Duration, Instant};

use crate::gfx::RectF;
use crate::model::AppId;
use crate::motion::{Spring, VelocityTracker, rubber_band};

pub const SCREEN_W: f32 = 1080.0;
pub const SCREEN_H: f32 = 2340.0;
pub const SCREEN: RectF = RectF::new(0.0, 0.0, SCREEN_W, SCREEN_H);
/// The A26 panel's rounded display corners.
pub const DISPLAY_RADIUS: f32 = 56.0;
pub const CARD_RADIUS: f32 = 44.0;
/// Card size in the app switcher, relative to the screen.
pub const SWITCHER_SCALE: f32 = 0.62;
const SWITCHER_CENTER_Y: f32 = 1190.0;
const SWITCHER_GAP: f32 = 64.0;

/// Card springs: iOS-like response (seconds) and damping fraction.
const OPEN_RESPONSE: f32 = 0.42;
pub const CLOSE_RESPONSE: f32 = 0.40;
pub const RETURN_RESPONSE: f32 = 0.34;

pub fn switcher_card_size() -> (f32, f32) {
    (SCREEN_W * SWITCHER_SCALE, SCREEN_H * SWITCHER_SCALE)
}

fn switcher_pitch() -> f32 {
    switcher_card_size().0 + SWITCHER_GAP
}

/// The rectangle of switcher card `index` when card `focus` (fractional) is
/// centered.
pub fn switcher_card_rect(index: usize, focus: f32) -> RectF {
    let (w, h) = switcher_card_size();
    let cx = SCREEN_W / 2.0 + (index as f32 - focus) * switcher_pitch();
    RectF::new(cx - w / 2.0, SWITCHER_CENTER_Y - h / 2.0, w, h)
}

/// A card moving between two rectangles along a spring (progress 0 to 1).
#[derive(Debug, Clone, Copy)]
pub struct CardMotion {
    pub app: AppId,
    pub from: RectF,
    pub to: RectF,
    pub from_radius: f32,
    pub to_radius: f32,
    pub spring: Spring,
}

impl CardMotion {
    /// `velocity` (px/s) is the card's speed at the start, projected onto
    /// its path, so a flick carries straight into the motion.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        app: AppId,
        from: RectF,
        to: RectF,
        from_radius: f32,
        to_radius: f32,
        velocity: (f32, f32),
        now: Instant,
        response: f32,
        damping: f32,
    ) -> Self {
        let (fx, fy) = from.center();
        let (tx, ty) = to.center();
        let (dx, dy) = (tx - fx, ty - fy);
        let length2 = dx * dx + dy * dy;
        let progress_velocity = if length2 > 1.0 {
            ((velocity.0 * dx + velocity.1 * dy) / length2).clamp(-2.0, 12.0)
        } else {
            0.0
        };
        Self {
            app,
            from,
            to,
            from_radius,
            to_radius,
            spring: Spring::new(0.0, 1.0, progress_velocity, now, response, damping),
        }
    }

    pub fn open(app: AppId, from: RectF, from_radius: f32, now: Instant) -> Self {
        Self::new(
            app,
            from,
            SCREEN,
            from_radius,
            DISPLAY_RADIUS,
            (0.0, 0.0),
            now,
            OPEN_RESPONSE,
            1.0,
        )
    }

    pub fn progress(&self, now: Instant) -> f32 {
        self.spring.value(now)
    }

    pub fn rect(&self, now: Instant) -> RectF {
        let p = self.progress(now);
        let rect = self.from.lerp(self.to, p);
        // Overshoot may not invert the card.
        RectF::new(rect.x, rect.y, rect.w.max(1.0), rect.h.max(1.0))
    }

    pub fn radius(&self, now: Instant) -> f32 {
        let p = self.progress(now).clamp(0.0, 1.0);
        self.from_radius + (self.to_radius - self.from_radius) * p
    }

    pub fn settled(&self, now: Instant) -> bool {
        self.spring.settled(now, 0.003)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GestureOutcome {
    Home,
    App,
    Switcher,
}

/// The swipe up from the bottom edge of an app: the app's snapshot follows
/// the finger, shrinking as it rises; pausing arms the app switcher.
#[derive(Debug, Clone)]
pub struct HomeGesture {
    pub app: AppId,
    pub start: (f32, f32),
    pub current: (f32, f32),
    pub tracker: VelocityTracker,
    still_since: Option<Instant>,
    pub armed_at: Option<Instant>,
}

/// Speed below which a raised card counts as held (px/s).
const HOLD_SPEED: f32 = 220.0;
const HOLD_TIME: Duration = Duration::from_millis(110);
const HOLD_MIN_RISE: f32 = 140.0;

impl HomeGesture {
    pub fn new(app: AppId, start: (f32, f32), now: Instant) -> Self {
        let mut tracker = VelocityTracker::default();
        tracker.push(now, start.0, start.1);
        Self {
            app,
            start,
            current: start,
            tracker,
            still_since: None,
            armed_at: None,
        }
    }

    pub fn rise(&self) -> f32 {
        (self.start.1 - self.current.1).max(0.0)
    }

    pub fn update(&mut self, x: f32, y: f32, now: Instant) {
        self.current = (x, y);
        self.tracker.push(now, x, y);
        self.refresh_hold(now);
    }

    /// Re-evaluates the hold that arms the switcher (also while the finger
    /// rests without generating events).
    pub fn refresh_hold(&mut self, now: Instant) {
        let (vx, vy) = self.tracker.velocity(now);
        let speed = (vx * vx + vy * vy).sqrt();
        if self.rise() >= HOLD_MIN_RISE && speed < HOLD_SPEED {
            let since = *self.still_since.get_or_insert(now);
            if self.armed_at.is_none() && now.duration_since(since) >= HOLD_TIME {
                self.armed_at = Some(now);
            }
        } else {
            self.still_since = None;
            if -vy > 900.0 {
                // A renewed flick upward means home, not the switcher.
                self.armed_at = None;
            }
        }
    }

    pub fn armed(&self) -> bool {
        self.armed_at.is_some()
    }

    /// When the hold timer would arm the switcher, so the loop wakes for it.
    pub fn hold_deadline(&self) -> Option<Instant> {
        match (self.armed_at, self.still_since) {
            (None, Some(since)) => Some(since + HOLD_TIME),
            _ => None,
        }
    }

    fn armed_progress(&self, now: Instant) -> f32 {
        self.armed_at.map_or(0.0, |at| {
            Spring::new(0.0, 1.0, 0.0, at, 0.32, 1.0)
                .value(now)
                .clamp(0.0, 1.0)
        })
    }

    /// The card under the finger: it shrinks with the rise and keeps the
    /// finger at the same relative point near its bottom edge.
    pub fn card_rect(&self, now: Instant) -> RectF {
        let rise = self.rise();
        let free_scale = 1.0 - 0.5 * (1.0 - (-rise / 760.0).exp());
        let scale = free_scale + (SWITCHER_SCALE - free_scale) * self.armed_progress(now);
        let (w, h) = (SCREEN_W * scale, SCREEN_H * scale);
        let dx = rubber_band(self.current.0 - self.start.0, 260.0);
        let below_finger = (SCREEN_H - self.start.1).max(0.0) * scale;
        let bottom = self.current.1 + below_finger;
        RectF::new(SCREEN_W / 2.0 + dx - w / 2.0, bottom - h, w, h)
    }

    pub fn card_radius(&self, now: Instant) -> f32 {
        let scale = self.card_rect(now).w / SCREEN_W;
        DISPLAY_RADIUS + (CARD_RADIUS - DISPLAY_RADIUS) * ((1.0 - scale) * 4.0).clamp(0.0, 1.0)
    }

    /// Dim applied over the home screen behind the card (switcher look).
    pub fn backdrop_dim(&self, now: Instant) -> f32 {
        self.armed_progress(now) * SWITCHER_DIM
    }

    pub fn release(&self, now: Instant) -> (GestureOutcome, (f32, f32)) {
        let velocity = self.tracker.velocity(now);
        let up = -velocity.1;
        let outcome = if self.armed() && up < 900.0 {
            GestureOutcome::Switcher
        } else if up > 700.0 || (self.rise() > 280.0 && up > -300.0) {
            GestureOutcome::Home
        } else {
            GestureOutcome::App
        };
        (outcome, velocity)
    }
}

/// Darkening of the home screen behind switcher cards.
pub const SWITCHER_DIM: f32 = 0.55;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone)]
pub struct SwitcherDrag {
    pub start: (f32, f32),
    pub current: (f32, f32),
    pub tracker: VelocityTracker,
    pub axis: Option<Axis>,
    pub card: Option<usize>,
    pub focus_at_start: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Dismissal {
    pub app: AppId,
    pub index: usize,
    /// Vertical offset of the card (negative = up).
    pub offset: Spring,
    /// True once the card is flying away to be closed.
    pub closing: bool,
}

/// The app switcher: a horizontal strip of app cards, newest on the right.
#[derive(Debug, Clone)]
pub struct Switcher {
    pub apps: Vec<AppId>,
    focus: f32,
    focus_spring: Option<Spring>,
    pub drag: Option<SwitcherDrag>,
    pub dismissal: Option<Dismissal>,
    pub entered: Instant,
    pub exiting: Option<Instant>,
}

/// What the switcher asks the shell to do after a touch ends.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SwitcherAction {
    None,
    Open(AppId, RectF),
    Home,
}

const DISMISS_DISTANCE: f32 = 380.0;
const SWITCHER_EXIT: Duration = Duration::from_millis(220);

impl Switcher {
    pub fn new(apps: Vec<AppId>, now: Instant) -> Self {
        let focus = apps.len().saturating_sub(1) as f32;
        Self {
            apps,
            focus,
            focus_spring: None,
            drag: None,
            dismissal: None,
            entered: now,
            exiting: None,
        }
    }

    pub fn focus(&self, now: Instant) -> f32 {
        if let Some(drag) = &self.drag
            && drag.axis == Some(Axis::Horizontal)
        {
            let raw = drag.focus_at_start - (drag.current.0 - drag.start.0) / switcher_pitch();
            return self.rubber_focus(raw);
        }
        self.focus_spring
            .map_or(self.focus, |spring| spring.value(now))
    }

    fn rubber_focus(&self, raw: f32) -> f32 {
        let max = self.apps.len().saturating_sub(1) as f32;
        let pitch = switcher_pitch();
        if raw < 0.0 {
            rubber_band(raw * pitch, 0.0) / pitch
        } else if raw > max {
            max + rubber_band((raw - max) * pitch, 0.0) / pitch
        } else {
            raw
        }
    }

    /// Fade-in of the strip after it opens (0 to 1).
    pub fn entrance(&self, now: Instant) -> f32 {
        Spring::new(0.0, 1.0, 0.0, self.entered, 0.3, 1.0)
            .value(now)
            .clamp(0.0, 1.0)
    }

    /// Fade-out toward the home screen (0 to 1) once exiting.
    pub fn exit_progress(&self, now: Instant) -> f32 {
        self.exiting.map_or(0.0, |at| {
            (now.saturating_duration_since(at).as_secs_f32() / SWITCHER_EXIT.as_secs_f32())
                .clamp(0.0, 1.0)
        })
    }

    pub fn exited(&self, now: Instant) -> bool {
        self.exiting.is_some_and(|at| now >= at + SWITCHER_EXIT)
    }

    pub fn card_rect(&self, index: usize, now: Instant) -> RectF {
        let mut rect = switcher_card_rect(index, self.focus(now));
        if let Some(dismissal) = &self.dismissal
            && dismissal.index == index
        {
            rect.y += dismissal.offset.value(now);
        } else if let Some(drag) = &self.drag
            && drag.axis == Some(Axis::Vertical)
            && drag.card == Some(index)
        {
            rect.y += rubber_band((drag.current.1 - drag.start.1).min(0.0), 2000.0)
                + rubber_band((drag.current.1 - drag.start.1).max(0.0), 0.0);
        }
        rect
    }

    pub fn card_alpha(&self, index: usize, now: Instant) -> f32 {
        let rect = self.card_rect(index, now);
        let home = switcher_card_rect(index, self.focus(now));
        let lifted = (home.y - rect.y).max(0.0);
        (1.0 - lifted / 1400.0).clamp(0.0, 1.0)
    }

    pub fn card_at(&self, x: f32, y: f32, now: Instant) -> Option<usize> {
        (0..self.apps.len()).find(|index| self.card_rect(*index, now).contains(x, y))
    }

    pub fn begin_touch(&mut self, x: f32, y: f32, now: Instant) {
        if self.exiting.is_some() {
            return;
        }
        let focus = self.focus(now);
        self.focus = focus;
        self.focus_spring = None;
        let mut tracker = VelocityTracker::default();
        tracker.push(now, x, y);
        self.drag = Some(SwitcherDrag {
            start: (x, y),
            current: (x, y),
            tracker,
            axis: None,
            card: self.card_at(x, y, now),
            focus_at_start: focus,
        });
    }

    pub fn move_touch(&mut self, x: f32, y: f32, now: Instant) {
        let busy = self.dismissal.is_some();
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        drag.current = (x, y);
        drag.tracker.push(now, x, y);
        if drag.axis.is_none() {
            let (dx, dy) = (x - drag.start.0, y - drag.start.1);
            if dx.abs().max(dy.abs()) > 18.0 {
                drag.axis = Some(if dy.abs() > dx.abs() && drag.card.is_some() && !busy {
                    Axis::Vertical
                } else {
                    Axis::Horizontal
                });
            }
        }
    }

    pub fn end_touch(&mut self, now: Instant) -> SwitcherAction {
        let Some(drag) = self.drag.take() else {
            return SwitcherAction::None;
        };
        let (vx, vy) = drag.tracker.velocity(now);
        match drag.axis {
            None => match drag.card {
                Some(index) => {
                    let rect = switcher_card_rect(index, self.focus);
                    SwitcherAction::Open(self.apps[index], rect)
                }
                None => SwitcherAction::Home,
            },
            Some(Axis::Horizontal) => {
                let raw = drag.focus_at_start - (drag.current.0 - drag.start.0) / switcher_pitch();
                let current = self.rubber_focus(raw);
                let velocity = -vx / switcher_pitch();
                let max = self.apps.len().saturating_sub(1) as f32;
                let target = (current + velocity * 0.18).round().clamp(0.0, max);
                self.focus = target;
                self.focus_spring = Some(Spring::new(current, target, velocity, now, 0.36, 0.92));
                SwitcherAction::None
            }
            Some(Axis::Vertical) => {
                let Some(index) = drag.card else {
                    return SwitcherAction::None;
                };
                let offset = drag.current.1 - drag.start.1;
                let current =
                    rubber_band(offset.min(0.0), 2000.0) + rubber_band(offset.max(0.0), 0.0);
                let close = offset < -DISMISS_DISTANCE || (vy < -1100.0 && offset < -60.0);
                let target = if close { -SCREEN_H * 1.1 } else { 0.0 };
                let (response, damping) = if close { (0.32, 1.0) } else { (0.34, 0.82) };
                self.dismissal = Some(Dismissal {
                    app: self.apps[index],
                    index,
                    offset: Spring::new(current, target, vy, now, response, damping),
                    closing: close,
                });
                SwitcherAction::None
            }
        }
    }

    /// Finishes a settled dismissal. Returns the app to terminate, if the
    /// card was thrown away; the strip closes the gap.
    pub fn finish_dismissal(&mut self, now: Instant) -> Option<AppId> {
        let dismissal = self.dismissal?;
        let done = if dismissal.closing {
            dismissal.offset.value(now) < -SCREEN_H * 0.95
        } else {
            dismissal.offset.settled(now, 0.5)
        };
        if !done {
            return None;
        }
        self.dismissal = None;
        if !dismissal.closing {
            return None;
        }
        self.apps.remove(dismissal.index);
        let current = self.focus(now);
        let max = self.apps.len().saturating_sub(1) as f32;
        let target = if dismissal.index as f32 <= current {
            (current - 1.0).round()
        } else {
            current.round()
        }
        .clamp(0.0, max);
        self.focus = target;
        self.focus_spring = Some(Spring::new(
            current.min(max.max(0.0)),
            target,
            0.0,
            now,
            0.36,
            1.0,
        ));
        Some(dismissal.app)
    }

    pub fn exit(&mut self, now: Instant) {
        if self.exiting.is_none() {
            self.exiting = Some(now);
            self.drag = None;
        }
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.entrance(now) < 0.999
            || self.drag.is_some()
            || self.dismissal.is_some()
            || self
                .focus_spring
                .is_some_and(|spring| !spring.settled(now, 0.001))
            || self.exiting.is_some()
    }
}

/// The lock screen's clock-to-passcode progress (0 = clock, 1 = keypad),
/// dragged by the finger and settled by a spring.
#[derive(Debug, Clone, Default)]
pub struct LockMotion {
    rest: f32,
    spring: Option<Spring>,
    drag: Option<LockDrag>,
}

#[derive(Debug, Clone)]
struct LockDrag {
    start_y: f32,
    start_value: f32,
    current_y: f32,
    tracker: VelocityTracker,
}

/// Finger travel that moves the lock screen fully to the keypad.
const LOCK_DRAG_DISTANCE: f32 = 760.0;

impl LockMotion {
    pub fn progress(&self, now: Instant) -> f32 {
        if let Some(drag) = &self.drag {
            let raw = drag.start_value + (drag.start_y - drag.current_y) / LOCK_DRAG_DISTANCE;
            return raw.clamp(-0.08, 1.08);
        }
        self.spring.map_or(self.rest, |spring| spring.value(now))
    }

    pub fn target(&self) -> f32 {
        self.rest
    }

    pub fn dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub fn animate_to(&mut self, target: f32, now: Instant) {
        let current = self.progress(now);
        let velocity = self.spring.map_or(0.0, |spring| spring.velocity_at(now));
        self.drag = None;
        self.rest = target;
        self.spring = Some(Spring::new(current, target, velocity, now, 0.38, 1.0));
    }

    pub fn snap(&mut self, value: f32) {
        self.rest = value;
        self.spring = None;
        self.drag = None;
    }

    pub fn begin_drag(&mut self, y: f32, now: Instant) {
        let value = self.progress(now);
        let mut tracker = VelocityTracker::default();
        tracker.push(now, 0.0, y);
        self.drag = Some(LockDrag {
            start_y: y,
            start_value: value,
            current_y: y,
            tracker,
        });
        self.spring = None;
    }

    pub fn move_drag(&mut self, y: f32, now: Instant) {
        if let Some(drag) = self.drag.as_mut() {
            drag.current_y = y;
            drag.tracker.push(now, 0.0, y);
        }
    }

    /// Ends a drag; returns the face it settles on (true = keypad).
    pub fn end_drag(&mut self, now: Instant) -> bool {
        let Some(drag) = self.drag.take() else {
            return self.rest >= 0.5;
        };
        let current = (drag.start_value + (drag.start_y - drag.current_y) / LOCK_DRAG_DISTANCE)
            .clamp(-0.08, 1.08);
        let velocity = -drag.tracker.velocity(now).1 / LOCK_DRAG_DISTANCE;
        let projected = current + velocity * 0.2;
        let target = if projected >= 0.5 { 1.0 } else { 0.0 };
        self.rest = target;
        self.spring = Some(Spring::new(current, target, velocity, now, 0.38, 1.0));
        target >= 0.5
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.drag.is_some()
            || self
                .spring
                .is_some_and(|spring| !spring.settled(now, 0.002))
    }
}

/// The power-key fade to black, and the fade back in after wake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fade {
    pub out: bool,
    pub started: Instant,
}

pub const FADE_OUT: Duration = Duration::from_millis(170);
pub const FADE_IN: Duration = Duration::from_millis(240);

impl Fade {
    fn duration(&self) -> Duration {
        if self.out { FADE_OUT } else { FADE_IN }
    }

    fn t(&self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.started).as_secs_f32() / self.duration().as_secs_f32())
            .clamp(0.0, 1.0)
    }

    /// Opacity of the black overlay.
    pub fn black(&self, now: Instant) -> f32 {
        let t = self.t(now);
        if self.out {
            t * t * (3.0 - 2.0 * t)
        } else {
            let e = 1.0 - (1.0 - t).powi(3);
            1.0 - e
        }
    }

    pub fn finished(&self, now: Instant) -> bool {
        now >= self.started + self.duration()
    }

    pub fn deadline(&self) -> Instant {
        self.started + self.duration()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_motion_runs_between_rects_and_settles() {
        let now = Instant::now();
        let icon = RectF::new(400.0, 2100.0, 168.0, 168.0);
        let motion = CardMotion::open(AppId::System, icon, 38.0, now);
        assert_eq!(motion.rect(now), icon);
        let late = now + Duration::from_secs(2);
        let end = motion.rect(late);
        assert!((end.w - SCREEN_W).abs() < 1.0 && end.y.abs() < 1.0);
        assert!(motion.settled(late));
        assert!((motion.radius(late) - DISPLAY_RADIUS).abs() < 0.1);
    }

    #[test]
    fn flick_velocity_carries_into_the_card_motion() {
        let now = Instant::now();
        let from = RectF::new(0.0, 0.0, 500.0, 1000.0);
        let to = RectF::new(0.0, -2000.0, 500.0, 1000.0);
        let thrown = CardMotion::new(
            AppId::System,
            from,
            to,
            40.0,
            40.0,
            (0.0, -4000.0),
            now,
            0.4,
            1.0,
        );
        let still = CardMotion::new(
            AppId::System,
            from,
            to,
            40.0,
            40.0,
            (0.0, 0.0),
            now,
            0.4,
            1.0,
        );
        let soon = now + Duration::from_millis(30);
        assert!(thrown.progress(soon) > still.progress(soon) + 0.02);
    }

    #[test]
    fn home_gesture_card_follows_and_shrinks_with_the_finger() {
        let now = Instant::now();
        let mut gesture = HomeGesture::new(AppId::Browser, (540.0, 2300.0), now);
        let start = gesture.card_rect(now);
        assert!((start.w - SCREEN_W).abs() < 0.5);
        gesture.update(600.0, 1500.0, now + Duration::from_millis(100));
        let raised = gesture.card_rect(now + Duration::from_millis(100));
        assert!(raised.w < SCREEN_W * 0.75);
        assert!(raised.y < 0.0 + 1500.0);
        // The finger stays on the card near its bottom edge.
        assert!(raised.contains(600.0, 1500.0));
    }

    #[test]
    fn quick_flick_goes_home_short_drag_returns_and_hold_arms_switcher() {
        let start = Instant::now();
        let mut flick = HomeGesture::new(AppId::Browser, (540.0, 2300.0), start);
        for step in 1..=6 {
            flick.update(
                540.0,
                2300.0 - step as f32 * 40.0,
                start + Duration::from_millis(step * 8),
            );
        }
        assert_eq!(
            flick.release(start + Duration::from_millis(50)).0,
            GestureOutcome::Home
        );

        let mut short = HomeGesture::new(AppId::Browser, (540.0, 2300.0), start);
        short.update(540.0, 2240.0, start + Duration::from_millis(200));
        assert_eq!(
            short.release(start + Duration::from_millis(400)).0,
            GestureOutcome::App
        );

        let mut hold = HomeGesture::new(AppId::Browser, (540.0, 2300.0), start);
        hold.update(540.0, 1700.0, start + Duration::from_millis(120));
        hold.update(540.0, 1700.0, start + Duration::from_millis(260));
        hold.refresh_hold(start + Duration::from_millis(400));
        assert!(hold.armed());
        assert_eq!(
            hold.release(start + Duration::from_millis(420)).0,
            GestureOutcome::Switcher
        );
    }

    #[test]
    fn switcher_scrolls_opens_and_dismisses_cards() {
        let now = Instant::now();
        let mut switcher = Switcher::new(vec![AppId::System, AppId::Browser], now);
        assert_eq!(switcher.focus(now), 1.0);
        let browser = switcher.card_rect(1, now);
        assert!((browser.center().0 - SCREEN_W / 2.0).abs() < 0.5);

        // Tap opens the card under the finger.
        let (x, y) = browser.center();
        switcher.begin_touch(x, y, now);
        assert_eq!(
            switcher.end_touch(now),
            SwitcherAction::Open(AppId::Browser, browser)
        );

        // Swipe right brings the older card to the center.
        switcher.begin_touch(300.0, 1200.0, now);
        for step in 1..=10 {
            switcher.move_touch(
                300.0 + step as f32 * 60.0,
                1200.0,
                now + Duration::from_millis(step * 10),
            );
        }
        assert_eq!(
            switcher.end_touch(now + Duration::from_millis(110)),
            SwitcherAction::None
        );
        assert!(switcher.focus(now + Duration::from_secs(2)).abs() < 1e-3);

        // Throwing a card up closes its app.
        let t = now + Duration::from_secs(2);
        let card = switcher.card_rect(0, t);
        let (x, y) = card.center();
        switcher.begin_touch(x, y, t);
        for step in 1..=8 {
            switcher.move_touch(
                x,
                y - step as f32 * 80.0,
                t + Duration::from_millis(step * 10),
            );
        }
        switcher.end_touch(t + Duration::from_millis(90));
        assert!(switcher.dismissal.is_some_and(|d| d.closing));
        assert_eq!(
            switcher.finish_dismissal(t + Duration::from_secs(2)),
            Some(AppId::System)
        );
        assert_eq!(switcher.apps, vec![AppId::Browser]);

        // A tap on empty space goes home.
        switcher.begin_touch(540.0, 150.0, t + Duration::from_secs(3));
        assert_eq!(
            switcher.end_touch(t + Duration::from_secs(3)),
            SwitcherAction::Home
        );
    }

    #[test]
    fn lock_motion_follows_the_finger_and_settles_by_velocity() {
        let now = Instant::now();
        let mut lock = LockMotion::default();
        lock.begin_drag(2000.0, now);
        lock.move_drag(1800.0, now + Duration::from_millis(200));
        let partial = lock.progress(now + Duration::from_millis(200));
        assert!(partial > 0.2 && partial < 0.4);
        // Slow release below halfway returns to the clock.
        assert!(!lock.end_drag(now + Duration::from_millis(400)));
        // A fast flick opens the keypad even from a short drag.
        let t = now + Duration::from_secs(1);
        lock.begin_drag(2000.0, t);
        for step in 1..=5 {
            lock.move_drag(
                2000.0 - step as f32 * 50.0,
                t + Duration::from_millis(step * 10),
            );
        }
        assert!(lock.end_drag(t + Duration::from_millis(55)));
        assert!((lock.progress(t + Duration::from_secs(2)) - 1.0).abs() < 0.01);
    }

    #[test]
    fn fades_run_to_and_from_black() {
        let now = Instant::now();
        let out = Fade {
            out: true,
            started: now,
        };
        assert_eq!(out.black(now), 0.0);
        assert!((out.black(now + FADE_OUT) - 1.0).abs() < 1e-5);
        let fade_in = Fade {
            out: false,
            started: now,
        };
        assert!((fade_in.black(now) - 1.0).abs() < 1e-5);
        assert!(fade_in.black(now + FADE_IN) < 1e-5);
        assert!(fade_in.finished(now + FADE_IN));
    }
}
