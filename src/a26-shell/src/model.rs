use serde::Serialize;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

use crate::config::Config;
use crate::freezer::FreezerState;
use crate::gfx::RectF;
use crate::interaction::{
    CARD_RADIUS, CardMotion, DISPLAY_RADIUS, Fade, GestureOutcome, HomeGesture, LockMotion, SCREEN,
    Switcher, SwitcherAction,
};
use crate::keyboard::{
    KeyAction, KeyboardEffect, KeyboardPurpose, KeyboardState, PublicKeyboardState,
};
use crate::lease::PublicLeaseState;
use crate::suspend::PublicSuspendState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Locked,
    Launcher,
    System,
    Browser,
    Phone,
    Messages,
    Exocortex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppId {
    System,
    Browser,
    Phone,
    Messages,
    Exocortex,
}

impl AppId {
    pub const COUNT: usize = 5;
    pub const ALL: [Self; Self::COUNT] = [
        Self::System,
        Self::Browser,
        Self::Phone,
        Self::Messages,
        Self::Exocortex,
    ];

    pub fn from_view(view: View) -> Option<Self> {
        match view {
            View::System => Some(Self::System),
            View::Browser => Some(Self::Browser),
            View::Phone => Some(Self::Phone),
            View::Messages => Some(Self::Messages),
            View::Exocortex => Some(Self::Exocortex),
            View::Locked | View::Launcher => None,
        }
    }

    pub fn view(self) -> View {
        match self {
            Self::System => View::System,
            Self::Browser => View::Browser,
            Self::Phone => View::Phone,
            Self::Messages => View::Messages,
            Self::Exocortex => View::Exocortex,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Browser => "Browser",
            Self::Phone => "Phone",
            Self::Messages => "Messages",
            Self::Exocortex => "Exocortex",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|app| app.cgroup_name() == value)
    }

    pub fn index(self) -> usize {
        self as usize
    }

    /// Whose leases are held for work outside Moon: the Exocortex daemon's
    /// turns (its bridge holds a transfer lease while any streams). Such a
    /// lease only keeps the phone out of deep suspend; it never thaws, keeps
    /// or outlives-with the app's own process, which may not even run.
    pub fn lease_is_external(self) -> bool {
        matches!(self, Self::Exocortex)
    }

    /// The app's id (`ctl`, notifications, its freezer cgroup).
    pub fn cgroup_name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Browser => "browser",
            Self::Phone => "phone",
            Self::Messages => "messages",
            Self::Exocortex => "exocortex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppLifecycle {
    Stopped,
    Launching,
    Foreground,
    Background,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicAppState {
    pub app: AppId,
    pub lifecycle: AppLifecycle,
    pub pid: Option<u32>,
    pub windows: Vec<u32>,
    pub freezer_cgroup: Option<String>,
    pub freezer_state: FreezerState,
    pub leases: Vec<PublicLeaseState>,
    pub last_used_ms_ago: u64,
    /// Started as a Wayland client of Moon (`wayland_apps`), not on X.
    pub wayland: bool,
}

impl View {
    pub fn is_app(self) -> bool {
        AppId::from_view(self).is_some()
    }
}

/// The two faces of the lock screen: the clock (shown on wake) and the
/// passcode keypad (revealed by a tap, an upward swipe, or a typed digit).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LockFace {
    Clock,
    Passcode,
}

/// A control currently held down, drawn highlighted until release (the
/// home screen's icons are home.rs's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    Keypad(crate::ui::KeypadAction),
}

/// An app opening: its card grows from where it was tapped (a dock icon or
/// a switcher card) to full screen.
#[derive(Debug, Clone, Copy)]
pub struct Launch {
    pub app: AppId,
    pub started: Instant,
    pub card: CardMotion,
    /// A background app being brought back: its window already exists.
    pub resumed: bool,
    pub window_mapped_at: Option<Instant>,
}

/// Wrong-passcode shake.
pub const SHAKE_ANIMATION: Duration = Duration::from_millis(480);
/// The passcode keypad returns to the clock after this much inactivity.
pub const PASSCODE_IDLE_TIMEOUT: Duration = Duration::from_secs(12);
/// Vimbrowser's first frames are painted by a software compositor; keep the
/// launch screen up briefly after its window maps so no blank frame shows.
const BROWSER_FIRST_PAINT_DELAY: Duration = Duration::from_millis(1500);
const SYSTEM_FIRST_PAINT_DELAY: Duration = Duration::from_millis(80);
/// Cross-fade from the passcode keypad to the home screen after unlock.
pub const UNLOCK_ANIMATION: Duration = Duration::from_millis(260);
/// How far a touch in an app's bottom zone must rise before it becomes the
/// interactive home gesture (taps there still reach the app).
pub const HOME_GESTURE_SLOP: f32 = 22.0;
/// The bottom zone where the home gesture starts.
pub const HOME_GESTURE_ZONE: f32 = 180.0;

#[derive(Debug, Clone, Copy)]
pub struct PointerGesture {
    pub start_x: i16,
    pub start_y: i16,
    pub last_x: i16,
    pub last_y: i16,
    pub started: Instant,
    /// Last begin/move of this contact; the lost-release watchdog counts
    /// from here, so a long drag is never cancelled while it moves.
    pub last_activity: Instant,
    pub keyboard_owned: bool,
    pub keyboard_key_index: Option<usize>,
    pub keyboard_pressed: bool,
    pub keyboard_initial_sent: bool,
    pub keyboard_repeat_uppercase: bool,
    pub keyboard_next_repeat_at: Option<Instant>,
    /// The touch belongs to the app in front (see `AppPointer`).
    pub forwarding: bool,
}

/// An app page push or pop (System, Phone, Messages; GPU mode): the
/// outgoing page, snapshotted, slides away while the live app slides in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageTransition {
    pub push: bool,
    /// When the slide began, or (while `waiting`) when it was asked for.
    pub started: Instant,
    /// The old page holds still until the app says its new page is fully
    /// drawn (`transition ready`), so the slide never shows it half drawn.
    pub waiting: bool,
}

pub const PAGE_TRANSITION: Duration = Duration::from_millis(380);
/// The slide starts anyway if the app never says it is ready.
pub const PAGE_READY_WAIT: Duration = Duration::from_millis(150);

impl PageTransition {
    pub fn new(push: bool, now: Instant) -> Self {
        Self {
            push,
            started: now,
            waiting: true,
        }
    }

    /// The new page is drawn: slide.
    pub fn ready(&mut self, now: Instant) {
        if self.waiting {
            self.waiting = false;
            self.started = now;
        }
    }

    /// Eased progress, 0 to 1.
    pub fn progress(&self, now: Instant) -> f32 {
        if self.waiting {
            return 0.0;
        }
        crate::gfx::ease_out(
            now.saturating_duration_since(self.started).as_secs_f32()
                / PAGE_TRANSITION.as_secs_f32(),
        )
    }
}

/// What a tapped notification asked its app to show (`open.target`):
/// handed over in Moon's state, where the app looks when it comes forward.
#[derive(Debug, Clone)]
pub struct OpenTarget {
    pub app: AppId,
    pub target: String,
    pub serial: u64,
    pub at: Instant,
}

