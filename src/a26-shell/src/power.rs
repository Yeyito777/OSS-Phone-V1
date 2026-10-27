//! Power: the power menu (hold the power key: slide to power off, Restart,
//! Cancel, as on iPhone), the clean power-off and restart through the
//! supervisor's `moon-power` helper, the critical-battery countdown sheet,
//! and the charging card shown on the lock screen when plugged in. The
//! battery policy is the supervisor's (docs/power.md): it posts the
//! low-battery notifications and `battery-critical`, and powers off by
//! itself; Moon only shows them.

use std::time::{Duration, Instant};

use crate::gfx::{Canvas, Path, RectF};
use crate::model::ShellState;
use crate::motion::Spring;
use crate::notify_client::Haptic;
use crate::text::{self, Weight};
use crate::ui::{GREEN, RED, SECONDARY, SECONDARY_ALPHA, WHITE};

pub const SCREEN_W: f32 = 1080.0;
pub const SCREEN_H: f32 = 2340.0;
/// Holding the power key this long opens the menu.
pub const HOLD_FOR_MENU: Duration = Duration::from_millis(900);

/// The slider: its track and knob.
pub const TRACK: RectF = RectF::new(90.0, 230.0, 900.0, 188.0);
pub const KNOB: f32 = 172.0;
const KNOB_INSET: f32 = 8.0;
pub const KNOB_TRAVEL: f32 = TRACK.w - KNOB - 2.0 * KNOB_INSET;
/// Round buttons: Restart (or Power Off, when restarting is chosen) and
/// Cancel.
const BUTTON_R: f32 = 98.0;
pub const SWITCH_CENTER: (f32, f32) = (540.0, 1480.0);
pub const CANCEL_CENTER: (f32, f32) = (540.0, 1960.0);

