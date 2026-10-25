use serde::Serialize;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

use crate::config::Config;
use crate::freezer::FreezerState;
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AppId {
    System,
    Browser,
}

impl AppId {
    pub fn from_view(view: View) -> Option<Self> {
        match view {
            View::System => Some(Self::System),
            View::Browser => Some(Self::Browser),
            View::Locked | View::Launcher => None,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Browser => "Browser",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "browser" => Some(Self::Browser),
            _ => None,
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::System => 0,
            Self::Browser => 1,
        }
    }

    pub fn cgroup_name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Browser => "browser",
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
}

impl View {
    pub fn is_app(self) -> bool {
        matches!(self, Self::System | Self::Browser)
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

/// A control currently held down, drawn highlighted until release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    Keypad(crate::ui::KeypadAction),
    App(AppId),
}

/// An app opening from (or closing into) its launcher icon.
#[derive(Debug, Clone, Copy)]
pub struct Launch {
    pub app: AppId,
    pub started: Instant,
    /// A background app being brought back: its window already exists.
    pub resumed: bool,
    pub window_mapped_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy)]
pub struct Closing {
    pub app: AppId,
    pub started: Instant,
}

/// Duration of the icon-to-fullscreen zoom when an app opens.
pub const OPEN_ANIMATION: Duration = Duration::from_millis(340);
/// Duration of the fullscreen-to-icon zoom when an app goes home.
pub const CLOSE_ANIMATION: Duration = Duration::from_millis(300);
/// Wrong-passcode shake.
pub const SHAKE_ANIMATION: Duration = Duration::from_millis(480);
/// The passcode keypad returns to the clock after this much inactivity.
pub const PASSCODE_IDLE_TIMEOUT: Duration = Duration::from_secs(12);
/// Vimbrowser's first frames are painted by a software compositor; keep the
/// launch screen up briefly after its window maps so no blank frame shows.
const BROWSER_FIRST_PAINT_DELAY: Duration = Duration::from_millis(1500);
const SYSTEM_FIRST_PAINT_DELAY: Duration = Duration::from_millis(80);

#[derive(Debug, Clone, Copy)]
pub struct PointerGesture {
    pub start_x: i16,
    pub start_y: i16,
    pub last_x: i16,
    pub last_y: i16,
    pub started: Instant,
    pub keyboard_owned: bool,
    pub keyboard_key_index: Option<usize>,
    pub keyboard_pressed: bool,
    pub keyboard_initial_sent: bool,
    pub keyboard_repeat_uppercase: bool,
    pub keyboard_next_repeat_at: Option<Instant>,
}

#[derive(Debug)]
pub struct ShellState {
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
    pub managed_windows: Vec<u32>,
    pub keyboard: KeyboardState,
    pub lock_face: LockFace,
    pub last_lock_input: Instant,
    pub pressed: Option<Press>,
    pub pin_error_at: Option<Instant>,
    pub closing: Option<Closing>,
    /// The shell window was exposed and must be recopied from its pixmap.
    pub shell_exposed: bool,
    active_app_focused: bool,
    launch: Option<Launch>,
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
}

impl ShellState {
    pub fn new(start_locked: bool, initial_volume: u8) -> Self {
        Self {
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
            managed_windows: Vec::new(),
            keyboard: KeyboardState::default(),
            lock_face: LockFace::Clock,
            last_lock_input: Instant::now(),
            pressed: None,
            pin_error_at: None,
            closing: None,
            shell_exposed: false,
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
            current_app: match self.view {
                View::System => Some("System"),
                View::Browser => Some("Browser"),
                View::Locked | View::Launcher => None,
            },
            volume: self.volume,
            battery_percent: self.battery_percent,
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
        }
    }

    pub fn lock(&mut self) {
        self.keyboard.hide();
        self.view = View::Locked;
        self.lock_face = LockFace::Clock;
        self.clear_pin();
        self.pointer = None;
        self.pressed = None;
        self.closing = None;
        self.pin_error_at = None;
        self.active_app_focused = false;
        self.cancel_app_launch();
        self.last_action = "lock".into();
        self.redraw = true;
    }

    pub fn screen_off(&mut self) {
        self.lock();
        self.screen_awake = false;
        self.last_action = "screen_off".into();
    }

    pub fn screen_on(&mut self) {
        self.lock();
        self.screen_awake = true;
        self.last_action = "screen_on".into();
    }

    pub fn toggle_screen(&mut self) {
        if self.screen_awake {
            self.screen_off();
        } else {
            self.screen_on();
        }
    }

