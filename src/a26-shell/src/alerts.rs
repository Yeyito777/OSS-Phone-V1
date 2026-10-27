//! Notifications, alarms and timers on screen: banners, the lock-screen
//! list, the notification centre, and the ringing screen. The data comes
//! from `moon-notifyd` (see notify.rs); this module decides what is shown
//! and what the user's touches do, and draws the artwork.
//!
//! Every animation here is built to stay within a 120 Hz frame on the
//! A26's software Xorg: a banner moves a card-sized region; the centre is a
//! curtain whose content stays in place while its edge moves, so a frame
//! redraws only the strip the edge crossed; the ringing screen rises the
//! same way from the bottom.

use std::time::{Duration, Instant};

use crate::clock::{LocalTime, unix_now};
use crate::gfx::{Canvas, Image, RectF};
use crate::model::{AppId, ShellState, View};
use crate::motion::{Spring, VelocityTracker, rubber_band};
use crate::notify_client::{
    Event, Notification, NotifyStore, RingAction, RingKind, Ringing, Timer,
};
use crate::text::{self, Weight};
use crate::ui::{CARD_RAISED, SECONDARY, SECONDARY_ALPHA, WHITE};

pub const SCREEN_W: f32 = 1080.0;
pub const SCREEN_H: f32 = 2340.0;

// Notification cards (banners, lock screen, centre share one design).
pub const CARD_X: f32 = 24.0;
pub const CARD_W: f32 = 1032.0;
pub const CARD_RADIUS: f32 = 56.0;
const CARD_PAD: f32 = 36.0;
const ICON: f32 = 92.0;
const TEXT_X: f32 = CARD_PAD + ICON + 30.0;
const TITLE_SIZE: f32 = 44.0;
const BODY_SIZE: f32 = 42.0;
const LINE: f32 = 54.0;
const CARD_GAP: f32 = 18.0;

/// Just below the camera cutout, over the lower part of the status bar (as
/// iPhone banners sit just below the Dynamic Island).
pub const BANNER_TOP: f32 = crate::status_bar::CUTOUT_BOTTOM + 10.0;
/// The banner layer: the tallest card plus room for its shadow.
pub const BANNER_LAYER_H: f32 = 300.0;
const BANNER_HIDDEN: f32 = -(BANNER_TOP + BANNER_LAYER_H);
const BANNER_SHOW_FOR: Duration = Duration::from_secs(5);

// The lock-screen list sits under the clock and its charging line.
pub const LOCK_LIST_TOP: f32 = 860.0;
pub const LOCK_LIST_H: f32 = 1280.0;
const LOCK_LIST_CARDS: usize = 3;

/// A touch starting this close to the top edge (the status bar) pulls the
/// notification centre down.
pub const PULL_ZONE: f32 = 130.0;
const CENTRE_TOP: f32 = 300.0;
const CENTRE_GROUP_CARDS: usize = 3;
const CENTRE_BOTTOM: f32 = 2220.0;

/// Ringing-screen buttons.
pub const RING_PRIMARY: RectF = RectF::new(120.0, 1560.0, 840.0, 190.0);
pub const RING_SECONDARY: RectF = RectF::new(120.0, 1800.0, 840.0, 190.0);

const TAP_SLOP: f32 = 30.0;
/// How long a notification keeps the screen on by itself (as on iPhone).
pub const NOTIFY_WAKE: Duration = Duration::from_secs(10);
const ORANGE: u32 = 0xff9f0a;

/// Something Moon must tell the daemon (drained by the main loop).
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    Dismiss(String),
    DismissAll,
    Activate(String),
    MarkSeen(Vec<String>),
    Ring(RingKind, String, RingAction),
    SetDnd(bool),
    /// A haptic and its length (ms; None for the style's default).
    Haptic(crate::notify_client::Haptic, Option<u32>),
}

#[derive(Debug)]
pub struct Banner {
    pub id: String,
    offset: Spring,
    hiding: bool,
    hide_at: Option<Instant>,
    /// Finger offset while dragged (negative is up).
    drag: Option<f32>,
}

impl Banner {
    fn new(id: String, now: Instant, urgent: bool) -> Self {
        Self {
            id,
            offset: Spring::new(BANNER_HIDDEN, 0.0, 0.0, now, 0.42, 0.78),
            hiding: false,
            hide_at: (!urgent).then_some(now + BANNER_SHOW_FOR),
            drag: None,
        }
    }

    /// The card's top edge on screen.
    pub fn y(&self, now: Instant) -> f32 {
        BANNER_TOP
            + match self.drag {
                Some(drag) => drag,
                None => self.offset.value(now),
            }
    }

    fn hide(&mut self, now: Instant, velocity: f32) {
        let from = self.y(now) - BANNER_TOP;
        self.offset = Spring::new(from, BANNER_HIDDEN, velocity, now, 0.32, 1.0);
        self.hiding = true;
        self.drag = None;
        self.hide_at = None;
    }

    fn settled(&self, now: Instant) -> bool {
        self.drag.is_none() && self.offset.settled(now, 0.5)
    }
}

/// The notification centre: a curtain pulled down from the top edge.
#[derive(Debug)]
pub struct Centre {
    edge: Spring,
    /// The edge while a finger holds it.
    drag: Option<f32>,
    pub open: bool,
    pub clear_armed: bool,
    pub swipe: Option<Swipe>,
}

/// A card dragged sideways in the centre, flying out once let go past the
/// threshold.
#[derive(Debug, Clone)]
pub struct Swipe {
    pub id: String,
    pub rect: RectF,
    offset: f32,
    fly: Option<Spring>,
}

impl Swipe {
    pub fn offset(&self, now: Instant) -> f32 {
        self.fly.map_or(self.offset, |spring| spring.value(now))
    }
}

impl Default for Centre {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            edge: Spring::new(0.0, 0.0, 0.0, now, 0.35, 1.0),
            drag: None,
            open: false,
            clear_armed: false,
            swipe: None,
        }
    }
}

impl Centre {
    pub fn edge(&self, now: Instant) -> f32 {
        self.drag
            .unwrap_or_else(|| self.edge.value(now))
            .clamp(0.0, SCREEN_H)
    }

    pub fn visible(&self, now: Instant) -> bool {
        self.drag.is_some() || self.open || self.edge(now) > 0.5
    }