/// The supervisor's critical-battery notification (app `moon`): Moon shows
/// a full-screen countdown to its `expires_at`, the shutdown time.
pub const CRITICAL_TAG: &str = "battery-critical";
/// The charging card on the lock screen.
pub const CHARGING_CARD_FOR: Duration = Duration::from_millis(2600);
pub const CARD: RectF = RectF::new(330.0, 960.0, 420.0, 420.0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PowerAction {
    PowerOff,
    Restart,
}

impl PowerAction {
    pub fn hook_argument(self) -> &'static str {
        match self {
            PowerAction::PowerOff => "poweroff",
            PowerAction::Restart => "reboot",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuButton {
    Switch,
    Cancel,
}

#[derive(Debug)]
enum MenuTouch {
    Knob { start_x: f32, offset: f32 },
    Button(Option<MenuButton>),
}

#[derive(Debug)]
pub struct PowerMenu {
    /// The curtain's edge (drops from the top).
    edge: Spring,
    pub open: bool,
    /// What the slider does: power off, or restart (after tapping Restart).
    pub action: PowerAction,
    knob_drag: Option<f32>,
    knob_spring: Option<Spring>,
    touch: Option<MenuTouch>,
    pub pressed: Option<MenuButton>,
}

impl PowerMenu {
    fn new(action: PowerAction, now: Instant) -> Self {
        Self {
            edge: Spring::new(0.0, SCREEN_H, 0.0, now, 0.42, 1.0),
            open: true,
            action,
            knob_drag: None,
            knob_spring: None,
            touch: None,
            pressed: None,
        }
    }

    pub fn edge(&self, now: Instant) -> f32 {
        self.edge.value(now).clamp(0.0, SCREEN_H)
    }

    pub fn knob_offset(&self, now: Instant) -> f32 {
        if let Some(offset) = self.knob_drag {
            return offset;
        }
        self.knob_spring
            .map_or(0.0, |spring| spring.value(now).clamp(0.0, KNOB_TRAVEL))
    }

    fn close(&mut self, now: Instant) {
        if self.open {
            self.open = false;
            self.touch = None;
            self.pressed = None;
            self.edge = Spring::new(self.edge(now), 0.0, 0.0, now, 0.36, 1.0);
        }
    }

    fn animating(&self, now: Instant) -> bool {
        !self.edge.settled(now, 0.5)
            || self
                .knob_spring
                .is_some_and(|spring| !spring.settled(now, 0.5))
    }
}

#[derive(Debug, Default)]
pub struct PowerUi {
    pub menu: Option<PowerMenu>,
    /// The power key is down (screen on, nothing ringing): since when.
    pub key_down: Option<Instant>,
    /// The menu was opened by the current hold: its release does nothing.
    pub held_open: bool,
    /// Powering off or restarting: the screen says so until the end.
    pub shutting_down: Option<PowerAction>,
    /// For the main loop: run the supervisor's hook.
    pub pending_action: Option<PowerAction>,
    /// The helper failed: say so for a moment, then carry on.
    pub failed_at: Option<Instant>,
    pub charging_card: Option<Instant>,
}

impl PowerUi {
    pub fn menu_visible(&self, now: Instant) -> bool {
        self.menu
            .as_ref()
            .is_some_and(|menu| menu.open || menu.edge(now) > 0.5)
    }

    pub fn charging_card_alpha(&self, now: Instant) -> f32 {
        let Some(at) = self.charging_card else {
            return 0.0;
        };
        let t = now.saturating_duration_since(at).as_secs_f32();
        let total = CHARGING_CARD_FOR.as_secs_f32();
        (t / 0.18).min((total - t) / 0.3).clamp(0.0, 1.0)
    }
}

impl ShellState {
    /// The supervisor's critical-battery notification: when it powers off
    /// (Unix seconds).
    pub fn critical_battery(&self) -> Option<i64> {
        self.alerts
            .store
            .notifications
            .iter()
            .find(|n| n.app == "moon" && n.tag.as_deref() == Some(CRITICAL_TAG))
            .map(|n| n.expires_at.unwrap_or(0))
    }

    /// The shutting-down screen or the critical-battery sheet is up.
    pub fn power_takes_over(&self) -> bool {
        self.power.shutting_down.is_some() || self.critical_battery().is_some()
    }

    /// Seconds until the supervisor powers off for a critical battery.
    pub fn critical_seconds(&self) -> Option<u64> {
        let at = self.critical_battery()?;
        Some((at - crate::clock::unix_now()).max(0) as u64)
    }

    /// The power key went down with the screen on (and nothing ringing).
    pub fn power_key_down(&mut self, now: Instant) {
        if self.power_takes_over() {
            return;
        }
        if self.power.menu.as_ref().is_some_and(|menu| menu.open) {
            // As on iPhone, the power key dismisses the menu.
            self.close_power_menu();
            self.power.held_open = true;
            return;
        }
        self.power.key_down = Some(now);
        self.power.held_open = false;
    }

    /// The power key came up: a short press is the usual sleep/wake.
    pub fn power_key_up(&mut self) {
        let held_open = std::mem::take(&mut self.power.held_open);
        if self.power.key_down.take().is_some() && !held_open {
            self.power_key();
        }
    }

    pub fn open_power_menu(&mut self, action: PowerAction) {
        if !self.screen_awake || self.power_takes_over() {
            return;
        }
        let now = Instant::now();
        self.alerts_cancel_touch();
        self.control_cancel_touch();
        self.call_cancel_touch();
        self.keyboard.hide();
        self.pointer = None;
        if let Some(app) = crate::model::AppId::from_view(self.view)
            && !self.alerts_cover_app(now)
        {
            self.pending_capture = Some(app);
        }
        self.power.menu = Some(PowerMenu::new(action, now));
        self.haptic(Haptic::Medium);
        self.last_action = "power_menu".into();
        self.redraw = true;
    }

    pub fn close_power_menu(&mut self) {
        if let Some(menu) = self.power.menu.as_mut() {
            menu.close(Instant::now());
            self.last_action = "power_menu_closed".into();
            self.redraw = true;
        }
    }

    /// Powers off or restarts cleanly: the screen says so, and the main
    /// loop runs the supervisor's hook.
    pub fn begin_shutdown(&mut self, action: PowerAction) {
        if self.power.shutting_down.is_some() {
            return;
        }
        self.power.shutting_down = Some(action);
        self.power.pending_action = Some(action);
        self.power.menu = None;
        self.keyboard.hide();
        self.pointer = None;
        self.last_action = match action {
            PowerAction::PowerOff => "power_off",
            PowerAction::Restart => "restart",
        }
        .into();
        self.redraw = true;
    }

    /// The helper could not be run: back to normal, with a word.
    pub fn shutdown_failed(&mut self) {
        self.power.shutting_down = None;
        self.power.failed_at = Some(Instant::now());
        self.redraw = true;
    }

    pub fn power_animating(&self, now: Instant) -> bool {
        self.power
            .menu
            .as_ref()
            .is_some_and(|menu| menu.animating(now))
            || self
                .power
                .charging_card
                .is_some_and(|at| now.saturating_duration_since(at) < CHARGING_CARD_FOR)
    }

    pub fn power_deadline(&self, now: Instant) -> Option<Instant> {
        let hold = self.power.key_down.map(|at| at + HOLD_FOR_MENU);
        // The countdown's next second.
        let countdown = self.critical_battery().map(|_| {
            let unix = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            now + Duration::from_nanos(1_000_000_000 - u64::from(unix.subsec_nanos()) + 1_000_000)
        });
        let failed = self.power.failed_at.map(|at| at + Duration::from_secs(3));
        [hold, countdown, failed].into_iter().flatten().min()
    }

    pub fn power_tick(&mut self, now: Instant) {
        if let Some(down) = self.power.key_down
            && now.saturating_duration_since(down) >= HOLD_FOR_MENU
        {
            self.power.key_down = None;
            self.power.held_open = true;
            self.open_power_menu(PowerAction::PowerOff);
        }
        if let Some(menu) = &self.power.menu
            && !menu.open
            && menu.edge(now) <= 0.5
        {
            self.power.menu = None;
            self.redraw = true;
        }
        if self.critical_battery().is_some() {
            // The countdown repaints each second (the supervisor powers off).
            self.redraw = true;
            if self.power.menu.is_some() {
                self.power.menu = None;
            }
        }
        if self
            .power
            .failed_at
            .is_some_and(|at| now.saturating_duration_since(at) >= Duration::from_secs(3))
        {
            self.power.failed_at = None;
            self.redraw = true;
        }
        if self
            .power
            .charging_card
            .is_some_and(|at| now.saturating_duration_since(at) >= CHARGING_CARD_FOR)
        {
            self.power.charging_card = None;
            self.redraw = true;
        }
    }

    /// A new battery reading: plugging a charger in lights the lock screen
    /// with the charging card.
    pub fn battery_changed(&mut self, charging: bool, was_charging: bool) {
        if !charging || was_charging {
            return;
        }
        let now = Instant::now();
        if !self.screen_awake {
            self.screen_on();
            self.notify_woke_screen(now);
        }
        if self.view == crate::model::View::Locked {
            self.power.charging_card = Some(now);
        }
        self.redraw = true;
    }

    /// A touch went down while the menu is up. Returns true when the menu
    /// (or a takeover screen) owns it.
    pub fn power_pointer_begin(&mut self, x: f32, y: f32) -> bool {
        if self.power_takes_over() {
            return true;
        }
        let now = Instant::now();
        let Some(menu) = self.power.menu.as_mut().filter(|menu| menu.open) else {
            return false;
        };
        let offset = menu.knob_offset(now);
        if knob_rect(offset).inset(-30.0).contains(x, y) {
            menu.knob_spring = None;
            menu.knob_drag = Some(offset);
            menu.touch = Some(MenuTouch::Knob { start_x: x, offset });
        } else {
            let button = button_at(x, y);
            menu.pressed = button;
            menu.touch = Some(MenuTouch::Button(button));
        }
        self.redraw = true;
        true
    }

    pub fn power_touch_active(&self) -> bool {
        self.power_takes_over()
            || self
                .power
                .menu
                .as_ref()
                .is_some_and(|menu| menu.touch.is_some())
    }

    pub fn power_pointer_move(&mut self, x: f32, y: f32) {
        let Some(menu) = self.power.menu.as_mut() else {
            return;
        };
        match menu.touch.as_mut() {
            Some(MenuTouch::Knob { start_x, offset }) => {
                menu.knob_drag = Some((*offset + x - *start_x).clamp(0.0, KNOB_TRAVEL));
                self.redraw = true;
            }
            Some(MenuTouch::Button(button)) => {
                if button.is_some() && button_at(x, y) != *button {
                    *button = None;
                    menu.pressed = None;
                    self.redraw = true;
                }
            }
            None => {}
        }
    }

    pub fn power_pointer_end(&mut self, x: f32, y: f32) {
        let now = Instant::now();
        let Some(menu) = self.power.menu.as_mut() else {
            return;
        };
        let touch = menu.touch.take();
        menu.pressed = None;
        self.redraw = true;
        match touch {
            Some(MenuTouch::Knob { .. }) => {
                let offset = menu.knob_drag.take().unwrap_or(0.0);
                if offset >= KNOB_TRAVEL * 0.85 {
                    menu.knob_drag = Some(KNOB_TRAVEL);
                    let action = menu.action;
                    self.begin_shutdown(action);
                } else {
                    menu.knob_spring = Some(Spring::new(offset, 0.0, 0.0, now, 0.35, 0.86));
                }
            }
            Some(MenuTouch::Button(Some(button))) if button_at(x, y) == Some(button) => {
                match button {
                    MenuButton::Switch => {
                        menu.action = match menu.action {
                            PowerAction::PowerOff => PowerAction::Restart,
                            PowerAction::Restart => PowerAction::PowerOff,
                        };
                        menu.knob_spring = None;
                        self.haptic(Haptic::Light);
                    }
                    MenuButton::Cancel => self.close_power_menu(),
                }
            }
            _ => {}
        }
    }

    pub fn power_cancel_touch(&mut self) {
        if let Some(menu) = self.power.menu.as_mut() {
            menu.touch = None;
            menu.pressed = None;
            menu.knob_drag = None;
        }
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

fn near(x: f32, y: f32, (cx, cy): (f32, f32)) -> bool {
    (x - cx).powi(2) + (y - cy).powi(2) <= (BUTTON_R + 40.0).powi(2)
}

fn button_at(x: f32, y: f32) -> Option<MenuButton> {
    if near(x, y, SWITCH_CENTER) {
        Some(MenuButton::Switch)
    } else if near(x, y, CANCEL_CENTER) {
        Some(MenuButton::Cancel)
    } else {
        None
    }
}

/// What the menu layer shows.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MenuKey {
    pub action: PowerAction,
    pub pressed: Option<MenuButton>,
}

/// The menu over its darkened background: the slider track with its words,
/// and the two round buttons (the knob is a layer of its own).
pub fn draw_menu(canvas: &mut Canvas, key: &MenuKey) {
    canvas.clear(0);
    canvas.fill_rrect(TRACK, TRACK.h / 2.0, WHITE, 0.16);
    let words = match key.action {
        PowerAction::PowerOff => "slide to power off",
        PowerAction::Restart => "slide to restart",
    };
    let text_x = TRACK.x + KNOB + KNOB_INSET + (TRACK.w - KNOB - KNOB_INSET) / 2.0;
    text::draw_centered(
        canvas,
        words,
        text_x,
        text::centered_baseline(TRACK.y + TRACK.h / 2.0, Weight::Regular, 54.0),
        Weight::Regular,
        54.0,
        WHITE,
        0.8,
    );
    let (switch_label, switch_glyph) = match key.action {
        PowerAction::PowerOff => ("Restart", PowerAction::Restart),
        PowerAction::Restart => ("Power Off", PowerAction::PowerOff),
    };
    for (center, label, button) in [
        (SWITCH_CENTER, switch_label, MenuButton::Switch),
        (CANCEL_CENTER, "Cancel", MenuButton::Cancel),
    ] {
        let alpha = if key.pressed == Some(button) {
            0.42
        } else {
            0.2
        };
        canvas.fill_circle(center.0, center.1, BUTTON_R, WHITE, alpha);
        match button {
            MenuButton::Switch => draw_action_glyph(canvas, center, switch_glyph, WHITE, 1.0),
            MenuButton::Cancel => draw_cross(canvas, center, 30.0, WHITE),
        }
        text::draw_centered(
            canvas,
            label,
            center.0,
            center.1 + BUTTON_R + 66.0,
            Weight::Regular,
            44.0,
            WHITE,
            0.92,
        );
    }
}

/// The slider's knob: white, with a red power glyph or a blue restart one.
pub fn draw_knob(canvas: &mut Canvas, action: PowerAction) {
    canvas.clear(0);
    let r = KNOB / 2.0;
    canvas.fill_circle(r, r, r, WHITE, 1.0);
    let color = match action {
        PowerAction::PowerOff => RED,
        PowerAction::Restart => 0x0a84ff,
    };
    draw_action_glyph(canvas, (r, r), action, color, 1.2);
}

/// The power symbol (a broken ring and a bar) or the restart arrow.
pub fn draw_action_glyph(
    canvas: &mut Canvas,
    (cx, cy): (f32, f32),
    action: PowerAction,
    color: u32,
    scale: f32,
) {
    let pi = std::f32::consts::PI;
    let r = 30.0 * scale;
    let stroke = 8.0 * scale;
    match action {
        PowerAction::PowerOff => {
            canvas.ring_arc(
                cx,
                cy,
                r,
                stroke,
                -pi / 2.0 + 0.62,
                1.5 * pi - 0.62,
                color,
                1.0,
            );
            let mut bar = Path::new();
            bar.stroke_segment((cx, cy - r - 6.0 * scale), (cx, cy - 4.0 * scale), stroke);
            canvas.fill_path(&bar, color, 1.0);
        }
        PowerAction::Restart => {
            let start = -pi / 2.0 + 0.5;
            canvas.ring_arc(cx, cy, r, stroke, start, start + 2.0 * pi - 0.9, color, 1.0);
            // The arrowhead at the arc's start, pointing along it.
            let (sin, cos) = start.sin_cos();
            let (ex, ey) = (cx + r * cos, cy + r * sin);
            let (tx, ty) = (-sin, cos);
            let (nx, ny) = (cos, sin);
            let size = 15.0 * scale;
            let mut head = Path::new();
            head.polygon(&[
                (ex - tx * size, ey - ty * size),
                (ex + nx * size + tx * 3.0, ey + ny * size + ty * 3.0),
                (ex - nx * size + tx * 3.0, ey - ny * size + ty * 3.0),
            ]);
            canvas.fill_path(&head, color, 1.0);
        }
    }
}

fn draw_cross(canvas: &mut Canvas, (cx, cy): (f32, f32), r: f32, color: u32) {
    let mut path = Path::new();
    path.stroke_segment((cx - r, cy - r), (cx + r, cy + r), 7.0);
    path.stroke_segment((cx - r, cy + r), (cx + r, cy - r), 7.0);
    canvas.fill_path(&path, color, 1.0);
}

/// What a full-screen takeover shows (redrawn when it changes).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TakeoverKey {
    ShuttingDown(PowerAction),
    Critical { seconds: u64 },
}

/// The shutting-down screen, or the critical-battery sheet with its
/// countdown, on black.
pub fn draw_takeover(canvas: &mut Canvas, key: &TakeoverKey) {
    canvas.clear(0xff00_0000);
    let cx = SCREEN_W / 2.0;
    match key {
        TakeoverKey::ShuttingDown(action) => {
            draw_action_glyph(canvas, (cx, 1050.0), *action, WHITE, 1.6);
            let words = match action {
                PowerAction::PowerOff => "Shutting down…",
                PowerAction::Restart => "Restarting…",
            };
            text::draw_centered(canvas, words, cx, 1230.0, Weight::Medium, 52.0, WHITE, 0.9);
        }
        TakeoverKey::Critical { seconds } => {
            draw_empty_battery(canvas, cx, 980.0);
            text::draw_centered(
                canvas,
                "Battery critically low",
                cx,
                1180.0,
                Weight::SemiBold,
                58.0,
                WHITE,
                1.0,
            );
            text::draw_centered(
                canvas,
                &format!("Shutting down in {seconds} s"),
                cx,
                1260.0,
                Weight::Regular,
                46.0,
                SECONDARY,
                SECONDARY_ALPHA,
            );
            text::draw_centered(
                canvas,
                "Connect a charger to keep it on.",
                cx,
                1330.0,
                Weight::Regular,
                40.0,
                SECONDARY,
                SECONDARY_ALPHA,
            );
        }
    }
}

fn draw_empty_battery(canvas: &mut Canvas, cx: f32, cy: f32) {
    let body = RectF::new(cx - 120.0, cy - 60.0, 230.0, 120.0);
    canvas.stroke_rrect(body, 28.0, 8.0, WHITE, 0.8);
    canvas.fill_rrect(
        RectF::new(body.x + body.w + 8.0, cy - 22.0, 14.0, 44.0),
        6.0,
        WHITE,
        0.8,
    );
    canvas.fill_rrect(
        RectF::new(body.x + 16.0, body.y + 16.0, 20.0, body.h - 32.0),
        8.0,
        RED,
        1.0,
    );
}

/// The charging card: a ring filled to the level, the percentage and a
/// bolt.
pub fn draw_charging_card(canvas: &mut Canvas, percent: Option<u8>) {
    canvas.clear(0);
    let r = CARD.w / 2.0;
    canvas.fill_rrect(RectF::new(0.0, 0.0, CARD.w, CARD.h), 96.0, 0x1c1c1e, 0.92);
    let level = f32::from(percent.unwrap_or(0)) / 100.0;
    let pi = std::f32::consts::PI;
    canvas.ring_arc(r, r - 20.0, 120.0, 18.0, -pi, pi, WHITE, 0.15);
    if level > 0.0 {
        canvas.ring_arc(
            r,
            r - 20.0,
            120.0,
            18.0,
            -pi / 2.0,
            -pi / 2.0 + 2.0 * pi * level,
            GREEN,
            1.0,
        );
    }
    let label = percent.map_or_else(|| "--".into(), |p| format!("{p}%"));
    text::draw_centered(
        canvas,
        &label,
        r,
        text::centered_baseline(r - 20.0, Weight::SemiBold, 64.0),
        Weight::SemiBold,
        64.0,
        WHITE,
        1.0,
    );
    text::draw_centered(
        canvas,
        "Charging",
        r,
        CARD.h - 52.0,
        Weight::Regular,
        40.0,
        SECONDARY,
        SECONDARY_ALPHA,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::View;

    #[test]
    fn holding_the_power_key_opens_the_menu_and_a_tap_still_sleeps() {
        let mut state = ShellState::new(false, 50);
        let start = Instant::now();
        state.power_key_down(start);
        state.power_tick(start + Duration::from_millis(200));
        assert!(state.power.menu.is_none());
        state.power_key_up();
        // A short press: the usual power fade.
        assert!(state.fade.is_some_and(|fade| fade.out));

        let mut state = ShellState::new(false, 50);
        state.power_key_down(start);
        state.power_tick(start + HOLD_FOR_MENU);
        assert!(state.power.menu.as_ref().is_some_and(|menu| menu.open));
        state.power_key_up();
        assert!(state.fade.is_none(), "the hold's release does nothing");
        // Restart switches the slider, which then restarts.
        let (sx, sy) = SWITCH_CENTER;
        assert!(state.power_pointer_begin(sx, sy));
        state.power_pointer_end(sx, sy);
        assert_eq!(
            state.power.menu.as_ref().unwrap().action,
            PowerAction::Restart
        );
        let (kx, ky) = knob_origin(0.0);
        assert!(state.power_pointer_begin(kx + 80.0, ky + 80.0));
        state.power_pointer_move(kx + 80.0 + KNOB_TRAVEL, ky + 80.0);
        state.power_pointer_end(kx + 80.0 + KNOB_TRAVEL, ky + 80.0);
        assert_eq!(state.power.pending_action, Some(PowerAction::Restart));
        assert_eq!(state.power.shutting_down, Some(PowerAction::Restart));

        // Cancel closes it; so does the power key.
        let mut state = ShellState::new(false, 50);
        state.open_power_menu(PowerAction::PowerOff);
        let (cx, cy) = CANCEL_CENTER;
        assert!(state.power_pointer_begin(cx, cy));
        state.power_pointer_end(cx, cy);
        assert!(!state.power.menu.as_ref().unwrap().open);
        state.open_power_menu(PowerAction::PowerOff);
        state.power_key_down(Instant::now());
        state.power_key_up();
        assert!(!state.power.menu.as_ref().unwrap().open);
        assert!(state.fade.is_none());
    }

    #[test]
    fn the_supervisors_critical_notification_takes_over_the_screen() {
        let mut state = ShellState::new(false, 50);
        state.open_power_menu(PowerAction::PowerOff);
        let event: crate::notify_client::Event = serde_json::from_value(serde_json::json!({
            "event": "notification_posted", "wake": true,
            "notification": {"id": "n7", "app": "moon", "tag": CRITICAL_TAG,
                "title": "Battery critically low", "urgency": "high", "posted_at": 1,
                "expires_at": crate::clock::unix_now() + 15}
        }))
        .unwrap();
        state.notify_event(event);
        assert!(state.power_takes_over());
        assert!((14..=15).contains(&state.critical_seconds().unwrap()));
        assert!(state.alerts.banner.is_none(), "the sheet, not a banner");
        state.power_tick(Instant::now());
        assert!(state.power.menu.is_none());
        // Plugged in: the supervisor withdraws it.
        state.notify_event(crate::notify_client::Event::NotificationRemoved { id: "n7".into() });
        assert!(!state.power_takes_over());
    }

    #[test]
    fn plugging_in_wakes_the_lock_screen_with_the_charging_card() {
        let mut state = ShellState::new(true, 50);
        state.screen_off();
        state.screen_awake = false;
        state.battery_changed(true, false);
        assert!(state.screen_awake);
        assert_eq!(state.view, View::Locked);
        assert!(state.power.charging_card.is_some());
        // Still plugged: no second card.
        state.power.charging_card = None;
        state.battery_changed(true, true);
        assert!(state.power.charging_card.is_none());
    }
}