    pub fn home(&mut self) {
        if self.screen_awake && self.view != View::Locked {
            self.keyboard.hide();
            if let Some(app) = AppId::from_view(self.view) {
                self.closing = Some(Closing {
                    app,
                    started: Instant::now(),
                });
            }
            self.pressed = None;
            self.view = View::Launcher;
            self.active_app_focused = false;
            self.cancel_app_launch();
            self.last_action = "home".into();
            self.redraw = true;
        }
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
        self.last_action = match app {
            AppId::System => "system_resumed",
            AppId::Browser => "browser_resumed",
        }
        .into();
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

    pub fn launch_browser(&mut self) {
        self.begin_launch(AppId::Browser);
    }

    fn begin_launch(&mut self, app: AppId) {
        let view = match app {
            AppId::System => View::System,
            AppId::Browser => View::Browser,
        };
        if self.screen_awake && self.view != View::Locked && self.view != view {
            self.keyboard.hide();
            self.view = view;
            self.active_app_focused = false;
            self.pressed = None;
            self.closing = None;
            self.managed_windows.clear();
            self.launch = Some(Launch {
                app,
                started: Instant::now(),
                resumed: false,
                window_mapped_at: None,
            });
            self.last_action = match app {
                AppId::System => "launch_system",
                AppId::Browser => "launch_browser",
            }
            .into();
            self.redraw = true;
        }
    }

    /// Reveals the passcode keypad over the lock-screen clock.
    pub fn show_passcode(&mut self) {
        if self.screen_awake && self.view == View::Locked && self.lock_face != LockFace::Passcode {
            self.lock_face = LockFace::Passcode;
            self.last_lock_input = Instant::now();
            self.last_action = "passcode_shown".into();
            self.redraw = true;
        }
    }

    /// Returns from the passcode keypad to the lock-screen clock.
    pub fn cancel_passcode(&mut self) {
        if self.view == View::Locked && self.lock_face == LockFace::Passcode {
            self.lock_face = LockFace::Clock;
            self.clear_pin();
            self.pin_error_at = None;
            self.last_action = "passcode_cancel".into();
            self.redraw = true;
        }
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
            self.view = View::Launcher;
            self.lock_face = LockFace::Clock;
            self.pin_error_at = None;
            self.failed_attempts = 0;
            self.lockout_until = None;
            self.last_action = "unlock".into();
        } else {
            self.failed_attempts = self.failed_attempts.saturating_add(1);
            self.pin_error_at = Some(Instant::now());
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
        }
        .into();
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

    pub fn app_launch_elapsed(&self) -> Duration {
        self.launch()
            .map(|launch| launch.started.elapsed())
            .unwrap_or_default()
    }

    pub fn note_app_window_mapped(&mut self) {
        let view = self.view;
        if let Some(launch) = self.launch.as_mut()
            && AppId::from_view(view) == Some(launch.app)
            && launch.window_mapped_at.is_none()
        {
            launch.window_mapped_at = Some(Instant::now());
            self.last_action = match launch.app {
                AppId::System => "system_window_warming",
                AppId::Browser => "browser_window_warming",
            }
            .into();
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
                AppId::System => SYSTEM_FIRST_PAINT_DELAY,
                AppId::Browser => BROWSER_FIRST_PAINT_DELAY,
            }
        };
        Some((launch.started + OPEN_ANIMATION).max(mapped_at + paint_delay))
    }

    pub fn app_ready_to_reveal(&self) -> bool {
        self.reveal_at()
            .is_some_and(|reveal_at| Instant::now() >= reveal_at)
    }

    pub fn finish_app_launch(&mut self) {
        let Some(launch) = self.launch() else {
            return;
        };
        self.launch = None;
        self.last_action = match launch.app {
            AppId::System => "system_ready",
            AppId::Browser => "browser_ready",
        }
        .into();
        self.redraw = true;
    }

    fn cancel_app_launch(&mut self) {
        self.launch = None;
    }

    /// True while a shell animation needs frames (zooms, shake).
    pub fn animating(&self, now: Instant) -> bool {
        if !self.screen_awake {
            return false;
        }
        let opening = self
            .launch()
            .is_some_and(|launch| now < launch.started + OPEN_ANIMATION);
        let closing = self.view == View::Launcher
            && self
                .closing
                .is_some_and(|closing| now < closing.started + CLOSE_ANIMATION);
        let shaking = self.view == View::Locked
            && self
                .pin_error_at
                .is_some_and(|at| now < at + SHAKE_ANIMATION);
        opening || closing || shaking
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let _ = self.is_lockout_active();
        if self.view == View::Locked
            && self.lock_face == LockFace::Passcode
            && now.duration_since(self.last_lock_input) >= PASSCODE_IDLE_TIMEOUT
            && self.pointer.is_none()
        {
            self.cancel_passcode();
            self.last_action = "passcode_idle_timeout".into();
        }
        if self
            .closing
            .is_some_and(|closing| now >= closing.started + CLOSE_ANIMATION)
        {
            self.closing = None;
            self.redraw = true;
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
        [
            self.lockout_until,
            self.volume_overlay_until,
            self.reveal_at(),
            passcode_timeout,
        ]
        .into_iter()
        .flatten()
        .filter(|deadline| *deadline > now)
        .min()
    }
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
        state.launch_browser();
        assert_eq!(state.view, View::Browser);
        assert_eq!(public(&state).current_app, Some("Browser"));
        assert!(state.view.is_app());
        assert!(state.app_launching());

        state.note_app_window_mapped();
        assert!(!state.app_ready_to_reveal());
        let launch = state.launch.as_mut().unwrap();
        launch.started -= Duration::from_secs(2);
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
        launch.started -= OPEN_ANIMATION;
        launch.window_mapped_at = Some(Instant::now() - Duration::from_millis(100));
        assert!(state.app_ready_to_reveal());
        state.finish_app_launch();
        assert!(!public(&state).app_launching);
        assert!(!state.animating(Instant::now()));
    }

    #[test]
    fn resumed_app_reveals_without_waiting_for_first_paint() {
        let mut state = ShellState::new(false, 50);
        state.launch_browser();
        state.note_app_resumed(AppId::Browser);
        state.launch.as_mut().unwrap().started -= OPEN_ANIMATION;
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
        state.closing.as_mut().unwrap().started -= CLOSE_ANIMATION;
        state.tick();
        assert!(state.closing.is_none());
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

        state.launch_browser();
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
        state.launch_browser();
        state.launch_system();
        assert_eq!(state.view, View::Locked);
        assert!(!state.keyboard.is_visible());
        assert_eq!(public(&state).current_app, None);
        assert_eq!(state.last_action, "keyboard_show_blocked");
    }
}