    fn settle(&mut self, open: bool, velocity: f32, now: Instant) {
        let from = self.edge(now);
        self.drag = None;
        self.open = open;
        self.edge = Spring::new(
            from,
            if open { SCREEN_H } else { 0.0 },
            velocity,
            now,
            0.38,
            1.0,
        );
        if !open {
            self.clear_armed = false;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingButton {
    Primary,
    Secondary,
}

#[derive(Debug)]
enum Touch {
    Banner {
        start: (f32, f32),
        last: (f32, f32),
        tracker: VelocityTracker,
    },
    Pull {
        start_y: f32,
        start_edge: f32,
        moved: bool,
        tracker: VelocityTracker,
    },
    CentreCard {
        id: String,
        rect: RectF,
        start: (f32, f32),
        horizontal: Option<bool>,
        tracker: VelocityTracker,
    },
    Ring(Option<RingButton>),
}

#[derive(Debug, Default)]
pub struct Alerts {
    pub store: NotifyStore,
    pub banner: Option<Banner>,
    pub centre: Centre,
    /// The ringing alert on screen.
    ring_shown: Option<String>,
    /// The ringing screen's rise and departure.
    pub ring_sheet: crate::motion::Sheet,
    pub ring_pressed: Option<RingButton>,
    pub outbox: Vec<Outgoing>,
    /// A lock-screen notification tapped: opened once unlocked.
    pub open_after_unlock: Option<String>,
    /// The daemon asked for the screen to come on.
    pub pending_wake: bool,
    /// The alerts covered the foreground app last tick.
    covering_app: bool,
    /// A notification turned the screen on: it goes off again then unless
    /// the user touches the phone.
    pub wake_expires: Option<Instant>,
    touch: Option<Touch>,
}

impl Alerts {
    /// The alarm or timer ringing on screen, if any.
    pub fn ringing(&self) -> Option<&Ringing> {
        self.store.ringing.first()
    }

    pub fn unseen(&self) -> Vec<&Notification> {
        self.store
            .notifications
            .iter()
            .filter(|n| !n.seen && !n.ringing)
            .collect()
    }
}

/// Alerts in `ctl state`, for tests and diagnostics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PublicAlerts {
    pub connected: bool,
    pub notifications: usize,
    pub unseen: usize,
    pub banner: Option<String>,
    pub centre_open: bool,
    pub ringing: Option<String>,
    pub alarm_set: bool,
}

impl ShellState {
    pub fn alerts_public(&self) -> PublicAlerts {
        PublicAlerts {
            connected: self.alerts.store.connected,
            notifications: self.alerts.store.notifications.len(),
            unseen: self.alerts.unseen().len(),
            banner: self.alerts.banner.as_ref().map(|b| b.id.clone()),
            centre_open: self.alerts.centre.open,
            ringing: self.alerts.ringing().map(|r| r.id.clone()),
            alarm_set: self.alerts.store.alarm_set(),
        }
    }

    pub fn alerts_cancel_touch(&mut self) {
        self.alerts.touch = None;
        self.alerts.ring_pressed = None;
        self.alerts.centre.drag = None;
    }

    /// Applies a daemon event: the store, then what the user should see.
    pub fn notify_event(&mut self, event: Event) {
        let now = Instant::now();
        self.alerts.store.apply(&event);
        match event {
            Event::Snapshot { .. } => {
                if self.alerts.ringing().is_some() {
                    self.alerts.pending_wake = true;
                }
                if let Some(banner) = &self.alerts.banner
                    && self.alerts.store.notification(&banner.id).is_none()
                {
                    self.alerts.banner = None;
                }
            }
            Event::NotificationPosted { notification, wake } => {
                if wake {
                    self.alerts.pending_wake = true;
                }
                let urgent = notification.urgency == "high";
                // A ringing call shows the call screen instead.
                let call = notification.ringing && notification.app == "phone";
                let banner_allowed = notification.urgency != "low"
                    && !call
                    && self.screen_awake
                    && self.view != View::Locked
                    && !self.alerts.centre.visible(now)
                    && self.alerts.ringing().is_none();
                if banner_allowed {
                    match self.alerts.banner.as_mut() {
                        // A shown banner swaps to the newer notification.
                        Some(banner) if !banner.hiding => {
                            banner.id = notification.id.clone();
                            banner.hide_at = (!urgent).then_some(now + BANNER_SHOW_FOR);
                        }
                        _ => self.alerts.banner = Some(Banner::new(notification.id, now, urgent)),
                    }
                }
            }
            Event::NotificationRemoved { id } => {
                if self.alerts.banner.as_ref().is_some_and(|b| b.id == id) {
                    self.alerts.banner = None;
                }
                if self
                    .alerts
                    .centre
                    .swipe
                    .as_ref()
                    .is_some_and(|s| s.id == id)
                {
                    self.alerts.centre.swipe = None;
                }
                if self.alerts.open_after_unlock.as_deref() == Some(id.as_str()) {
                    self.alerts.open_after_unlock = None;
                }
            }
            Event::AlarmRinging { .. } | Event::TimerRinging { .. } => {
                self.alerts.pending_wake = true;
                self.alerts.banner = None;
            }
            _ => {}
        }
        self.sync_ring_screen(now);
        self.redraw = true;
    }

    /// Notes when a new alert starts ringing; a ringing screen over an app
    /// needs the app's snapshot beneath it while it rises.
    fn sync_ring_screen(&mut self, now: Instant) {
        let current = self.alerts.ringing().map(|ring| ring.id.clone());
        match (current, &self.alerts.ring_shown) {
            (Some(id), Some(shown)) if *shown == id => {}
            (Some(id), _) => {
                self.alerts.ring_shown = Some(id);
                self.alerts.ring_sheet.show(now, self.screen_awake);
                self.alerts.ring_pressed = None;
                if let Some(app) = AppId::from_view(self.view)
                    && self.screen_awake
                    && !self.alerts.centre.visible(now)
                {
                    self.pending_capture = Some(app);
                }
                self.keyboard.hide();
            }
            (None, Some(_)) => {
                self.alerts.ring_shown = None;
                self.alerts.ring_pressed = None;
                self.alerts.ring_sheet.hide(now, false, self.screen_awake);
            }
            (None, None) => {}
        }
    }

    /// The daemon connection came or went.
    pub fn notify_connected(&mut self, connected: bool) {
        if self.alerts.store.connected != connected {
            self.alerts.store.connected = connected;
            self.redraw = true;
        }
    }

    /// The shell must cover the foreground app: the centre or the ringing
    /// screen is up.
    /// The centre, Control Centre or the ringing screen is up: the shell
    /// covers the foreground app.
    pub fn alerts_cover_app(&self, now: Instant) -> bool {
        self.alerts.ring_sheet.visible()
            || self.alerts.centre.visible(now)
            || self.control.visible(now)
            || self.calls_cover()
    }

    pub fn alerts_animating(&self, now: Instant) -> bool {
        let alerts = &self.alerts;
        alerts.banner.as_ref().is_some_and(|b| !b.settled(now))
            || (alerts.centre.drag.is_none() && !alerts.centre.edge.settled(now, 0.5))
            || alerts
                .centre
                .swipe
                .as_ref()
                .and_then(|s| s.fly)
                .is_some_and(|fly| !fly.settled(now, 1.0))
            || alerts.ring_sheet.animating(now)
    }

    pub fn alerts_deadline(&self) -> Option<Instant> {
        let banner = self.alerts.banner.as_ref().and_then(|b| b.hide_at);
        match (banner, self.alerts.wake_expires) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The user touched or pressed something: a notification wake no
    /// longer turns the screen back off.
    pub fn alerts_user_input(&mut self) {
        self.alerts.wake_expires = None;
    }

    /// A notification turned the screen on by itself.
    pub fn notify_woke_screen(&mut self, now: Instant) {
        if self.alerts.ringing().is_none() {
            self.alerts.wake_expires = Some(now + NOTIFY_WAKE);
        }
    }

    /// Banner timeouts, finished animations, and the centre opening.
    pub fn alerts_tick(&mut self, now: Instant) {
        if self.alerts.ring_sheet.tick(now) {
            self.redraw = true;
        }
        let covering = self.view.is_app() && self.alerts_cover_app(now);
        if self.alerts.covering_app && !covering {
            // The app is back on top of the shell.
            self.pending_reveal = true;
        }
        self.alerts.covering_app = covering;
        if self.alerts.wake_expires.is_some_and(|at| now >= at) {
            self.alerts.wake_expires = None;
            let untouched = self.screen_awake
                && self.view == View::Locked
                && self.pointer.is_none()
                && self.alerts.ringing().is_none();
            if untouched {
                // Back off, the way it came on: through the power fade.
                self.power_key();
                self.last_action = "notify_wake_expired".into();
            }
        }
        let touching_banner = matches!(self.alerts.touch, Some(Touch::Banner { .. }));
        if let Some(banner) = self.alerts.banner.as_mut() {
            if banner.hide_at.is_some_and(|at| now >= at) && !touching_banner {
                banner.hide(now, 0.0);
                self.redraw = true;
            }
            if banner.hiding && banner.settled(now) {
                self.alerts.banner = None;
                self.redraw = true;
            }
        }
        if let Some(swipe) = &self.alerts.centre.swipe
            && let Some(fly) = swipe.fly
            && fly.settled(now, 1.0)
        {
            let id = swipe.id.clone();
            self.alerts.centre.swipe = None;
            if fly.value(now).abs() > 10.0 {
                // Flew out: gone from the list now, and from the daemon.
                self.alerts.store.notifications.retain(|n| n.id != id);
                self.alerts.outbox.push(Outgoing::Dismiss(id));
            }
            self.redraw = true;
        }
        if !self.screen_awake || self.view == View::Locked {
            if self.alerts.centre.visible(now) {
                self.alerts.centre = Centre::default();
                self.redraw = true;
            }
            if let Some(banner) = self.alerts.banner.take() {
                // On the lock screen a notification is in the list instead.
                let _ = banner;
                self.redraw = true;
            }
        }
    }

    /// Opens what a notification points at and tells the daemon.
    pub fn activate_notification(&mut self, id: &str) {
        let action = self
            .alerts
            .store
            .notification(id)
            .and_then(|n| n.open.clone());
        let open = action.as_ref().and_then(|open| AppId::parse(&open.app));
        if let (Some(app), Some(action)) = (open, &action)
            && let Some(target) = &action.target
        {
            // The app reads it from Moon's state when it comes forward.
            self.open_target = Some(crate::model::OpenTarget::new(app, target.clone()));
        }
        self.alerts.outbox.push(Outgoing::Activate(id.to_owned()));
        self.alerts.store.notifications.retain(|n| n.id != id);
        if self.alerts.banner.as_ref().is_some_and(|b| b.id == id) {
            let now = Instant::now();
            if let Some(banner) = self.alerts.banner.as_mut() {
                banner.hide(now, -2000.0);
            }
        }
        if let Some(app) = open
            && AppId::from_view(self.view) != Some(app)
        {
            self.open_app(app);
        }
        self.last_action = "notification_activated".into();
        self.redraw = true;
    }

    /// Unlocked: a lock-screen notification the user tapped opens now.
    pub fn alerts_unlocked(&mut self) {
        if let Some(id) = self.alerts.open_after_unlock.take()
            && self.alerts.store.notification(&id).is_some()
        {
            self.activate_notification(&id);
        }
    }

    /// The power key while something rings snoozes an alarm or stops a
    /// timer instead of turning the screen off. Returns true when it did.
    pub fn alerts_power_key(&mut self) -> bool {
        let Some(ring) = self.alerts.ringing().cloned() else {
            return false;
        };
        let action = match ring.kind {
            RingKind::Alarm => RingAction::Again,
            RingKind::Timer => RingAction::Stop,
        };
        self.ring_action(&ring, action);
        true
    }

    fn ring_action(&mut self, ring: &Ringing, action: RingAction) {
        self.alerts
            .outbox
            .push(Outgoing::Ring(ring.kind, ring.id.clone(), action));
        // Gone at once; the daemon's *_ended confirms it.
        self.alerts.store.ringing.retain(|r| r != ring);
        self.sync_ring_screen(Instant::now());
        self.last_action = match (ring.kind, action) {
            (RingKind::Alarm, RingAction::Stop) => "alarm_stopped",
            (RingKind::Alarm, RingAction::Again) => "alarm_snoozed",
            (RingKind::Timer, RingAction::Stop) => "timer_stopped",
            (RingKind::Timer, RingAction::Again) => "timer_restarted",
        }
        .into();
        self.redraw = true;
    }

    /// A touch went down. Returns true when an alert owns it.
    pub fn alerts_pointer_begin(&mut self, x: f32, y: f32) -> bool {
        let now = Instant::now();
        let mut tracker = VelocityTracker::default();
        tracker.push(now, x, y);
        if self.alerts.ringing().is_some() {
            let button = ring_button_at(x, y);
            self.alerts.ring_pressed = button;
            self.alerts.touch = Some(Touch::Ring(button));
            self.redraw = true;
            return true;
        }
        if self.alerts.centre.visible(now) {
            if self.alerts.centre.swipe.is_some() {
                // Let a flying card finish first.
                self.alerts.touch = None;
                return true;
            }
            let layout = centre_layout(&self.alerts.store, None, unix_now());
            if let Some((id, rect)) = layout.card_at(x, y) {
                self.alerts.touch = Some(Touch::CentreCard {
                    id,
                    rect,
                    start: (x, y),
                    horizontal: None,
                    tracker,
                });
            } else {
                self.alerts.touch = Some(Touch::Pull {
                    start_y: y,
                    start_edge: self.alerts.centre.edge(now),
                    moved: false,
                    tracker,
                });
            }
            return true;
        }
        if let Some(banner) = &self.alerts.banner
            && !banner.hiding
            && banner_rect(banner.y(now), self.banner_height()).contains(x, y)
        {
            self.alerts.touch = Some(Touch::Banner {
                start: (x, y),
                last: (x, y),
                tracker,
            });
            return true;
        }
        let pull_allowed = self.screen_awake
            && y <= PULL_ZONE
            && x < crate::control_centre::SPLIT_X
            && (self.view == View::Launcher || self.view.is_app())
            && self.switcher.is_none()
            && self.gesture.is_none()
            && !self.app_launching()
            && self.fade.is_none();
        if pull_allowed {
            self.alerts.touch = Some(Touch::Pull {
                start_y: y,
                start_edge: 0.0,
                moved: false,
                tracker,
            });
            return true;
        }
        false
    }

    pub fn alerts_touch_active(&self) -> bool {
        self.alerts.touch.is_some()
    }

    pub fn alerts_pointer_move(&mut self, x: f32, y: f32) {
        let now = Instant::now();
        let view = self.view;
        let Some(touch) = self.alerts.touch.as_mut() else {
            return;
        };
        match touch {
            Touch::Ring(button) => {
                let over = ring_button_at(x, y);
                if over != *button {
                    self.alerts.ring_pressed = None;
                } else {
                    self.alerts.ring_pressed = *button;
                }
            }
            Touch::Banner {
                start,
                last,
                tracker,
            } => {
                tracker.push(now, x, y);
                *last = (x, y);
                let dy = y - start.1;
                if let Some(banner) = self.alerts.banner.as_mut() {
                    banner.drag = Some(if dy < 0.0 { dy } else { rubber_band(dy, 60.0) });
                    banner.hide_at = None;
                }
            }
            Touch::Pull {
                start_y,
                start_edge,
                moved,
                tracker,
            } => {
                tracker.push(now, x, y);
                let dy = y - *start_y;
                if !*moved && dy.abs() > TAP_SLOP {
                    *moved = true;
                    if let Some(banner) = self.alerts.banner.as_mut() {
                        banner.hide(now, 0.0);
                    }
                    if *start_edge == 0.0
                        && let Some(app) = AppId::from_view(view)
                    {
                        // The app's snapshot shows beneath the curtain.
                        self.pending_capture = Some(app);
                    }
                }
                if *moved {
                    self.alerts.centre.drag = Some((*start_edge + dy).clamp(0.0, SCREEN_H));
                }
            }
            Touch::CentreCard {
                id,
                rect,
                start,
                horizontal,
                tracker,
            } => {
                tracker.push(now, x, y);
                let (dx, dy) = (x - start.0, y - start.1);
                if horizontal.is_none() && dx.abs().max(dy.abs()) > TAP_SLOP {
                    *horizontal = Some(dx.abs() > dy.abs());
                    if *horizontal == Some(false) {
                        // A vertical drag on a card moves the curtain.
                        let start_y = start.1;
                        let tracker = std::mem::take(tracker);
                        self.alerts.touch = Some(Touch::Pull {
                            start_y,
                            start_edge: self.alerts.centre.edge(now),
                            moved: true,
                            tracker,
                        });
                        self.alerts.centre.drag =
                            Some((self.alerts.centre.edge(now) + dy).clamp(0.0, SCREEN_H));
                        self.redraw = true;
                        return;
                    }
                    self.alerts.centre.swipe = Some(Swipe {
                        id: id.clone(),
                        rect: *rect,
                        offset: 0.0,
                        fly: None,
                    });
                }
                if *horizontal == Some(true)
                    && let Some(swipe) = self.alerts.centre.swipe.as_mut()
                {
                    swipe.offset = dx;
                }
            }
        }
        self.redraw = true;
    }

    pub fn alerts_pointer_end(&mut self, x: f32, y: f32) {
        let now = Instant::now();
        let Some(touch) = self.alerts.touch.take() else {
            return;
        };
        match touch {
            Touch::Ring(button) => {
                self.alerts.ring_pressed = None;
                if let Some(button) = button.filter(|b| ring_button_at(x, y) == Some(*b))
                    && let Some(ring) = self.alerts.ringing().cloned()
                {
                    let action = match button {
                        RingButton::Primary => match ring.kind {
                            RingKind::Alarm => RingAction::Again,
                            RingKind::Timer => RingAction::Stop,
                        },
                        RingButton::Secondary => match ring.kind {
                            RingKind::Alarm => RingAction::Stop,
                            RingKind::Timer => RingAction::Again,
                        },
                    };
                    self.ring_action(&ring, action);
                }
            }
            Touch::Banner {
                start, mut tracker, ..
            } => {
                tracker.push(now, x, y);
                let (dx, dy) = (x - start.0, y - start.1);
                let velocity = tracker.velocity(now).1;
                let Some(banner) = self.alerts.banner.as_mut() else {
                    return;
                };
                if dx.abs() <= TAP_SLOP && dy.abs() <= TAP_SLOP {
                    banner.drag = None;
                    let id = banner.id.clone();
                    self.activate_notification(&id);
                } else if dy < -40.0 || velocity < -500.0 {
                    // Swiped up: the banner goes; the notification stays in
                    // the centre, as on iPhone.
                    banner.hide(now, velocity.min(0.0));
                    self.last_action = "banner_hidden".into();
                } else {
                    let from = banner.y(now) - BANNER_TOP;
                    banner.drag = None;
                    banner.offset = Spring::new(from, 0.0, velocity, now, 0.35, 0.8);
                    banner.hide_at = Some(now + BANNER_SHOW_FOR);
                }
            }
            Touch::Pull {
                start_edge,
                moved,
                mut tracker,
                ..
            } => {
                tracker.push(now, x, y);
                if !moved {
                    // A tap: on the open centre's controls, or nothing.
                    if start_edge > 0.0 {
                        self.centre_tap(x, y);
                    }
                    self.alerts.centre.drag = None;
                    return;
                }
                let velocity = tracker.velocity(now).1;
                let edge = self.alerts.centre.edge(now);
                let open = if velocity.abs() > 400.0 {
                    velocity > 0.0
                } else {
                    edge > SCREEN_H * if start_edge > 0.0 { 0.7 } else { 0.3 }
                };
                let was_open = self.alerts.centre.open;
                self.alerts.centre.settle(open, velocity, now);
                if open && !was_open {
                    self.centre_opened();
                }
                self.last_action = if open {
                    "notification_centre_open"
                } else {
                    "notification_centre_closed"
                }
                .into();
            }
            Touch::CentreCard {
                id,
                start,
                horizontal,
                mut tracker,
                ..
            } => {
                tracker.push(now, x, y);
                match horizontal {
                    None => {
                        // A tap on a card opens it.
                        self.alerts.centre.settle(false, 0.0, now);
                        self.activate_notification(&id);
                    }
                    Some(_) => {
                        let dx = x - start.0;
                        let velocity = tracker.velocity(now).0;
                        if let Some(swipe) = self.alerts.centre.swipe.as_mut() {
                            let out = dx.abs() > 320.0 || velocity.abs() > 900.0;
                            let direction = if dx + velocity * 0.1 >= 0.0 {
                                1.0
                            } else {
                                -1.0
                            };
                            let target = if out {
                                direction * (SCREEN_W + 60.0)
                            } else {
                                0.0
                            };
                            swipe.fly = Some(Spring::new(dx, target, velocity, now, 0.28, 1.0));
                        }
                    }
                }
            }
        }
        self.redraw = true;
    }

    fn centre_tap(&mut self, x: f32, y: f32) {
        let layout = centre_layout(&self.alerts.store, None, unix_now());
        if layout.clear.contains(x, y) && !self.alerts.store.notifications.is_empty() {
            if self.alerts.centre.clear_armed {
                self.alerts.outbox.push(Outgoing::DismissAll);
                self.alerts.store.notifications.clear();
                self.alerts.centre.clear_armed = false;
                self.last_action = "notifications_cleared".into();
            } else {
                self.alerts.centre.clear_armed = true;
            }
        } else {
            self.alerts.centre.clear_armed = false;
        }
    }

    /// The centre came down: everything in it has been seen.
    fn centre_opened(&mut self) {
        let unseen: Vec<String> = self
            .alerts
            .store
            .notifications
            .iter()
            .filter(|n| !n.seen)
            .map(|n| n.id.clone())
            .collect();
        if !unseen.is_empty() {
            self.alerts.outbox.push(Outgoing::MarkSeen(unseen));
        }
        if let Some(banner) = self.alerts.banner.as_mut() {
            banner.hide(Instant::now(), 0.0);
        }
    }

    /// A tap on the lock screen's clock face: on a notification, remember
    /// it for after unlock. Returns true when it hit one.
    pub fn lock_list_tap(&mut self, x: f32, y: f32) -> bool {
        let unseen = self.alerts.unseen();
        let layout = lock_list_layout(&unseen);
        let hit = layout
            .iter()
            .find(|(_, rect)| rect.contains(x, y - LOCK_LIST_TOP))
            .map(|(id, _)| id.clone());
        match hit {
            Some(id) => {
                self.alerts.open_after_unlock = Some(id);
                true
            }
            None => false,
        }
    }

    fn banner_height(&self) -> f32 {
        self.alerts
            .banner
            .as_ref()
            .and_then(|b| self.alerts.store.notification(&b.id))
            .map_or(160.0, card_height)
    }
}

pub fn banner_rect(y: f32, height: f32) -> RectF {
    RectF::new(CARD_X, y, CARD_W, height)
}

pub fn ring_button_at(x: f32, y: f32) -> Option<RingButton> {
    if RING_PRIMARY.contains(x, y) {
        Some(RingButton::Primary)
    } else if RING_SECONDARY.contains(x, y) {
        Some(RingButton::Secondary)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Layout

/// Up to two lines of body text.
fn body_lines(notification: &Notification) -> Vec<String> {
    notification
        .body
        .as_deref()
        .map(|body| {
            wrap(
                body,
                Weight::Regular,
                BODY_SIZE,
                CARD_W - TEXT_X - CARD_PAD,
                2,
            )
        })
        .unwrap_or_default()
}

pub fn card_height(notification: &Notification) -> f32 {
    let lines = body_lines(notification).len() as f32;
    CARD_PAD * 2.0 + 50.0 + LINE * lines
}

/// Word-wraps `text` into at most `max_lines`, the last one ellipsized.
pub fn wrap(text: &str, weight: Weight, size: f32, width: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                word.to_owned()
            } else {
                format!("{line} {word}")
            };
            if text::measure(&candidate, weight, size) <= width || line.is_empty() {
                line = candidate;
            } else {
                lines.push(std::mem::replace(&mut line, word.to_owned()));
            }
        }
        lines.push(line);
    }
    lines.retain(|line| !line.is_empty());
    if lines.len() > max_lines {
        let rest = lines[max_lines - 1..].join(" ");
        lines.truncate(max_lines - 1);
        lines.push(rest);
    }
    lines
        .into_iter()
        .map(|line| text::fit(&line, weight, size, width))
        .collect()
}

/// "now", "5m ago", "2h ago", or the clock time for older ones.
pub fn relative_time(posted_at: i64, now: i64) -> String {
    let age = (now - posted_at).max(0);
    match age {
        0..60 => "now".into(),
        60..3600 => format!("{}m ago", age / 60),
        3600..43_200 => format!("{}h ago", age / 3600),
        _ => LocalTime::at(posted_at).time_text(false),
    }
}

/// Lock-screen cards (in list-layer coordinates), newest first, and the
/// summary pill when more are waiting.
pub fn lock_list_layout(unseen: &[&Notification]) -> Vec<(String, RectF)> {
    let mut y = 0.0;
    let mut cards = Vec::new();
    for notification in unseen.iter().take(LOCK_LIST_CARDS) {
        let height = card_height(notification);
        cards.push((
            notification.id.clone(),
            RectF::new(CARD_X, y, CARD_W, height),
        ));
        y += height + CARD_GAP;
    }
    cards
}

#[derive(Debug)]
pub struct CentreLayout {
    pub clear: RectF,
    pub groups: Vec<CentreGroup>,
    /// Notifications that did not fit on screen.
    pub hidden: usize,
}

#[derive(Debug)]
pub struct CentreGroup {
    pub app: String,
    pub header_y: f32,
    pub cards: Vec<(String, RectF)>,
    /// Cards of this group beyond the ones shown.
    pub more: usize,
    pub more_y: f32,
}

impl CentreLayout {
    pub fn card_at(&self, x: f32, y: f32) -> Option<(String, RectF)> {
        self.groups
            .iter()
            .flat_map(|group| group.cards.iter())
            .find(|(_, rect)| rect.contains(x, y))
            .cloned()
    }
}

/// The centre: notifications grouped by app (the app with the newest one
/// first), each group's newest cards and a count of the rest.
pub fn centre_layout(store: &NotifyStore, skip: Option<&str>, _now: i64) -> CentreLayout {
    let mut layout = CentreLayout {
        clear: RectF::new(SCREEN_W - 24.0 - 150.0, CENTRE_TOP - 108.0, 150.0, 96.0),
        groups: Vec::new(),
        hidden: 0,
    };
    let mut apps: Vec<&str> = Vec::new();
    for notification in &store.notifications {
        if !apps.contains(&notification.app.as_str()) {
            apps.push(&notification.app);
        }
    }
    let mut y = CENTRE_TOP + 24.0;
    for app in apps {
        let members: Vec<&Notification> = store
            .notifications
            .iter()
            .filter(|n| n.app == app && Some(n.id.as_str()) != skip)
            .collect();
        let all = store.notifications.iter().filter(|n| n.app == app).count();
        if y + 70.0 + 160.0 > CENTRE_BOTTOM {
            layout.hidden += all;
            continue;
        }
        let mut group = CentreGroup {
            app: app.to_owned(),
            header_y: y,
            cards: Vec::new(),
            more: 0,
            more_y: 0.0,
        };
        y += 76.0;
        for (index, notification) in store
            .notifications
            .iter()
            .filter(|n| n.app == app)
            .enumerate()
        {
            let height = card_height(notification);
            if index >= CENTRE_GROUP_CARDS || y + height > CENTRE_BOTTOM {
                group.more = all - index;
                break;
            }
            group.cards.push((
                notification.id.clone(),
                RectF::new(CARD_X, y, CARD_W, height),
            ));
            y += height + CARD_GAP;
        }
        let _ = members;
        if group.more > 0 {
            group.more_y = y;
            y += 70.0;
        }
        y += 30.0;
        layout.groups.push(group);
    }
    layout
}

// ---------------------------------------------------------------------------
// Drawing

/// App icons by app id (`system`, `browser`); others get a generic glyph.
pub trait Icons {
    fn icon(&self, app: &str) -> Option<&Image>;
}

pub fn app_display_name(app: &str) -> String {
    match app {
        "system" => "System".into(),
        "browser" => "Browser".into(),
        "moon" => "Moon".into(),
        "moon-notifyd" => "Clock".into(),
        other => {
            let mut name = other.replace('-', " ");
            if let Some(first) = name.get_mut(0..1) {
                first.make_ascii_uppercase();
            }
            name
        }
    }
}

fn draw_app_icon(canvas: &mut Canvas, icons: &dyn Icons, app: &str, rect: RectF) {
    match icons.icon(app) {
        Some(image) => canvas.draw_image_scaled(image, rect, 1.0),
        None => {
            canvas.fill_rrect(rect, rect.w * 0.23, 0x636366, 1.0);
            if app == "moon-notifyd" {
                draw_clock_glyph(canvas, rect.center(), rect.w * 0.3, WHITE);
            } else {
                draw_bell_glyph(canvas, rect.center(), rect.w * 0.3, WHITE);
            }
        }
    }
}

/// A notification card. `opaque` draws a solid card (the banner window over
/// an app cannot blend with what is beneath it).
pub fn draw_card(
    canvas: &mut Canvas,
    rect: RectF,
    notification: &Notification,
    now: i64,
    icons: &dyn Icons,
    opaque: bool,
) {
    if !opaque {
        canvas.shadow_rrect(rect, CARD_RADIUS, 28.0, 0x000000, 0.28);
    }
    canvas.fill_rrect(
        rect,
        CARD_RADIUS,
        CARD_RAISED,
        if opaque { 1.0 } else { 0.9 },
    );
    let icon = RectF::new(rect.x + CARD_PAD, rect.y + CARD_PAD, ICON, ICON);
    draw_app_icon(canvas, icons, &notification.app, icon);
    let x = rect.x + TEXT_X;
    let right = rect.x + rect.w - CARD_PAD;
    let title_baseline = rect.y + CARD_PAD + 44.0;
    let time = relative_time(notification.posted_at, now);
    let time_width = text::measure(&time, Weight::Regular, 36.0);
    text::draw_right(
        canvas,
        &time,
        right,
        title_baseline,
        Weight::Regular,
        36.0,
        SECONDARY,
        SECONDARY_ALPHA,
    );
    // Messages and calls are titled by the number: shown as the call
    // screen and the apps show it.
    let title = match notification.app.as_str() {
        "messages" | "phone" => crate::calls::format_number(&notification.title),
        _ => notification.title.clone(),
    };
    let title = text::fit(
        &title,
        Weight::SemiBold,
        TITLE_SIZE,
        right - x - time_width - 24.0,
    );
    text::draw(
        canvas,
        &title,
        x,
        title_baseline,
        Weight::SemiBold,
        TITLE_SIZE,
        WHITE,
        1.0,
    );
    for (index, line) in body_lines(notification).iter().enumerate() {
        text::draw(
            canvas,
            line,
            x,
            title_baseline + LINE * (index as f32 + 1.0),
            Weight::Regular,
            BODY_SIZE,
            WHITE,
            0.86,
        );
    }
}

/// The lock-screen list: unseen notifications, newest first, and a pill
/// counting the rest.
pub fn draw_lock_list(canvas: &mut Canvas, unseen: &[&Notification], now: i64, icons: &dyn Icons) {
    canvas.clear(0);
    let layout = lock_list_layout(unseen);
    for (id, rect) in &layout {
        if let Some(notification) = unseen.iter().find(|n| &n.id == id) {
            draw_card(canvas, *rect, notification, now, icons, false);
        }
    }
    let rest = unseen.len().saturating_sub(layout.len());
    if rest > 0 {
        let y = layout.last().map_or(0.0, |(_, r)| r.y + r.h + CARD_GAP);
        let label = format!(
            "{rest} more notification{}",
            if rest == 1 { "" } else { "s" }
        );
        let width = text::measure(&label, Weight::SemiBold, 38.0) + 72.0;
        let pill = RectF::new((SCREEN_W - width) / 2.0, y, width, 76.0);
        canvas.fill_rrect(pill, 38.0, CARD_RAISED, 0.85);
        text::draw_centered(
            canvas,
            &label,
            SCREEN_W / 2.0,
            text::centered_baseline(y + 38.0, Weight::SemiBold, 38.0),
            Weight::SemiBold,
            38.0,
            WHITE,
            0.9,
        );
    }
}

/// The centre's content (cards, headers, status bar), drawn transparent
/// over its darkened wallpaper. `skip` leaves out a card being swiped.
#[allow(clippy::too_many_arguments)]
pub fn draw_centre(
    canvas: &mut Canvas,
    store: &NotifyStore,
    skip: Option<&str>,
    clear_armed: bool,
    now: i64,
    time_text: &str,
    icons: &dyn Icons,
) {
    canvas.clear(0);
    let date = LocalTime::at(now).date_text();
    text::draw(
        canvas,
        &date,
        48.0,
        CENTRE_TOP - 150.0,
        Weight::SemiBold,
        40.0,
        SECONDARY,
        SECONDARY_ALPHA,
    );
    text::draw(
        canvas,
        "Notifications",
        48.0,
        CENTRE_TOP - 50.0,
        Weight::Bold,
        84.0,
        WHITE,
        1.0,
    );
    let _ = time_text;
    let layout = centre_layout(store, skip, now);
    if store.notifications.is_empty() {
        text::draw_centered(
            canvas,
            "No Notifications",
            SCREEN_W / 2.0,
            1140.0,
            Weight::SemiBold,
            52.0,
            SECONDARY,
            SECONDARY_ALPHA,
        );
        return;
    }
    // Clear: a round × that becomes a "Clear" pill on the first tap.
    let clear = layout.clear;
    if clear_armed {
        canvas.fill_rrect(clear, clear.h / 2.0, CARD_RAISED, 1.0);
        text::draw_centered(
            canvas,
            "Clear",
            clear.x + clear.w / 2.0,
            text::centered_baseline(clear.y + clear.h / 2.0, Weight::SemiBold, 38.0),
            Weight::SemiBold,
            38.0,
            WHITE,
            1.0,
        );
    } else {
        let (cx, cy) = (clear.x + clear.w - 48.0, clear.y + clear.h / 2.0);
        canvas.fill_circle(cx, cy, 34.0, CARD_RAISED, 1.0);
        draw_cross(canvas, (cx, cy), 13.0, SECONDARY);
    }
    for group in &layout.groups {
        let icon = RectF::new(CARD_X + 12.0, group.header_y + 8.0, 52.0, 52.0);
        draw_app_icon(canvas, icons, &group.app, icon);
        text::draw(
            canvas,
            &app_display_name(&group.app),
            icon.x + icon.w + 22.0,
            group.header_y + 48.0,
            Weight::SemiBold,
            40.0,
            WHITE,
            0.9,
        );
        for (id, rect) in &group.cards {
            if Some(id.as_str()) == skip {
                continue;
            }
            if let Some(notification) = store.notification(id) {
                draw_card(canvas, *rect, notification, now, icons, false);
            }
        }
        if group.more > 0 {
            text::draw(
                canvas,
                &format!("{} more", group.more),
                CARD_X + 36.0,
                group.more_y + 46.0,
                Weight::Medium,
                38.0,
                SECONDARY,
                SECONDARY_ALPHA,
            );
        }
    }
    if layout.hidden > 0 {
        text::draw_centered(
            canvas,
            &format!("{} more notifications", layout.hidden),
            SCREEN_W / 2.0,
            CENTRE_BOTTOM + 60.0,
            Weight::Medium,
            38.0,
            SECONDARY,
            SECONDARY_ALPHA,
        );
    }
}

/// What the ringing screen shows.
#[derive(Debug, Clone, PartialEq)]
pub struct RingInfo {
    pub kind: RingKind,
    pub label: String,
    /// The alarm's set time ("7:30"), or the timer's length ("10 min").
    pub detail: String,
}

impl RingInfo {
    pub fn from_store(store: &NotifyStore, ring: &Ringing) -> Self {
        match ring.kind {
            RingKind::Alarm => {
                let alarm = store.alarms.iter().find(|a| a.id == ring.id);
                Self {
                    kind: ring.kind,
                    label: alarm
                        .map(|a| a.label.clone())
                        .filter(|l| !l.is_empty())
                        .unwrap_or_else(|| "Alarm".into()),
                    detail: String::new(),
                }
            }
            RingKind::Timer => {
                let timer = store.timers.iter().find(|t| t.id == ring.id);
                Self {
                    kind: ring.kind,
                    label: timer
                        .map(|t| t.label.clone())
                        .filter(|l| !l.is_empty())
                        .unwrap_or_else(|| "Timer".into()),
                    detail: timer.map(timer_length).unwrap_or_default(),
                }
            }
        }
    }
}

fn timer_length(timer: &Timer) -> String {
    let total = timer.duration_s;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    match (h, m, s) {
        (0, 0, s) => format!("{s} sec timer"),
        (0, m, 0) => format!("{m} min timer"),
        (0, m, s) => format!("{m} min {s} sec timer"),
        (h, 0, _) => format!("{h} hr timer"),
        (h, m, _) => format!("{h} hr {m} min timer"),
    }
}

/// The full-screen alarm or timer, drawn over its darkened wallpaper.
pub fn draw_ring_screen(
    canvas: &mut Canvas,
    info: &RingInfo,
    time_text: &str,
    pressed: Option<RingButton>,
) {
    canvas.clear(0);
    let cx = SCREEN_W / 2.0;
    match info.kind {
        RingKind::Alarm => {
            draw_clock_glyph(canvas, (cx, 360.0), 46.0, ORANGE);
            text::draw_centered(
                canvas,
                &info.label,
                cx,
                520.0,
                Weight::SemiBold,
                60.0,
                WHITE,
                0.92,
            );
            text::draw_centered(
                canvas,
                time_text,
                cx,
                820.0,
                Weight::DisplaySemiBold,
                270.0,
                WHITE,
                0.96,
            );
        }
        RingKind::Timer => {
            draw_timer_glyph(canvas, (cx, 380.0), 52.0, ORANGE);
            text::draw_centered(
                canvas,
                &info.label,
                cx,
                560.0,
                Weight::SemiBold,
                60.0,
                WHITE,
                0.92,
            );
            text::draw_centered(
                canvas,
                "Time's up",
                cx,
                800.0,
                Weight::DisplaySemiBold,
                150.0,
                WHITE,
                0.96,
            );
            text::draw_centered(
                canvas,
                &info.detail,
                cx,
                900.0,
                Weight::Regular,
                46.0,
                SECONDARY,
                SECONDARY_ALPHA,
            );
        }
    }
    let (primary, secondary) = match info.kind {
        RingKind::Alarm => ("Snooze", "Stop"),
        RingKind::Timer => ("Stop", "Repeat"),
    };
    for (rect, label, button, color) in [
        (RING_PRIMARY, primary, RingButton::Primary, ORANGE),
        (RING_SECONDARY, secondary, RingButton::Secondary, 0x3a3a3c),
    ] {
        let alpha = if pressed == Some(button) { 0.6 } else { 1.0 };
        canvas.fill_rrect(rect, rect.h / 2.0, color, alpha);
        text::draw_centered(
            canvas,
            label,
            rect.x + rect.w / 2.0,
            text::centered_baseline(rect.y + rect.h / 2.0, Weight::SemiBold, 62.0),
            Weight::SemiBold,
            62.0,
            if button == RingButton::Primary {
                0x000000
            } else {
                WHITE
            },
            1.0,
        );
    }
}

fn draw_cross(canvas: &mut Canvas, (cx, cy): (f32, f32), r: f32, color: u32) {
    let mut path = crate::gfx::Path::new();
    path.stroke_segment((cx - r, cy - r), (cx + r, cy + r), 6.0);
    path.stroke_segment((cx - r, cy + r), (cx + r, cy - r), 6.0);
    canvas.fill_path(&path, color, 0.9);
}

/// An alarm clock: a ring with hands and two bells.
pub fn draw_clock_glyph(canvas: &mut Canvas, (cx, cy): (f32, f32), r: f32, color: u32) {
    canvas.stroke_circle(cx, cy, r, r * 0.16, color, 1.0);
    let mut path = crate::gfx::Path::new();
    path.stroke_segment((cx, cy), (cx, cy - r * 0.55), r * 0.13);
    path.stroke_segment((cx, cy), (cx + r * 0.4, cy + r * 0.1), r * 0.13);
    canvas.fill_path(&path, color, 1.0);
    for side in [-1.0, 1.0] {
        canvas.fill_circle(cx + side * r * 0.78, cy - r * 0.86, r * 0.3, color, 1.0);
    }
}

fn draw_timer_glyph(canvas: &mut Canvas, (cx, cy): (f32, f32), r: f32, color: u32) {
    canvas.stroke_circle(cx, cy, r, r * 0.14, color, 1.0);
    canvas.fill_rrect(
        RectF::new(cx - r * 0.18, cy - r * 1.42, r * 0.36, r * 0.3),
        r * 0.08,
        color,
        1.0,
    );
    let mut path = crate::gfx::Path::new();
    path.stroke_segment((cx, cy), (cx + r * 0.42, cy - r * 0.42), r * 0.13);
    canvas.fill_path(&path, color, 1.0);
}

fn draw_bell_glyph(canvas: &mut Canvas, (cx, cy): (f32, f32), r: f32, color: u32) {
    let mut path = crate::gfx::Path::new();
    path.move_to(cx - r, cy + r * 0.6);
    path.line_to(cx - r * 0.75, cy + r * 0.3);
    path.line_to(cx - r * 0.75, cy - r * 0.2);
    path.arc(
        cx,
        cy - r * 0.2,
        r * 0.75,
        std::f32::consts::PI,
        2.0 * std::f32::consts::PI,
    );
    path.line_to(cx + r * 0.75, cy + r * 0.3);
    path.line_to(cx + r, cy + r * 0.6);
    path.close();
    canvas.fill_path(&path, color, 1.0);
    canvas.fill_circle(cx, cy + r * 0.85, r * 0.22, color, 1.0);
}

/// A small alarm clock for the status bar.
pub fn draw_status_alarm(canvas: &mut Canvas, (cx, cy): (f32, f32), color: u32) {
    draw_clock_glyph(canvas, (cx, cy + 2.0), 15.0, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notification(id: &str, app: &str, posted_at: i64, body: Option<&str>) -> Notification {
        Notification {
            id: id.into(),
            app: app.into(),
            tag: None,
            title: format!("Title {id}"),
            body: body.map(Into::into),
            urgency: "normal".into(),
            posted_at,
            seen: false,
            open: None,
            ringing: false,
        }
    }

    #[test]
    fn wrapping_keeps_two_lines_and_ellipsizes_the_rest() {
        let long = "The quick brown fox jumps over the lazy dog and keeps on running through the field until the end of the day";
        let lines = wrap(long, Weight::Regular, BODY_SIZE, 700.0, 2);
        assert_eq!(lines.len(), 2);
        assert!(lines[1].ends_with('…'));
        assert!(
            lines
                .iter()
                .all(|l| text::measure(l, Weight::Regular, BODY_SIZE) <= 700.0)
        );
        assert_eq!(
            wrap("short", Weight::Regular, BODY_SIZE, 700.0, 2),
            vec!["short"]
        );
    }

    #[test]
    fn relative_times_read_like_ios() {
        assert_eq!(relative_time(100, 130), "now");
        assert_eq!(relative_time(100, 100 + 5 * 60), "5m ago");
        assert_eq!(relative_time(100, 100 + 2 * 3600), "2h ago");
    }

    #[test]
    fn the_centre_groups_by_app_newest_first() {
        let store = NotifyStore {
            notifications: vec![
                notification("n5", "browser", 50, None),
                notification("n4", "system", 40, Some("body")),
                notification("n3", "browser", 30, None),
                notification("n2", "browser", 20, None),
                notification("n1", "browser", 10, None),
            ],
            ..NotifyStore::default()
        };
        let layout = centre_layout(&store, None, 60);
        assert_eq!(layout.groups[0].app, "browser");
        assert_eq!(layout.groups[0].cards.len(), CENTRE_GROUP_CARDS);
        assert_eq!(layout.groups[0].more, 1);
        assert_eq!(layout.groups[1].app, "system");
        let (id, rect) = &layout.groups[1].cards[0];
        assert_eq!(id, "n4");
        assert_eq!(
            layout
                .card_at(rect.x + 10.0, rect.y + 10.0)
                .map(|(id, _)| id),
            Some("n4".into())
        );
    }

    #[test]
    fn banners_hide_on_their_own_and_a_swipe_up_hides_them() {
        let mut state = ShellState::new(false, 50);
        state.notify_event(Event::NotificationPosted {
            notification: notification("n1", "system", unix_now(), Some("Connected")),
            wake: false,
        });
        assert!(state.alerts.banner.is_some());
        let now = Instant::now();
        let y = state
            .alerts
            .banner
            .as_ref()
            .unwrap()
            .y(now + Duration::from_secs(1));
        assert!((y - BANNER_TOP).abs() < 2.0, "{y}");
        // A swipe up on it hides it but keeps the notification.
        let start = BANNER_TOP + 80.0;
        state.alerts.banner = Some(Banner::new(
            "n1".into(),
            now - Duration::from_secs(2),
            false,
        ));
        assert!(state.alerts_pointer_begin(540.0, start));
        state.alerts_pointer_move(540.0, start - 120.0);
        state.alerts_pointer_end(540.0, start - 160.0);
        assert!(state.alerts.banner.as_ref().unwrap().hiding);
        assert_eq!(state.alerts.store.notifications.len(), 1);
        assert!(state.alerts.outbox.is_empty());
    }

    #[test]
    fn a_banner_tap_activates_and_opens_its_app() {
        let mut state = ShellState::new(false, 50);
        let mut n = notification("n1", "system", unix_now(), None);
        n.open = Some(crate::notify_client::OpenAction {
            app: "system".into(),
            target: None,
        });
        state.notify_event(Event::NotificationPosted {
            notification: n,
            wake: false,
        });
        let now = Instant::now() - Duration::from_secs(2);
        state.alerts.banner = Some(Banner::new("n1".into(), now, false));
        assert!(state.alerts_pointer_begin(540.0, BANNER_TOP + 60.0));
        state.alerts_pointer_end(542.0, BANNER_TOP + 62.0);
        assert_eq!(state.alerts.outbox, vec![Outgoing::Activate("n1".into())]);
        assert_eq!(state.view, View::System);
    }

    #[test]
    fn ringing_wakes_and_stop_snooze_go_to_the_daemon() {
        let mut state = ShellState::new(true, 50);
        state.screen_off();
        state.notify_event(Event::AlarmRinging {
            alarm: crate::notify_client::Alarm {
                id: "a1".into(),
                hour: 7,
                minute: 0,
                days: vec![],
                label: "Up".into(),
                enabled: true,
                next_fire_at: None,
                snoozed_until: None,
                snooze_minutes: 9,
            },
        });
        assert!(state.alerts.pending_wake);
        assert!(state.alerts.ringing().is_some());
        // Snooze is the big button.
        let (x, y) = RING_PRIMARY.center();
        assert!(state.alerts_pointer_begin(x, y));
        state.alerts_pointer_end(x, y);
        assert_eq!(
            state.alerts.outbox,
            vec![Outgoing::Ring(
                RingKind::Alarm,
                "a1".into(),
                RingAction::Again
            )]
        );
        assert!(state.alerts.ringing().is_none());
    }

    #[test]
    fn pulling_down_from_the_top_opens_the_centre_and_marks_seen() {
        let mut state = ShellState::new(false, 50);
        state.notify_event(Event::Snapshot {
            notifications: vec![notification("n1", "system", 10, None)],
            alarms: vec![],
            timers: vec![],
            ringing: vec![],
            settings: None,
        });
        assert!(state.alerts_pointer_begin(540.0, 40.0));
        for y in [120.0, 400.0, 900.0, 1400.0] {
            state.alerts_pointer_move(540.0, y);
        }
        state.alerts_pointer_end(540.0, 1450.0);
        assert!(state.alerts.centre.open);
        assert_eq!(
            state.alerts.outbox,
            vec![Outgoing::MarkSeen(vec!["n1".into()])]
        );
        // Clear needs two taps.
        let (x, y) = centre_layout(&state.alerts.store, None, 0).clear.center();
        for _ in 0..2 {
            assert!(state.alerts_pointer_begin(x, y));
            state.alerts_pointer_end(x, y);
        }
        assert_eq!(state.alerts.outbox.last(), Some(&Outgoing::DismissAll));
        assert!(state.alerts.store.notifications.is_empty());
    }
}