impl OpenTarget {
    pub fn new(app: AppId, target: String) -> Self {
        static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        Self {
            app,
            target,
            serial: SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            at: Instant::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicOpenTarget {
    pub app: AppId,
    pub target: String,
    pub serial: u64,
    pub age_ms: u64,
}

/// Pointer events for the app in front, sent through XTEST by the main
/// loop (to a Wayland app as wl_touch). Moon's own apps (System, Phone, Messages, Exocortex) get each touch as a held
/// button from touch-down to release; the Browser gets taps on release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppPointer {
    Press(i16, i16),
    Motion(i16, i16),
    Release(i16, i16),
}

impl View {
    /// Moon's own apps take drags (the Browser keeps its own gestures).
    pub fn takes_drags(self) -> bool {
        matches!(
            self,
            Self::System | Self::Phone | Self::Messages | Self::Exocortex
        )
    }
}

#[derive(Debug)]
pub struct ShellState {
    /// In-shell brightness requests, drained by the brightness backend.
    #[allow(dead_code)]
    pub display_requests: Vec<crate::display::DisplayRequest>,
    /// Brightness as in-shell controls should show it (backend-refreshed).
    #[allow(dead_code)]
    pub display_view: Option<crate::display::DisplayView>,
    /// Auto-Lock's pre-timeout dim is active: the brightness backend must not
    /// write the backlight, and restores its level when this clears.
    #[allow(dead_code)]
    pub idle_dimmed: bool,
    /// Debug: the light-sensor probe frame (`light-probe`), drawn over all.
    pub light_probe: Option<crate::gfx::RectF>,
    pub view: View,
    pub screen_awake: bool,
    pub pin_input: Vec<u8>,
    pub failed_attempts: u32,
    pub lockout_until: Option<Instant>,
    pub volume: u8,
    pub volume_overlay_until: Option<Instant>,
    pub battery_percent: Option<u8>,
    pub battery_charging: bool,
    pub wifi_connected: bool,
    pub pointer: Option<PointerGesture>,
    pub last_action: String,
    pub redraw: bool,
    pub should_exit: bool,
    /// Exit to run the binary on disk again (`restart`).
    pub restart: bool,
    pub managed_windows: Vec<u32>,
    /// Managed windows that are proxies of Wayland toplevels: keys for them
    /// go to the Wayland server (`wayland_keys`) instead of XTEST.
    pub wayland_windows: Vec<u32>,
    pub wayland_keys: Vec<crate::keyboard::KeyboardInput>,
    pub keyboard: KeyboardState,
    /// The paste key was pressed: the main loop types the clipboard.
    pub paste_requested: bool,
    pub lock_face: LockFace,
    pub last_lock_input: Instant,
    pub pressed: Option<Press>,
    pub pin_error_at: Option<Instant>,
    /// An app card zooming back into its dock icon.
    pub closing: Option<CardMotion>,
    /// The finger-following swipe up out of an app.
    pub gesture: Option<HomeGesture>,
    /// A cancelled home gesture: the card springs back to full screen.
    pub returning: Option<CardMotion>,
    pub switcher: Option<Switcher>,
    /// A card flying from the home gesture into its switcher slot.
    pub switcher_entry: Option<CardMotion>,
    pub lock_motion: LockMotion,
    pub fade: Option<Fade>,
    pub unlock_at: Option<Instant>,
    /// Snapshot requests for the main loop (taken before restacking).
    pub pending_capture: Option<AppId>,
    pub pending_fade_capture: bool,
    /// Power fades ramp the backlight instead of drawing (set at startup).
    pub backlight_fades: bool,
    /// Apps the user closed from the switcher, for the main loop to stop.
    pub pending_kills: Vec<AppId>,
    /// A cancelled home gesture finished; the app window must be raised.
    pub pending_reveal: bool,
    /// The home gesture ended in the app switcher; the main loop opens it
    /// with the running apps.
    pub pending_switcher: bool,
    /// The shell window was exposed and must be recopied from its pixmap.
    pub shell_exposed: bool,
    /// Notifications, alarms and timers on screen (alerts.rs).
    pub alerts: crate::alerts::Alerts,
    /// Quick settings (control_centre.rs).
    pub control: crate::control_centre::ControlCentre,
    /// The power menu, shutdown and battery screens (power.rs).
    pub power: crate::power::PowerUi,
    /// Holding the power key to talk to Exocortex (voice.rs).
    pub voice: crate::voice::VoiceUi,
    /// Telephony status and the call screens (calls.rs).
    pub calls: crate::calls::CallUi,
    /// Wallpaper and Auto-Lock (settings.rs); `settings_changed` asks the
    /// main loop to save and apply them.
    pub settings: crate::settings::Settings,
    pub settings_changed: bool,
    /// The apps' icon badges (`badge N`; badges.rs); `badges_changed` asks
    /// the main loop to save them.
    pub badges: crate::badges::Badges,
    pub badges_changed: bool,
    /// The last touch or key, for Auto-Lock.
    pub last_input: Instant,
    /// The app in front holds Auto-Lock off: it plays media (its media
    /// lease) or keeps the screen awake (`awake on`). Set by the main loop.
    pub auto_lock_held: bool,
    /// The app shown before the current one (the bottom-edge swipe).
    previous_app: Option<AppId>,
    /// Touch events for the app in front, drained by the main loop, each
    /// with when the X server read it from the touchscreen (the XI2 raw
    /// touch's time, CLOCK_MONOTONIC milliseconds) when it came from there.
    pub app_pointer: Vec<(AppPointer, Option<u32>)>,
    /// The time of the XI2 raw touch being handled (`app_pointer`).
    pub touch_time: Option<u32>,
    pub open_target: Option<OpenTarget>,
    pub page_transition: Option<PageTransition>,
    /// The home screen's editing and the App Library (home.rs); its layout
    /// is `settings.home`.
    pub home: crate::home::HomeScreen,
    active_app_focused: bool,
    launch: Option<Launch>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublicTime {
    pub zone: Option<String>,
    pub tz: Option<String>,
    pub clock_24h: bool,
}

#[derive(Debug, Serialize)]
pub struct PublicState {
    pub version: &'static str,
    pub view: View,
    pub locked: bool,
    pub screen_awake: bool,
    pub pin_digits: usize,
    pub failed_attempts: u32,
    pub lockout_remaining_ms: u64,
    pub current_app: Option<&'static str>,
    pub volume: u8,
    pub battery_percent: Option<u8>,
    pub battery_charging: bool,
    pub wifi_connected: bool,
    pub width: u16,
    pub height: u16,
    pub lock_face: LockFace,
    pub last_action: String,
    pub pointer_active: bool,
    pub managed_windows: Vec<u32>,
    pub app_focused: bool,
    pub app_launching: bool,
    pub apps: Vec<PublicAppState>,
    pub keyboard: PublicKeyboardState,
    pub suspend: PublicSuspendState,
    pub alerts: crate::alerts::PublicAlerts,
    pub settings: crate::settings::Settings,
    pub control_centre_open: bool,
    pub telephony: crate::calls::PublicCalls,
    /// The power menu is open; the screen says it is powering off.
    pub power_menu_open: bool,
    /// Holding the power key: Moon listens (voice.rs).
    pub voice_listening: bool,
    pub shutting_down: Option<&'static str>,
    pub critical_battery_s: Option<u64>,
    /// The last notification target for an app (see `OpenTarget`).
    pub open_target: Option<PublicOpenTarget>,
    /// Edit mode and the App Library (the layout is `settings.home`).
    pub home: crate::home::PublicHome,
    /// Local time: the zone id (if chosen), its TZ rule, and the clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time: Option<PublicTime>,
    /// Frame timing of the last finished animation (see `profile on`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animation: Option<String>,
    /// Brightness (docs/display-brightness.md); filled in by the backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<crate::brightness::PublicDisplay>,
    /// The apps' own status bar colours (`statusbar color`), by app.
    pub status_bar: std::collections::BTreeMap<&'static str, crate::status_bar::PublicStatusStyle>,
    /// The running apps keeping the screen awake (`awake on`).
    pub awake: Vec<&'static str>,
    /// The apps' icon badges (`badge N`), by app.
    pub badges: std::collections::BTreeMap<&'static str, u32>,
}

impl ShellState {
    pub fn new(start_locked: bool, initial_volume: u8) -> Self {
        Self {
            display_requests: Vec::new(),
            display_view: None,
            idle_dimmed: false,
            light_probe: None,
            view: if start_locked {
                View::Locked
            } else {
                View::Launcher
            },
            screen_awake: true,
            pin_input: Vec::new(),
            failed_attempts: 0,
            lockout_until: None,
            volume: initial_volume.min(100),
            volume_overlay_until: None,
            battery_percent: None,
            battery_charging: false,
            wifi_connected: false,
            pointer: None,
            last_action: "startup".into(),
            redraw: true,
            should_exit: false,
            restart: false,
            managed_windows: Vec::new(),
            wayland_windows: Vec::new(),
            wayland_keys: Vec::new(),
            keyboard: KeyboardState::default(),
            paste_requested: false,
            lock_face: LockFace::Clock,
            last_lock_input: Instant::now(),
            pressed: None,
            pin_error_at: None,
            closing: None,
            gesture: None,
            returning: None,
            switcher: None,
            switcher_entry: None,
            lock_motion: LockMotion::default(),
            fade: None,
            unlock_at: None,
            pending_capture: None,
            pending_fade_capture: false,
            backlight_fades: false,
            pending_kills: Vec::new(),
            pending_reveal: false,
            pending_switcher: false,
            shell_exposed: false,
            alerts: Default::default(),
            control: Default::default(),
            calls: Default::default(),
            power: Default::default(),
            voice: Default::default(),
            settings: Default::default(),
            settings_changed: false,
            badges: Default::default(),
            badges_changed: false,
            auto_lock_held: false,
            last_input: Instant::now(),
            previous_app: None,
            app_pointer: Vec::new(),
            touch_time: None,
            open_target: None,
            page_transition: None,
            home: Default::default(),
            active_app_focused: false,
            launch: None,
        }
    }

    pub fn public(
        &self,
        width: u16,
        height: u16,
        apps: Vec<PublicAppState>,
        suspend: PublicSuspendState,
    ) -> PublicState {
        let remaining = self
            .lockout_until
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
            .unwrap_or_default();
        PublicState {
            version: env!("CARGO_PKG_VERSION"),
            view: self.view,
            locked: self.view == View::Locked,
            screen_awake: self.screen_awake,
            pin_digits: self.pin_input.len(),
            failed_attempts: self.failed_attempts,
            lockout_remaining_ms: remaining.as_millis().min(u64::MAX as u128) as u64,
            current_app: AppId::from_view(self.view).map(AppId::display_name),
            volume: self.volume,
            battery_percent: self.battery_percent,
            battery_charging: self.battery_charging,
            wifi_connected: self.wifi_connected,
            width,
            height,
            lock_face: self.lock_face,
            last_action: self.last_action.clone(),
            pointer_active: self.pointer.is_some(),
            managed_windows: self.managed_windows.clone(),
            app_focused: self.active_app_focused,
            app_launching: self.app_launching(),
            apps,
            keyboard: self.keyboard.public(),
            suspend,
            alerts: self.alerts_public(),
            settings: self.settings.clone(),
            control_centre_open: self.control.open,
            telephony: self.calls_public(),
            power_menu_open: self.power.menu.as_ref().is_some_and(|menu| menu.open),
            voice_listening: self.voice_listening(),
            shutting_down: self
                .power
                .shutting_down
                .map(crate::power::PowerAction::hook_argument),
            critical_battery_s: self.critical_seconds(),
            open_target: self.open_target.as_ref().map(|open| PublicOpenTarget {
                app: open.app,
                target: open.target.clone(),
                serial: open.serial,
                age_ms: open.at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            }),
            home: self.home_public(),
            time: None,
            animation: None,
            display: None,
            status_bar: Default::default(),
            awake: Vec::new(),
            badges: self.badges.public(),
        }
    }

    pub fn lock(&mut self) {
        self.keyboard.hide();
        self.view = View::Locked;
        self.lock_face = LockFace::Clock;
        self.lock_motion.snap(0.0);
        self.clear_pin();
        self.pointer = None;
        self.pressed = None;
        self.closing = None;
        self.gesture = None;
        self.returning = None;
        self.switcher = None;
        self.switcher_entry = None;
        self.unlock_at = None;
        self.pending_capture = None;
        self.pin_error_at = None;
        self.active_app_focused = false;
        self.cancel_app_launch();
        self.alerts_cancel_touch();
        self.control_cancel_touch();
        self.call_cancel_touch();
        self.power_cancel_touch();
        self.home_reset();
        self.last_action = "lock".into();
        self.redraw = true;
    }

    pub fn screen_off(&mut self) {
        self.voice_stop(false, Instant::now());
        self.lock();
        // Auto-Lock with the power menu open: it is gone when the screen
        // comes back, as on iPhone.
        self.power.menu = None;
        self.fade = None;
        self.screen_awake = false;
        self.last_action = "screen_off".into();
    }

    pub fn screen_on(&mut self) {
        self.lock();
        self.screen_awake = true;
        self.last_input = Instant::now();
        self.last_action = "screen_on".into();
    }

    /// A touch or key: Auto-Lock starts over (and undims) and a
    /// notification's wake no longer turns the screen back off.
    /// A tap haptic, when the user wants them and the screen is on (it
    /// never wakes anything).
    pub fn haptic(&mut self, style: crate::notify_client::Haptic) {
        let ms = Some(self.settings.haptic_ms[style.index()]).filter(|ms| *ms > 0);
        self.haptic_ms(style, ms);
    }

    /// A haptic of a given length (`ctl haptic STYLE MS`, for tuning).
    /// The wrong-passcode buzz plays even with tap haptics off.
    pub fn haptic_ms(&mut self, style: crate::notify_client::Haptic, ms: Option<u32>) {
        let wanted = self.settings.haptics || style == crate::notify_client::Haptic::Error;
        if wanted && self.screen_awake {
            self.alerts
                .outbox
                .push(crate::alerts::Outgoing::Haptic(style, ms));
        }
    }

    pub fn note_input(&mut self) {
        self.last_input = Instant::now();
        self.idle_dimmed = false;
        self.alerts_user_input();
    }

    /// The power key: fades to black before the screen-off transaction, or
    /// wakes to the lock screen.
    pub fn power_key(&mut self) {
        if !self.screen_awake {
            self.screen_on();
        } else if self.fade.is_none_or(|fade| !fade.out) {
            self.fade = Some(Fade {
                out: true,
                started: Instant::now(),
            });
            self.pending_fade_capture = !self.backlight_fades;
            self.pointer = None;
            self.pressed = None;
            self.keyboard.hide();
            self.last_action = "power_fade_out".into();
            self.redraw = true;
        }
    }

    /// Starts the fade in from black once a woken panel is lit.
    pub fn fade_in(&mut self) {
        self.fade = Some(Fade {
            out: false,
            started: Instant::now(),
        });
        self.redraw = true;
    }

    pub fn home(&mut self) {
        self.go_home(true);
    }

    /// Home after the app's process ended: there is nothing to snapshot.
    pub fn home_after_exit(&mut self) {
        self.go_home(false);
    }

    fn go_home(&mut self, snapshot: bool) {
        if !self.screen_awake || self.view == View::Locked {
            return;
        }
        // Already home: out of edit mode, or back from the App Library.
        let at_home = self.view == View::Launcher && self.switcher.is_none();
        let now = Instant::now();
        self.keyboard.hide();
        if let Some(app) = AppId::from_view(self.view) {
            let (from, radius) = if let Some(gesture) = self.gesture.take() {
                (gesture.card_rect(now), gesture.card_radius(now))
            } else if let Some(card) = self.returning.take() {
                (card.rect(now), card.radius(now))
            } else if let Some(launch) = self.launch() {
                (launch.card.rect(now), launch.card.radius(now))
            } else {
                if snapshot {
                    self.pending_capture = Some(app);
                }
                (SCREEN, DISPLAY_RADIUS)
            };
            let to = self.home_icon_rect(app);
            self.closing = Some(close_motion(app, from, to, radius, (0.0, 0.0), now));
        }
        self.switcher = None;
        self.switcher_entry = None;
        self.pressed = None;
        self.view = View::Launcher;
        self.active_app_focused = false;
        self.cancel_app_launch();
        self.last_action = "home".into();
        self.redraw = true;
        if at_home {
            self.home_button();
        }
    }

    /// The finger rose out of an app's bottom zone: from now on the app's
    /// snapshot follows it.
    pub fn begin_home_gesture(&mut self, x: f32, y: f32) -> bool {
        let Some(app) = AppId::from_view(self.view) else {
            return false;
        };
        if !self.screen_awake
            || self.app_launching()
            || self.fade.is_some()
            || self.gesture.is_some()
        {
            return false;
        }
        let now = Instant::now();
        let mut gesture = HomeGesture::new(app, (x, y + HOME_GESTURE_SLOP), now);
        gesture.update(x, y, now);
        self.gesture = Some(gesture);
        self.returning = None;
        self.pending_capture = Some(app);
        self.keyboard.hide();
        // X focus stays with the app under the gesture; if it springs back,
        // no FocusIn would arrive to restore this flag.
        self.last_action = "home_gesture".into();
        self.redraw = true;
        true
    }

    pub fn update_home_gesture(&mut self, x: f32, y: f32) {
        if let Some(gesture) = self.gesture.as_mut() {
            gesture.update(x, y, Instant::now());
            self.redraw = true;
        }
    }

    /// The finger lifted: go home, back to the app, or to the switcher.
    pub fn end_home_gesture(&mut self) -> Option<GestureOutcome> {
        let gesture = self.gesture.take()?;
        let now = Instant::now();
        let (outcome, velocity) = gesture.release(now);
        let rect = gesture.card_rect(now);
        let radius = gesture.card_radius(now);
        match outcome {
            GestureOutcome::Home => {
                self.go_home(false);
                let to = self.home_icon_rect(gesture.app);
                self.closing = Some(close_motion(gesture.app, rect, to, radius, velocity, now));
                self.last_action = "swipe_up_background".into();
            }
            GestureOutcome::App => {
                self.returning = Some(CardMotion::new(
                    gesture.app,
                    rect,
                    SCREEN,
                    radius,
                    DISPLAY_RADIUS,
                    velocity,
                    now,
                    crate::interaction::RETURN_RESPONSE,
                    0.9,
                ));
                self.last_action = "home_gesture_cancel".into();
            }
            GestureOutcome::Switcher => {
                // The caller opens the switcher with the running apps.
                self.go_home(false);
                self.closing = None;
                self.switcher_entry = Some(CardMotion::new(
                    gesture.app,
                    rect,
                    rect,
                    radius,
                    CARD_RADIUS,
                    velocity,
                    now,
                    0.36,
                    0.9,
                ));
                self.pending_switcher = true;
                self.last_action = "app_switcher".into();
            }
        }
        self.redraw = true;
        Some(outcome)
    }

    /// Opens the app switcher over the home screen. `apps` are the running
    /// apps, oldest first; the newest is centered.
    pub fn open_switcher(&mut self, apps: Vec<AppId>) {
        if !self.screen_awake || self.view == View::Locked {
            return;
        }
        if self.view.is_app() {
            self.go_home(true);
            self.closing = None;
        }
        if apps.is_empty() {
            self.switcher_entry = None;
            return;
        }
        let now = Instant::now();
        let switcher = Switcher::new(apps, now);
        if let Some(entry) = self.switcher_entry.take()
            && let Some(index) = switcher.apps.iter().position(|app| *app == entry.app)
        {
            let slot = switcher.card_rect(index, now);
            let from = entry.rect(now);
            let velocity = (0.0, 0.0);
            self.switcher_entry = Some(CardMotion::new(
                entry.app,
                from,
                slot,
                entry.from_radius,
                CARD_RADIUS,
                velocity,
                now,
                0.36,
                0.92,
            ));
        }
        self.switcher = Some(switcher);
        self.last_action = "app_switcher".into();
        self.redraw = true;
    }

    pub fn switcher_touch_begin(&mut self, x: f32, y: f32) {
        if let Some(switcher) = self.switcher.as_mut() {
            switcher.begin_touch(x, y, Instant::now());
            self.redraw = true;
        }
    }

    pub fn switcher_touch_move(&mut self, x: f32, y: f32) {
        if let Some(switcher) = self.switcher.as_mut() {
            switcher.move_touch(x, y, Instant::now());
            self.redraw = true;
        }
    }

    pub fn switcher_touch_end(&mut self) {
        let Some(switcher) = self.switcher.as_mut() else {
            return;
        };
        let now = Instant::now();
        match switcher.end_touch(now) {
            SwitcherAction::None => {}
            SwitcherAction::Home => switcher.exit(now),
            SwitcherAction::Open(app, rect) => {
                self.switcher = None;
                self.switcher_entry = None;
                self.begin_launch_from(app, rect, CARD_RADIUS);
            }
        }
        self.redraw = true;
    }

    pub fn replace_managed_windows(&mut self, windows: Vec<u32>) {
        self.managed_windows = windows;
        if self.managed_windows.is_empty() {
            self.active_app_focused = false;
            self.hide_keyboard();
        }
    }

    pub fn set_active_app_focused(&mut self, focused: bool) {
        self.active_app_focused = focused && self.screen_awake && self.view.is_app();
    }

    pub fn active_app_focused(&self) -> bool {
        self.active_app_focused
    }

    pub fn note_app_resumed(&mut self, app: AppId) {
        // A resumed app keeps its existing window; the opening zoom still
        // plays and the window is revealed as soon as it finishes.
        if let Some(launch) = self.launch.as_mut().filter(|launch| launch.app == app) {
            launch.resumed = true;
            launch.window_mapped_at.get_or_insert_with(Instant::now);
        }
        self.last_action = format!("{}_resumed", app.cgroup_name());
        self.redraw = true;
    }

    pub fn note_memory_eviction(&mut self, app: AppId) {
        self.last_action = format!(
            "{}_evicted_memory_pressure",
            app.display_name().to_ascii_lowercase()
        );
        self.redraw = true;
    }

    pub fn launch_system(&mut self) {
        self.begin_launch(AppId::System);
    }

    pub fn open_app(&mut self, app: AppId) {
        self.begin_launch(app);
    }

    fn begin_launch(&mut self, app: AppId) {
        let origin = self.home_icon_rect(app);
        self.begin_launch_from(app, origin, crate::ui::ICON_RADIUS);
    }

    pub(crate) fn begin_launch_from(&mut self, app: AppId, origin: RectF, radius: f32) {
        let view = app.view();
        if self.screen_awake && self.view != View::Locked && self.view != view {
            let now = Instant::now();
            self.keyboard.hide();
            if let Some(current) = AppId::from_view(self.view)
                && self.launch().is_none()
            {
                // Switching straight from another app: keep its snapshot.
                self.pending_capture = Some(current);
            }
            if let Some(current) = AppId::from_view(self.view) {
                self.previous_app = Some(current);
            }
            self.view = view;
            self.active_app_focused = false;
            self.pressed = None;
            self.closing = None;
            self.gesture = None;
            self.returning = None;
            self.switcher = None;
            self.switcher_entry = None;
            self.managed_windows.clear();
            self.launch = Some(Launch {
                app,
                started: now,
                card: CardMotion::open(app, origin, radius, now),
                resumed: false,
                window_mapped_at: None,
            });
            self.last_action = format!("launch_{}", app.cgroup_name());
            self.redraw = true;
        }
    }

    /// Switches straight to the previous app (bottom-edge horizontal
    /// swipe).
    pub fn switch_app(&mut self) {
        let Some(current) = AppId::from_view(self.view) else {
            return;
        };
        let other = match self.previous_app.filter(|app| *app != current) {
            Some(app) => app,
            // Before any switch: the System app and the Browser swap.
            None if current == AppId::System => AppId::Browser,
            None => AppId::System,
        };
        self.begin_launch(other);
        self.last_action = "swipe_switch_app".into();
    }

    /// Reveals the passcode keypad over the lock-screen clock.
    pub fn show_passcode(&mut self) {
        if self.screen_awake && self.view == View::Locked && self.lock_face != LockFace::Passcode {
            self.lock_face = LockFace::Passcode;
            self.lock_motion.animate_to(1.0, Instant::now());
            self.last_lock_input = Instant::now();
            self.last_action = "passcode_shown".into();
            self.redraw = true;
        }
    }

    /// Returns from the passcode keypad to the lock-screen clock.
    pub fn cancel_passcode(&mut self) {
        if self.view == View::Locked && self.lock_face == LockFace::Passcode {
            self.lock_face = LockFace::Clock;
            self.lock_motion.animate_to(0.0, Instant::now());
            self.clear_pin();
            self.pin_error_at = None;
            self.last_action = "passcode_cancel".into();
            self.redraw = true;
        }
    }

    /// A touch went down on the lock screen; vertical motion drags between
    /// the clock and the keypad.
    pub fn lock_drag_begin(&mut self, y: f32) {
        if self.view == View::Locked && self.screen_awake && self.fade.is_none() {
            self.lock_motion.begin_drag(y, Instant::now());
        }
    }

    pub fn lock_drag_move(&mut self, y: f32) {
        if self.lock_motion.dragging() {
            self.lock_motion.move_drag(y, Instant::now());
            self.last_lock_input = Instant::now();
            self.redraw = true;
        }
    }

    /// Ends a lock-screen drag. Returns true when the touch moved enough to
    /// count as a drag (so it is not also a tap).
    pub fn lock_drag_end(&mut self, travelled: f32) -> bool {
        if !self.lock_motion.dragging() {
            return false;
        }
        if travelled < 24.0 {
            // A tap: settle where it was without a decision.
            let target = self.lock_motion.target();
            self.lock_motion.animate_to(target, Instant::now());
            return false;
        }
        let keypad = self.lock_motion.end_drag(Instant::now());
        let face = if keypad {
            LockFace::Passcode
        } else {
            LockFace::Clock
        };
        if face != self.lock_face {
            self.lock_face = face;
            self.last_lock_input = Instant::now();
            if face == LockFace::Clock {
                self.clear_pin();
                self.pin_error_at = None;
            }
            self.last_action = if keypad {
                "passcode_shown"
            } else {
                "passcode_cancel"
            }
            .into();
        }
        self.redraw = true;
        true
    }

    pub fn input_digit(&mut self, digit: u8, config: &Config) {
        if !self.screen_awake || self.view != View::Locked || digit > 9 || self.is_lockout_active()
        {
            return;
        }
        self.show_passcode();
        self.last_lock_input = Instant::now();
        if self.pin_input.len() < config.pin_length {
            self.pin_input.push(b'0' + digit);
            self.last_action = "pin_digit".into();
            self.redraw = true;
        }
        if self.pin_input.len() == config.pin_length {
            self.submit_pin(config);
        }
    }

    pub fn backspace_pin(&mut self) {
        if self.screen_awake && self.view == View::Locked && !self.is_lockout_active() {
            self.last_lock_input = Instant::now();
            self.pin_input.pop();
            self.last_action = "pin_backspace".into();
            self.redraw = true;
        }
    }

    pub fn submit_pin(&mut self, config: &Config) -> bool {
        if !self.screen_awake || self.view != View::Locked || self.is_lockout_active() {
            return false;
        }
        let accepted = verify_pin(config, &self.pin_input);
        self.clear_pin();
        self.last_lock_input = Instant::now();
        if accepted {
            self.unlock_at = Some(Instant::now());
            self.view = View::Launcher;
            self.lock_face = LockFace::Clock;
            self.pin_error_at = None;
            self.failed_attempts = 0;
            self.lockout_until = None;
            self.last_action = "unlock".into();
            self.alerts_unlocked();
            self.voice_unlocked();
        } else {
            self.failed_attempts = self.failed_attempts.saturating_add(1);
            self.pin_error_at = Some(Instant::now());
            self.haptic(crate::notify_client::Haptic::Error);
            self.last_action = "unlock_failed".into();
            if self.failed_attempts % 5 == 0 {
                self.lockout_until = Some(Instant::now() + Duration::from_secs(30));
            }
        }
        self.redraw = true;
        accepted
    }

    fn clear_pin(&mut self) {
        self.pin_input.fill(0);
        self.pin_input.clear();
    }

    pub fn is_lockout_active(&mut self) -> bool {
        if let Some(deadline) = self.lockout_until {
            if Instant::now() < deadline {
                return true;
            }
            self.lockout_until = None;
            self.redraw = true;
        }
        false
    }

    pub fn change_volume(&mut self, delta: i8) {
        self.volume = (i16::from(self.volume) + i16::from(delta)).clamp(0, 100) as u8;
        self.volume_overlay_until = Some(Instant::now() + Duration::from_millis(1800));
        self.last_action = if delta >= 0 {
            "volume_up"
        } else {
            "volume_down"
        }
        .into();
        self.redraw = true;
    }

    pub fn set_volume(&mut self, value: u8) {
        self.volume = value.min(100);
        self.volume_overlay_until = Some(Instant::now() + Duration::from_millis(1800));
        self.last_action = "volume_set".into();
        self.redraw = true;
    }

    /// Queues a touch event for the app in front, stamped with the raw
    /// touch's time (`touch_time`).
    pub fn push_app_pointer(&mut self, event: AppPointer) {
        self.app_pointer.push((event, self.touch_time));
    }

    pub fn show_keyboard(&mut self, purpose: KeyboardPurpose) -> bool {
        if !self.screen_awake
            || !self.view.is_app()
            || self.app_launching()
            || self.managed_windows.is_empty()
        {
            self.last_action = "keyboard_show_blocked".into();
            return false;
        }
        self.keyboard.show(purpose);
        self.last_action = "keyboard_show".into();
        true
    }

    pub fn hide_keyboard(&mut self) {
        if self.keyboard.hide() {
            self.last_action = "keyboard_hide".into();
        }
    }

    pub fn activate_keyboard_key(&mut self, action: KeyAction) -> KeyboardEffect {
        let effect = self.keyboard.activate(action);
        self.last_action = match action {
            KeyAction::Shift => "keyboard_shift",
            KeyAction::SwitchLayout(_) => "keyboard_layout",
            KeyAction::Enter => "keyboard_submit",
            KeyAction::Character(_) | KeyAction::Space | KeyAction::Backspace => "keyboard_input",
            KeyAction::Paste => "keyboard_paste",
        }
        .into();
        if effect == KeyboardEffect::Paste {
            self.paste_requested = true;
        }
        effect
    }

    pub fn update_device_status(
        &mut self,
        battery_percent: Option<u8>,
        battery_charging: bool,
        wifi_connected: bool,
    ) {
        let battery_percent = battery_percent.map(|value| value.min(100));
        if self.battery_percent != battery_percent
            || self.battery_charging != battery_charging
            || self.wifi_connected != wifi_connected
        {
            self.battery_percent = battery_percent;
            self.battery_charging = battery_charging;
            self.wifi_connected = wifi_connected;
            self.redraw = true;
        }
    }

    pub fn app_launching(&self) -> bool {
        self.launch
            .is_some_and(|launch| AppId::from_view(self.view) == Some(launch.app))
    }

    pub fn launch(&self) -> Option<Launch> {
        self.launch.filter(|_| self.app_launching())
    }

    pub fn note_app_window_mapped(&mut self) {
        let view = self.view;
        if let Some(launch) = self.launch.as_mut()
            && AppId::from_view(view) == Some(launch.app)
            && launch.window_mapped_at.is_none()
        {
            launch.window_mapped_at = Some(Instant::now());
            self.last_action = format!("{}_window_warming", launch.app.cgroup_name());
            self.redraw = true;
        }
    }

    fn reveal_at(&self) -> Option<Instant> {
        let launch = self.launch()?;
        let mapped_at = launch.window_mapped_at?;
        let paint_delay = if launch.resumed {
            Duration::ZERO
        } else {
            match launch.app {
                // Phone, Messages and Exocortex are the System app's binary.
                AppId::System | AppId::Phone | AppId::Messages | AppId::Exocortex => {
                    SYSTEM_FIRST_PAINT_DELAY
                }
                AppId::Browser => BROWSER_FIRST_PAINT_DELAY,
            }
        };
        Some(mapped_at + paint_delay)
    }

    pub fn app_ready_to_reveal(&self) -> bool {
        let now = Instant::now();
        self.launch()
            .is_some_and(|launch| launch.card.progress(now) >= 0.995 || launch.card.settled(now))
            && self.reveal_at().is_some_and(|reveal_at| now >= reveal_at)
    }

    /// The shell covers the foreground app: while it opens, while the home
    /// gesture or its return is in motion, and during the power fade.
    pub fn shell_covers_app(&self) -> bool {
        self.view.is_app()
            && (self.app_launching()
                || self.gesture.is_some()
                || self.returning.is_some()
                || self
                    .fade
                    .is_some_and(|fade| fade.out && !self.backlight_fades)
                || self.alerts_cover_app(Instant::now()))
    }

    pub fn finish_app_launch(&mut self) {
        let Some(launch) = self.launch() else {
            return;
        };
        self.launch = None;
        self.last_action = format!("{}_ready", launch.app.cgroup_name());
        self.redraw = true;
    }

    fn cancel_app_launch(&mut self) {
        self.launch = None;
    }

    /// True while a shell animation needs frames.
    pub fn animating(&self, now: Instant) -> bool {
        if !self.screen_awake {
            return false;
        }
        let opening = self.launch().is_some_and(|launch| {
            !launch.card.settled(now)
                // The launch screen's activity indicator.
                || now.duration_since(launch.started) > Duration::from_millis(700)
        });
        let card = |motion: &Option<CardMotion>| motion.is_some_and(|m| !m.settled(now));
        let shaking = self.view == View::Locked
            && self
                .pin_error_at
                .is_some_and(|at| now < at + SHAKE_ANIMATION);
        let unlocking = self.unlock_at.is_some_and(|at| now < at + UNLOCK_ANIMATION);
        opening
            || card(&self.closing)
            || card(&self.returning)
            || card(&self.switcher_entry)
            || self.gesture.as_ref().is_some_and(|g| {
                g.armed_at
                    .is_some_and(|at| now < at + Duration::from_millis(500))
            })
            || self.switcher.as_ref().is_some_and(|s| s.animating(now))
            || (self.view == View::Locked && self.lock_motion.animating(now))
            || self.fade.is_some()
            || shaking
            || unlocking
            || self.alerts_animating(now)
            || self.control.animating(now)
            || self.calls_animating(now)
            || self.power_animating(now)
            || self.voice.visible(now)
            || self.page_transition.is_some()
            || (self.view == View::Launcher && self.home.animating(now))
    }

    /// An app may ask for a page transition now: its own app in front,
    /// nothing of the shell over it.
    pub fn page_transition_allowed(&self) -> bool {
        self.screen_awake
            && self.view.takes_drags()
            && self.launch().is_none()
            && self.gesture.is_none()
            && self.switcher.is_none()
            && !self.shell_covers_app()
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        self.alerts_tick(now);
        self.control_tick(now);
        self.calls_tick(now);
        self.power_tick(now);
        self.voice_tick(now);
        self.home_tick(now);
        if let Some(transition) = self.page_transition.as_mut()
            && transition.waiting
            && now.saturating_duration_since(transition.started) >= PAGE_READY_WAIT
        {
            transition.ready(now);
        }
        if let Some(transition) = self.page_transition
            && ((!transition.waiting
                && now.saturating_duration_since(transition.started) >= PAGE_TRANSITION)
                || !self.page_transition_allowed())
        {
            self.page_transition = None;
            self.redraw = true;
        }
        if self.is_lockout_active() && self.view == View::Locked {
            // Repaint the "Try again in N s" countdown.
            self.redraw = true;
        }
        if self.view == View::Locked
            && self.lock_face == LockFace::Passcode
            && now.duration_since(self.last_lock_input) >= PASSCODE_IDLE_TIMEOUT
            && self.pointer.is_none()
        {
            self.cancel_passcode();
            self.last_action = "passcode_idle_timeout".into();
        }
        if self.closing.is_some_and(|closing| closing.settled(now)) {
            self.closing = None;
            self.redraw = true;
        }
        if self.switcher_entry.is_some_and(|entry| entry.settled(now)) {
            self.switcher_entry = None;
            self.redraw = true;
        }
        if self.returning.is_some_and(|card| card.settled(now)) {
            self.returning = None;
            self.pending_reveal = true;
            self.redraw = true;
        }
        if let Some(gesture) = self.gesture.as_mut() {
            let armed = gesture.armed();
            gesture.refresh_hold(now);
            if gesture.armed() != armed {
                let engaged = gesture.armed();
                self.redraw = true;
                if engaged {
                    // The app switcher is about to open.
                    self.haptic(crate::notify_client::Haptic::Medium);
                }
            }
        }
        if let Some(switcher) = self.switcher.as_mut() {
            if let Some(app) = switcher.finish_dismissal(now) {
                self.pending_kills.push(app);
                self.last_action = format!("{}_closed", app.display_name().to_ascii_lowercase());
                self.redraw = true;
            }
            if switcher.exited(now) || (switcher.apps.is_empty() && switcher.dismissal.is_none()) {
                self.switcher = None;
                self.redraw = true;
            }
        }
        if self
            .unlock_at
            .is_some_and(|at| now >= at + UNLOCK_ANIMATION)
        {
            self.unlock_at = None;
            self.redraw = true;
        }
        if let Some(fade) = self.fade
            && fade.finished(now)
        {
            if fade.out {
                // The screen is black: hand over to the screen-off transaction.
                self.screen_off();
            } else {
                self.fade = None;
                self.redraw = true;
            }
        }
        if self
            .pin_error_at
            .is_some_and(|at| now >= at + SHAKE_ANIMATION)
        {
            self.pin_error_at = None;
            self.redraw = true;
        }
        if self
            .volume_overlay_until
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            self.volume_overlay_until = None;
            self.redraw = true;
        }
    }

    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        let passcode_timeout = (self.view == View::Locked && self.lock_face == LockFace::Passcode)
            .then(|| self.last_lock_input + PASSCODE_IDLE_TIMEOUT);
        let lockout_second = self
            .lockout_until
            .filter(|until| *until > now)
            .map(|until| {
                let remaining = until.duration_since(now);
                now + Duration::from_nanos(u64::from(remaining.subsec_nanos()) + 1_000_000)
            });
        [
            self.lockout_until,
            lockout_second,
            self.volume_overlay_until,
            self.reveal_at(),
            passcode_timeout,
            self.gesture.as_ref().and_then(HomeGesture::hold_deadline),
            self.fade.map(|fade| fade.deadline()),
            self.alerts_deadline(),
            self.power_deadline(now),
            self.home.deadline(),
        ]
        .into_iter()
        .flatten()
        .filter(|deadline| *deadline > now)
        .min()
    }
}

/// The card of an app going home, zooming into its icon (`to`).
fn close_motion(
    app: AppId,
    from: RectF,
    to: RectF,
    radius: f32,
    velocity: (f32, f32),
    now: Instant,
) -> CardMotion {
    CardMotion::new(
        app,
        from,
        to,
        radius,
        crate::ui::ICON_RADIUS,
        velocity,
        now,
        crate::interaction::CLOSE_RESPONSE,
        0.86,
    )
}

impl Drop for ShellState {
    fn drop(&mut self) {
        self.clear_pin();
    }
}

fn verify_pin(config: &Config, candidate: &[u8]) -> bool {
    let mut hasher = Sha256::new();
    hasher.update(&config.pin_salt);
    hasher.update(candidate);
    let actual = hasher.finalize();
    actual.as_slice().ct_eq(config.pin_hash.as_slice()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> Config {
        let salt = b"test-salt-123456".to_vec();
        let mut hasher = Sha256::new();
        hasher.update(&salt);
        hasher.update(b"654321");
        Config {
            pin_salt: salt,
            pin_hash: hasher.finalize().to_vec(),
            pin_length: 6,
            start_locked: true,
            initial_volume: 50,
            keyboard_repeat_delay_ms: 200,
            keyboard_repeat_rate_hz: 45,
            deep_suspend: false,
            clock_24h: false,
            timezone: None,
            socket_path: "/tmp/a26-shell-test.sock".into(),
        }
    }

    fn public(state: &ShellState) -> PublicState {
        state.public(
            1080,
            2340,
            Vec::new(),
            crate::suspend::SuspendCoordinator::new(
                true,
                true,
                crate::suspend::PersistedSuspendState::default(),
            )
            .public(),
        )
    }

    #[test]
    fn correct_pin_unlocks() {
        let config = test_config();
        let mut state = ShellState::new(true, 50);
        for digit in [6, 5, 4, 3, 2, 1] {
            state.input_digit(digit, &config);
        }
        assert_eq!(state.view, View::Launcher);
    }

    #[test]
    fn wrong_pin_stays_locked() {
        let config = test_config();
        let mut state = ShellState::new(true, 50);
        for digit in [1, 2, 3, 4, 5, 6] {
            state.input_digit(digit, &config);
        }
        assert_eq!(state.view, View::Locked);
        assert_eq!(state.failed_attempts, 1);
    }

    #[test]
    fn power_cycle_wakes_only_to_lock_screen() {
        let config = test_config();
        let mut state = ShellState::new(true, 50);
        for digit in [6, 5, 4, 3, 2, 1] {
            state.input_digit(digit, &config);
        }
        state.launch_system();
        assert_eq!(state.view, View::System);

        state.screen_off();
        assert!(!state.screen_awake);
        assert_eq!(state.view, View::Locked);
        state.screen_on();
        assert!(state.screen_awake);
        assert_eq!(state.view, View::Locked);
    }

    #[test]
    fn browser_is_a_first_class_external_app() {
        let mut state = ShellState::new(false, 50);
        state.open_app(AppId::Browser);
        assert_eq!(state.view, View::Browser);
        assert_eq!(public(&state).current_app, Some("Browser"));
        assert!(state.view.is_app());
        assert!(state.app_launching());

        state.note_app_window_mapped();
        assert!(!state.app_ready_to_reveal());
        let launch = state.launch.as_mut().unwrap();
        launch.started -= Duration::from_secs(2);
        launch.card.spring.started -= Duration::from_secs(2);
        launch.window_mapped_at = Some(Instant::now() - Duration::from_secs(2));
        assert!(state.app_ready_to_reveal());
        state.finish_app_launch();
        assert!(!state.app_launching());
    }

    #[test]
    fn system_app_reveals_after_the_opening_zoom() {
        let mut state = ShellState::new(false, 50);
        state.launch_system();
        assert_eq!(state.view, View::System);
        assert!(state.app_launching());
        assert!(state.animating(Instant::now()));
        assert!(!state.app_ready_to_reveal());

        state.note_app_window_mapped();
        let launch = state.launch.as_mut().unwrap();
        launch.card.spring.started -= Duration::from_secs(2);
        launch.window_mapped_at = Some(Instant::now() - Duration::from_millis(100));
        assert!(state.app_ready_to_reveal());
        state.finish_app_launch();
        assert!(!public(&state).app_launching);
        assert!(!state.animating(Instant::now()));
    }

    #[test]
    fn resumed_app_reveals_without_waiting_for_first_paint() {
        let mut state = ShellState::new(false, 50);
        state.open_app(AppId::Browser);
        state.note_app_resumed(AppId::Browser);
        state.launch.as_mut().unwrap().card.spring.started -= Duration::from_secs(2);
        assert!(state.app_ready_to_reveal());
    }

    #[test]
    fn going_home_plays_the_closing_zoom_once() {
        let mut state = ShellState::new(false, 50);
        state.launch_system();
        state.finish_app_launch();
        state.home();
        assert_eq!(state.view, View::Launcher);
        assert_eq!(
            state.closing.map(|closing| closing.app),
            Some(AppId::System)
        );
        assert!(state.animating(Instant::now()));
        state.closing.as_mut().unwrap().spring.started -= Duration::from_secs(2);
        state.tick();
        assert!(state.closing.is_none());
    }

    fn foreground_system() -> ShellState {
        let mut state = ShellState::new(false, 50);
        state.launch_system();
        state.finish_app_launch();
        state.pending_capture = None;
        state
    }

    #[test]
    fn home_gesture_snapshots_and_flick_goes_home() {
        let mut state = foreground_system();
        assert!(state.begin_home_gesture(540.0, 2250.0));
        assert_eq!(state.pending_capture, Some(AppId::System));
        assert!(state.shell_covers_app());
        for step in 1..=6 {
            std::thread::sleep(Duration::from_millis(4));
            state.update_home_gesture(540.0, 2250.0 - step as f32 * 60.0);
        }
        assert_eq!(state.end_home_gesture(), Some(GestureOutcome::Home));
        assert_eq!(state.view, View::Launcher);
        assert!(state.closing.is_some());
        assert!(state.gesture.is_none());
    }

    #[test]
    fn short_home_gesture_springs_back_and_reveals_the_app() {
        let mut state = foreground_system();
        state.begin_home_gesture(540.0, 2250.0);
        state.gesture.as_mut().unwrap().tracker = Default::default();
        assert_eq!(state.end_home_gesture(), Some(GestureOutcome::App));
        assert_eq!(state.view, View::System);
        assert!(state.shell_covers_app());
        state.returning.as_mut().unwrap().spring.started -= Duration::from_secs(2);
        state.tick();
        assert!(state.pending_reveal);
        assert!(!state.shell_covers_app());
    }

    #[test]
    fn held_home_gesture_opens_the_switcher_and_a_thrown_card_closes_its_app() {
        let mut state = foreground_system();
        state.begin_home_gesture(540.0, 2250.0);
        let gesture = state.gesture.as_mut().unwrap();
        gesture.armed_at = Some(Instant::now());
        gesture.tracker = Default::default();
        gesture.current = (540.0, 1500.0);
        assert_eq!(state.end_home_gesture(), Some(GestureOutcome::Switcher));
        assert!(state.pending_switcher);
        state.open_switcher(vec![AppId::Browser, AppId::System]);
        assert!(state.switcher.is_some());
        assert!(state.switcher_entry.is_some());

        let now = Instant::now();
        let rect = state.switcher.as_ref().unwrap().card_rect(0, now);
        let (x, y) = rect.center();
        state.switcher_touch_begin(x, y);
        for step in 1..=8 {
            std::thread::sleep(Duration::from_millis(3));
            state.switcher_touch_move(x, y - step as f32 * 90.0);
        }
        state.switcher_touch_end();
        let dismissal = state.switcher.as_mut().unwrap().dismissal.as_mut().unwrap();
        assert!(dismissal.closing);
        dismissal.offset.started -= Duration::from_secs(2);
        state.tick();
        assert_eq!(state.pending_kills, vec![AppId::Browser]);
        assert_eq!(state.switcher.as_ref().unwrap().apps, vec![AppId::System]);

        // Tapping the remaining card opens its app from the card.
        let rect = state
            .switcher
            .as_ref()
            .unwrap()
            .card_rect(0, Instant::now() + Duration::from_secs(2));
        let (x, y) = rect.center();
        state
            .switcher
            .as_mut()
            .unwrap()
            .begin_touch(x, y, Instant::now() + Duration::from_secs(2));
        state.switcher_touch_end();
        assert_eq!(state.view, View::System);
        assert!(state.app_launching());
        assert!(state.switcher.is_none());
    }

    #[test]
    fn page_transitions_run_their_course_and_stop_when_the_shell_takes_over() {
        let mut state = ShellState::new(false, 50);
        // Only Moon's own apps, in front, may ask.
        assert!(!state.page_transition_allowed());
        state.view = View::Messages;
        assert!(state.page_transition_allowed());
        let started = Instant::now();
        let mut transition = PageTransition::new(true, started);
        // Held still until the app's new page is drawn.
        assert_eq!(transition.progress(started + PAGE_TRANSITION), 0.0);
        transition.ready(started);
        assert_eq!(transition.progress(started), 0.0);
        assert_eq!(transition.progress(started + PAGE_TRANSITION), 1.0);
        state.page_transition = Some(transition);
        assert!(state.animating(started));
        state.tick();
        assert!(state.page_transition.is_some());
        // Never ready: it slides anyway after a moment, then ends.
        state.page_transition = Some(PageTransition::new(false, started - PAGE_READY_WAIT));
        state.tick();
        assert!(!state.page_transition.unwrap().waiting);
        state.page_transition.as_mut().unwrap().started -= PAGE_TRANSITION;
        state.tick();
        assert!(state.page_transition.is_none());
        // The browser keeps its own pages.
        state.view = View::Browser;
        assert!(!state.page_transition_allowed());
    }

    #[test]
    fn power_key_fades_out_before_screen_off_and_wakes_to_the_lock_screen() {
        let mut state = foreground_system();
        state.power_key();
        assert!(state.screen_awake);
        assert!(state.pending_fade_capture);
        assert!(state.shell_covers_app());
        state.fade.as_mut().unwrap().started -= Duration::from_secs(1);
        state.tick();
        assert!(!state.screen_awake);
        assert_eq!(state.view, View::Locked);
        state.power_key();
        assert!(state.screen_awake);
        state.fade_in();
        assert!(state.animating(Instant::now()));
    }

    #[test]
    fn lock_screen_drag_reveals_the_keypad() {
        let mut state = ShellState::new(true, 50);
        state.lock_drag_begin(2000.0);
        for step in 1..=6 {
            std::thread::sleep(Duration::from_millis(3));
            state.lock_drag_move(2000.0 - step as f32 * 80.0);
        }
        assert!(state.lock_drag_end(480.0));
        assert_eq!(state.lock_face, LockFace::Passcode);
        // A tap is not a drag.
        state.lock_drag_begin(1000.0);
        assert!(!state.lock_drag_end(3.0));
        assert_eq!(state.lock_face, LockFace::Passcode);
    }

    #[test]
    fn bottom_swipe_switches_between_apps() {
        let mut state = ShellState::new(false, 50);
        state.launch_system();
        state.finish_app_launch();
        state.switch_app();
        assert_eq!(state.view, View::Browser);
        assert!(state.app_launching());
        assert!(state.closing.is_none());
        state.home();
        state.switch_app();
        assert_eq!(state.view, View::Launcher);
    }

    #[test]
    fn lock_screen_reveals_passcode_and_times_out_to_the_clock() {
        let config = test_config();
        let mut state = ShellState::new(true, 50);
        assert_eq!(state.lock_face, LockFace::Clock);
        state.input_digit(1, &config);
        assert_eq!(state.lock_face, LockFace::Passcode);
        assert_eq!(state.pin_input.len(), 1);
        state.last_lock_input -= PASSCODE_IDLE_TIMEOUT;
        state.tick();
        assert_eq!(state.lock_face, LockFace::Clock);
        assert!(state.pin_input.is_empty());
    }

    #[test]
    fn wrong_passcode_shakes_and_lock_resets_the_face() {
        let config = test_config();
        let mut state = ShellState::new(true, 50);
        for digit in [1, 1, 1, 1, 1, 1] {
            state.input_digit(digit, &config);
        }
        assert!(state.pin_error_at.is_some());
        assert!(state.animating(Instant::now()));
        state.lock();
        assert_eq!(state.lock_face, LockFace::Clock);
        assert!(state.pin_error_at.is_none());
    }

    #[test]
    fn keyboard_state_follows_app_and_security_transitions() {
        let mut state = ShellState::new(false, 50);
        state.launch_system();
        state.finish_app_launch();
        state.managed_windows.push(42);
        assert!(state.show_keyboard(KeyboardPurpose::Text));
        assert!(state.keyboard.is_visible());

        state.home();
        assert_eq!(state.view, View::Launcher);
        assert!(!state.keyboard.is_visible());

        state.launch_system();
        state.finish_app_launch();
        state.managed_windows.push(43);
        assert!(state.show_keyboard(KeyboardPurpose::Search));
        state.replace_managed_windows(Vec::new());
        assert!(!state.keyboard.is_visible());

        state.managed_windows.push(44);
        assert!(state.show_keyboard(KeyboardPurpose::Password));
        state.lock();
        assert_eq!(state.view, View::Locked);
        assert!(!state.keyboard.is_visible());
    }

    #[test]
    fn app_focus_authorization_is_event_driven_and_fail_closed() {
        let mut state = ShellState::new(false, 50);
        state.set_active_app_focused(true);
        assert!(!state.active_app_focused());

        state.open_app(AppId::Browser);
        state.replace_managed_windows(vec![42]);
        assert!(!state.active_app_focused());
        state.set_active_app_focused(true);
        assert!(state.active_app_focused());
        assert!(public(&state).app_focused);

        state.set_active_app_focused(false);
        assert!(!state.active_app_focused());
        state.set_active_app_focused(true);
        state.home();
        assert!(!state.active_app_focused());
        state.lock();
        assert!(!state.active_app_focused());
    }

    #[test]
    fn password_keyboard_public_state_has_no_typed_content() {
        let mut state = ShellState::new(false, 50);
        state.launch_system();
        state.finish_app_launch();
        state.managed_windows.push(42);
        assert!(state.show_keyboard(KeyboardPurpose::Password));
        assert!(matches!(
            state.activate_keyboard_key(KeyAction::Character('x')),
            KeyboardEffect::Inject(crate::keyboard::KeyboardInput::Character('x'))
        ));

        let public = serde_json::to_value(public(&state)).unwrap();
        assert_eq!(
            public.get("keyboard").unwrap(),
            &serde_json::json!({
                "visible": true,
                "purpose": "password",
                "shift": false,
                "layout": "letters"
            })
        );
        assert_eq!(state.last_action, "keyboard_input");
    }

    #[test]
    fn keyboard_request_and_app_launch_are_blocked_on_lock() {
        let mut state = ShellState::new(true, 50);
        state.managed_windows.push(42);
        assert!(!state.show_keyboard(KeyboardPurpose::Url));
        state.open_app(AppId::Browser);
        state.launch_system();
        assert_eq!(state.view, View::Locked);
        assert!(!state.keyboard.is_visible());
        assert_eq!(public(&state).current_app, None);
        assert_eq!(state.last_action, "keyboard_show_blocked");
    }
}
