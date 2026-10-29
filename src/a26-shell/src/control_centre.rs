//! Control Centre: quick settings pulled down from the top-right edge, as
//! on iPhone (the notification centre comes from the rest of the top edge).
//! Wi-Fi, Do Not Disturb (moon-notifyd), the flashlight, the next alarm,
//! and brightness and volume sliders. It is a curtain like the
//! notification centre: its content stays put while the edge follows the
//! finger, so a frame redraws only the strip the edge crossed; a slider
//! being dragged redraws only itself.

use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use crate::alerts::{Outgoing, PULL_ZONE};
use crate::display::DisplayRequest;
use crate::gfx::{Canvas, Path, RectF};
use crate::model::{ShellState, View};
use crate::motion::{Spring, VelocityTracker};
use crate::text::{self, Weight};
use crate::ui::{SECONDARY, SECONDARY_ALPHA, WHITE};

pub const SCREEN_H: f32 = 2340.0;
/// A pull starting right of this on the top edge opens Control Centre.
pub const SPLIT_X: f32 = 700.0;

const BLOCK: RectF = RectF::new(40.0, 260.0, 480.0, 480.0);
pub const BRIGHTNESS: RectF = RectF::new(560.0, 260.0, 210.0, 480.0);
pub const VOLUME: RectF = RectF::new(830.0, 260.0, 210.0, 480.0);
const TILE: f32 = 136.0;
const TAP_SLOP: f32 = 30.0;
const PURPLE: u32 = 0x5e5ce6;
const BLUE: u32 = 0x0a84ff;
const ORANGE: u32 = 0xff9f0a;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tile {
    Wifi,
    Dnd,
    Flashlight,
    Alarm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Slider {
    Brightness,
    Volume,
}

impl Slider {
    pub fn rect(self) -> RectF {
        match self {
            Slider::Brightness => BRIGHTNESS,
            Slider::Volume => VOLUME,
        }
    }
}

fn tile_center(tile: Tile) -> (f32, f32) {
    let (column, row) = match tile {
        Tile::Wifi => (0, 0),
        Tile::Dnd => (1, 0),
        Tile::Flashlight => (0, 1),
        Tile::Alarm => (1, 1),
    };
    (
        BLOCK.x + BLOCK.w * (0.25 + 0.5 * column as f32),
        BLOCK.y + BLOCK.h * (0.22 + 0.5 * row as f32),
    )
}

fn tile_at(x: f32, y: f32) -> Option<Tile> {
    [Tile::Wifi, Tile::Dnd, Tile::Flashlight, Tile::Alarm]
        .into_iter()
        .find(|tile| {
            let (cx, cy) = tile_center(*tile);
            (x - cx).abs() < 110.0 && (y - cy).abs() < 130.0
        })
}

/// The torch LED (s2mf301-fled on the A26), driven through whichever of
/// the usual sysfs controls exists; none means no flashlight tile.
#[derive(Debug)]
pub struct Flashlight {
    path: PathBuf,
    pub on: bool,
}

impl Flashlight {
    pub fn probe() -> Option<Self> {
        let candidates = std::env::var_os("MOON_TORCH")
            .map(|path| vec![PathBuf::from(path)])
            .unwrap_or_else(|| {
                [
                    "/sys/class/camera/flash/rear_flash",
                    "/sys/class/leds/torch-light0/brightness",
                    "/sys/class/leds/torch/brightness",
                    "/sys/class/leds/flash/brightness",
                ]
                .iter()
                .map(PathBuf::from)
                .collect()
            });
        candidates
            .into_iter()
            .find(|path| path.exists())
            .map(|path| Self { path, on: false })
    }

    fn set(&mut self, on: bool) {
        match fs::write(&self.path, if on { "1\n" } else { "0\n" }) {
            Ok(()) => self.on = on,
            Err(error) => eprintln!("flashlight {}: {error}", self.path.display()),
        }
    }
}

#[derive(Debug)]
enum Touch {
    Pull {
        start_y: f32,
        start_edge: f32,
        moved: bool,
        tracker: VelocityTracker,
    },
    Slider {
        slider: Slider,
        start_y: f32,
        start_value: f32,
        moved: bool,
    },
    Tile(Tile),
}

#[derive(Debug)]
pub struct ControlCentre {
    edge: Spring,
    drag: Option<f32>,
    pub open: bool,
    touch: Option<Touch>,
    pub pressed: Option<Tile>,
    /// A slider held by a finger and its live value (0-100).
    pub dragging: Option<(Slider, u8)>,
    pub flashlight: Option<Flashlight>,
    /// A volume chosen here, for the main loop to apply.
    pub pending_volume: Option<u8>,
}

impl Default for ControlCentre {
    fn default() -> Self {
        Self {
            edge: Spring::new(0.0, 0.0, 0.0, Instant::now(), 0.35, 1.0),
            drag: None,
            open: false,
            touch: None,
            pressed: None,
            dragging: None,
            flashlight: None,
            pending_volume: None,
        }
    }
}

impl ControlCentre {
    pub fn edge(&self, now: Instant) -> f32 {
        self.drag
            .unwrap_or_else(|| self.edge.value(now))
            .clamp(0.0, SCREEN_H)
    }

    pub fn visible(&self, now: Instant) -> bool {
        self.drag.is_some() || self.open || self.edge(now) > 0.5
    }

    pub fn animating(&self, now: Instant) -> bool {
        self.drag.is_none() && !self.edge.settled(now, 0.5)
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
    }

    /// The level a slider shows: the finger's while dragged.
    pub fn slider_value(
        &self,
        slider: Slider,
        display: Option<crate::display::DisplayView>,
        volume: u8,
    ) -> Option<u8> {
        if let Some((held, value)) = self.dragging
            && held == slider
        {
            return Some(value);
        }
        match slider {
            Slider::Brightness => display.map(|view| view.brightness),
            Slider::Volume => Some(volume),
        }
    }
}

impl ShellState {
    /// A touch went down: Control Centre takes it when it is shown, or for
    /// a pull from the right of the top edge. Returns true when it did.
    pub fn control_pointer_begin(&mut self, x: f32, y: f32) -> bool {
        let now = Instant::now();
        if self.alerts.ringing().is_some() || !self.screen_awake {
            return false;
        }
        if self.control.visible(now) {
            if let Some(tile) = tile_at(x, y) {
                self.control.pressed = Some(tile);
                self.control.touch = Some(Touch::Tile(tile));
            } else if let Some(slider) = [Slider::Brightness, Slider::Volume]
                .into_iter()
                .find(|s| s.rect().contains(x, y))
            {
                let value = self
                    .control
                    .slider_value(slider, self.display_view, self.volume)
                    .unwrap_or(50);
                self.control.touch = Some(Touch::Slider {
                    slider,
                    start_y: y,
                    start_value: f32::from(value),
                    moved: false,
                });
            } else {
                let mut tracker = VelocityTracker::default();
                tracker.push(now, x, y);
                self.control.touch = Some(Touch::Pull {
                    start_y: y,
                    start_edge: self.control.edge(now),
                    moved: false,
                    tracker,
                });
            }
            self.redraw = true;
            return true;
        }
        let allowed = y <= PULL_ZONE
            && x >= SPLIT_X
            && !self.home_button_at(x, y)
            && self.switcher.is_none()
            && self.gesture.is_none()
            && !self.app_launching()
            && self.fade.is_none()
            && !self.alerts.centre.visible(now);
        if allowed {
            let mut tracker = VelocityTracker::default();
            tracker.push(now, x, y);
            self.control.touch = Some(Touch::Pull {
                start_y: y,
                start_edge: 0.0,
                moved: false,
                tracker,
            });
            return true;
        }
        false
    }

    pub fn control_touch_active(&self) -> bool {
        self.control.touch.is_some()
    }

    pub fn control_pointer_move(&mut self, x: f32, y: f32) {
        let now = Instant::now();
        let view = self.view;
        match self.control.touch.as_mut() {
            Some(Touch::Pull {
                start_y,
                start_edge,
                moved,
                tracker,
            }) => {
                tracker.push(now, x, y);
                let dy = y - *start_y;
                if !*moved && dy.abs() > TAP_SLOP {
                    *moved = true;
                    if *start_edge == 0.0
                        && let Some(app) = crate::model::AppId::from_view(view)
                    {
                        self.pending_capture = Some(app);
                    }
                    self.keyboard.hide();
                }
                if *moved {
                    self.control.drag = Some((*start_edge + dy).clamp(0.0, SCREEN_H));
                }
            }
            Some(Touch::Slider {
                slider,
                start_y,
                start_value,
                moved,
            }) => {
                let dy = y - *start_y;
                if dy.abs() > 6.0 {
                    *moved = true;
                }
                let value = (*start_value - dy / slider.rect().h * 100.0).clamp(0.0, 100.0);
                let value = value.round() as u8;
                let slider = *slider;
                let previous = self.control.dragging.map(|(_, v)| v);
                if (value == 0 || value == 100) && previous.is_some_and(|p| p != value) {
                    // A tick as the slider reaches either end.
                    self.haptic(crate::notify_client::Haptic::Selection);
                }
                if self.control.dragging != Some((slider, value)) {
                    self.control.dragging = Some((slider, value));
                    match slider {
                        Slider::Brightness => {
                            self.display_requests.push(DisplayRequest::Set(value))
                        }
                        Slider::Volume => self.control.pending_volume = Some(value),
                    }
                }
            }
            Some(Touch::Tile(tile)) => {
                if tile_at(x, y) != Some(*tile) {
                    self.control.pressed = None;
                }
            }
            None => return,
        }
        self.redraw = true;
    }

    pub fn control_pointer_end(&mut self, x: f32, y: f32) {
        let now = Instant::now();
        let Some(touch) = self.control.touch.take() else {
            return;
        };
        match touch {
            Touch::Pull {
                start_edge,
                moved,
                mut tracker,
                ..
            } => {
                tracker.push(now, x, y);
                if !moved {
                    if start_edge > 0.0 {
                        // A tap on the background closes it.
                        self.control.settle(false, 0.0, now);
                    }
                    self.control.drag = None;
                } else {
                    let velocity = tracker.velocity(now).1;
                    let edge = self.control.edge(now);
                    let open = if velocity.abs() > 400.0 {
                        velocity > 0.0
                    } else {
                        edge > SCREEN_H * if start_edge > 0.0 { 0.7 } else { 0.3 }
                    };
                    self.control.settle(open, velocity, now);
                    self.last_action = if open {
                        "control_centre_open"
                    } else {
                        "control_centre_closed"
                    }
                    .into();
                }
            }
            Touch::Slider {
                slider,
                start_value,
                moved,
                ..
            } => {
                self.control.dragging = None;
                if !moved && slider == Slider::Brightness {
                    // A tap on the brightness slider's glyph toggles auto
                    // when the light sensor is there.
                    let rect = slider.rect();
                    if y > rect.y + rect.h - 120.0
                        && let Some(view) = self.display_view
                        && view.sensor_available
                    {
                        self.display_requests.push(DisplayRequest::Auto(!view.auto));
                    }
                }
                let _ = start_value;
            }
            Touch::Tile(tile) => {
                self.control.pressed = None;
                if tile_at(x, y) == Some(tile) {
                    self.control_tile(tile);
                }
            }
        }
        self.redraw = true;
    }

    fn control_tile(&mut self, tile: Tile) {
        let now = Instant::now();
        self.haptic(crate::notify_client::Haptic::Light);
        match tile {
            Tile::Dnd => {
                let on = !self.alerts.store.do_not_disturb;
                self.alerts.store.do_not_disturb = on;
                self.alerts.outbox.push(Outgoing::SetDnd(on));
                self.last_action = if on { "dnd_on" } else { "dnd_off" }.into();
            }
            Tile::Flashlight => {
                if let Some(flashlight) = self.control.flashlight.as_mut() {
                    let on = !flashlight.on;
                    flashlight.set(on);
                    self.last_action = if on {
                        "flashlight_on"
                    } else {
                        "flashlight_off"
                    }
                    .into();
                }
            }
            Tile::Wifi | Tile::Alarm => {
                // Their settings live in the System app.
                self.control.settle(false, 0.0, now);
                if self.view == View::Locked {
                    self.show_passcode();
                } else if self.view != View::System {
                    self.launch_system();
                }
            }
        }
    }

    pub fn control_cancel_touch(&mut self) {
        self.control.touch = None;
        self.control.drag = None;
        self.control.pressed = None;
        self.control.dragging = None;
    }

    /// Screen off, or an alarm ringing: it closes.
    pub fn control_tick(&mut self, now: Instant) {
        let close = !self.screen_awake || self.alerts.ringing().is_some();
        if close && self.control.visible(now) {
            self.control.open = false;
            self.control.drag = None;
            self.control.edge = Spring::new(0.0, 0.0, 0.0, now, 0.35, 1.0);
            self.control.touch = None;
        }
    }
}

/// What the Control Centre layer shows; it is redrawn only when this
/// changes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ControlKey {
    pub wifi: Option<String>,
    pub dnd: bool,
    pub flashlight: Option<bool>,
    pub alarm: Option<String>,
    pub pressed: Option<Tile>,
}

/// The static part: the toggles block (sliders are separate layers).
pub fn draw_control(canvas: &mut Canvas, key: &ControlKey) {
    canvas.clear(0);
    canvas.fill_rrect(BLOCK, 60.0, WHITE, 0.14);
    for tile in [Tile::Wifi, Tile::Dnd, Tile::Flashlight, Tile::Alarm] {
        let (cx, cy) = tile_center(tile);
        let (on, color, label, enabled) = match tile {
            Tile::Wifi => (
                key.wifi.is_some(),
                BLUE,
                key.wifi.clone().unwrap_or_else(|| "Wi-Fi".into()),
                true,
            ),
            Tile::Dnd => (key.dnd, PURPLE, "Focus".into(), true),
            Tile::Flashlight => (
                key.flashlight == Some(true),
                WHITE,
                "Flashlight".into(),
                key.flashlight.is_some(),
            ),
            Tile::Alarm => (
                key.alarm.is_some(),
                ORANGE,
                key.alarm.clone().unwrap_or_else(|| "No Alarm".into()),
                true,
            ),
        };
        let alpha =
            if key.pressed == Some(tile) { 0.6 } else { 1.0 } * if enabled { 1.0 } else { 0.35 };
        if on {
            canvas.fill_circle(cx, cy, TILE / 2.0, color, alpha);
        } else {
            canvas.fill_circle(cx, cy, TILE / 2.0, WHITE, 0.18 * alpha);
        }
        // White glyphs, except dark on the lit (white) flashlight.
        let ink = if tile == Tile::Flashlight && on {
            0x000000
        } else {
            WHITE
        };
        match tile {
            Tile::Wifi => crate::ui::draw_wifi_glyph_in(canvas, cx, cy + 22.0, true, ink),
            Tile::Dnd => draw_moon(canvas, (cx, cy), 30.0, ink),
            Tile::Flashlight => draw_torch(canvas, (cx, cy), ink),
            Tile::Alarm => crate::alerts::draw_clock_glyph(canvas, (cx, cy + 4.0), 28.0, ink),
        }
        let label = text::fit(&label, Weight::Medium, 32.0, 210.0);
        text::draw_centered(
            canvas,
            &label,
            cx,
            cy + TILE / 2.0 + 48.0,
            Weight::Medium,
            32.0,
            WHITE,
            0.9 * if enabled { 1.0 } else { 0.5 },
        );
    }
    text::draw_centered(
        canvas,
        "Swipe up to close",
        540.0,
        2226.0,
        Weight::Regular,
        38.0,
        SECONDARY,
        SECONDARY_ALPHA,
    );
}

/// A vertical slider: filled from the bottom to `value` (0-100); None when
/// its control is unavailable.
pub fn draw_slider(canvas: &mut Canvas, slider: Slider, value: Option<u8>, auto: Option<bool>) {
    canvas.clear(0);
    let rect = RectF::new(0.0, 0.0, slider.rect().w, slider.rect().h);
    canvas.fill_rrect(rect, 60.0, WHITE, if value.is_some() { 0.14 } else { 0.07 });
    let fraction = f32::from(value.unwrap_or(0)) / 100.0;
    let fill_top = (rect.h * (1.0 - fraction)).round() as i32;
    canvas.set_clip(0, fill_top, canvas.width, canvas.height);
    canvas.fill_rrect(rect, 60.0, WHITE, 0.92);
    canvas.reset_clip();
    let (cx, cy) = (rect.w / 2.0, rect.h - 70.0);
    let filled_here = cy + 30.0 > rect.h * (1.0 - fraction);
    let ink = if filled_here { 0x636366 } else { WHITE };
    match slider {
        Slider::Brightness => {
            draw_sun(canvas, (cx, cy), ink);
            if let Some(auto) = auto {
                text::draw_centered(
                    canvas,
                    if auto { "AUTO" } else { "" },
                    cx,
                    cy - 52.0,
                    Weight::Bold,
                    24.0,
                    ink,
                    0.9,
                );
            }
        }
        Slider::Volume => crate::ui::draw_speaker(canvas, cx, cy, value.unwrap_or(0), ink),
    }
}

fn draw_sun(canvas: &mut Canvas, (cx, cy): (f32, f32), color: u32) {
    canvas.fill_circle(cx, cy, 13.0, color, 1.0);
    let mut rays = Path::new();
    for i in 0..8 {
        let angle = i as f32 * std::f32::consts::TAU / 8.0;
        let (s, c) = angle.sin_cos();
        rays.stroke_segment(
            (cx + c * 21.0, cy + s * 21.0),
            (cx + c * 29.0, cy + s * 29.0),
            5.0,
        );
    }
    canvas.fill_path(&rays, color, 1.0);
}

/// A crescent: a disc with an offset disc taken out (a coverage mask, so
/// it sits cleanly on any background).
fn draw_moon(canvas: &mut Canvas, (cx, cy): (f32, f32), r: f32, color: u32) {
    let (bx, by, cut) = (cx + r * 0.45, cy - r * 0.35, r * 0.82);
    let x0 = (cx - r - 1.0).floor() as i32;
    let y0 = (cy - r - 1.0).floor() as i32;
    let size = (2.0 * r + 3.0).ceil() as i32;
    let mut mask = vec![0_u8; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let (px, py) = ((x0 + x) as f32 + 0.5, (y0 + y) as f32 + 0.5);
            let disc = (r - ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() + 0.5).clamp(0.0, 1.0);
            let bite = (cut - ((px - bx).powi(2) + (py - by).powi(2)).sqrt() + 0.5).clamp(0.0, 1.0);
            mask[(y * size + x) as usize] = (disc * (1.0 - bite) * 255.0 + 0.5) as u8;
        }
    }
    canvas.draw_mask(x0, y0, size, size, &mask, color, 1.0);
}

fn draw_torch(canvas: &mut Canvas, (cx, cy): (f32, f32), color: u32) {
    let mut head = Path::new();
    head.polygon(&[
        (cx - 22.0, cy - 34.0),
        (cx + 22.0, cy - 34.0),
        (cx + 14.0, cy - 12.0),
        (cx - 14.0, cy - 12.0),
    ]);
    canvas.fill_path(&head, color, 1.0);
    canvas.fill_rrect(
        RectF::new(cx - 13.0, cy - 10.0, 26.0, 48.0),
        5.0,
        color,
        1.0,
    );
}

/// The status-bar crescent while Do Not Disturb is on.
pub fn draw_status_moon(canvas: &mut Canvas, (cx, cy): (f32, f32)) {
    draw_moon(canvas, (cx, cy), 15.0, WHITE);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ShellState;

    #[test]
    fn a_pull_from_the_top_right_opens_it_and_sliders_send_levels() {
        let mut state = ShellState::new(false, 50);
        state.display_view = Some(crate::display::DisplayView {
            brightness: 40,
            auto: false,
            sensor_available: true,
        });
        assert!(!state.control_pointer_begin(300.0, 40.0));
        assert!(state.control_pointer_begin(900.0, 40.0));
        for y in [200.0, 700.0, 1400.0] {
            state.control_pointer_move(900.0, y);
        }
        state.control_pointer_end(900.0, 1450.0);
        assert!(state.control.open);
        // Drag the brightness slider up by half its height.
        let (x, y) = BRIGHTNESS.center();
        assert!(state.control_pointer_begin(x, y));
        state.control_pointer_move(x, y - BRIGHTNESS.h / 2.0);
        state.control_pointer_end(x, y - BRIGHTNESS.h / 2.0);
        assert_eq!(
            state.display_requests.last(),
            Some(&DisplayRequest::Set(90))
        );
        // Volume, down.
        let (x, y) = VOLUME.center();
        assert!(state.control_pointer_begin(x, y));
        state.control_pointer_move(x, y + VOLUME.h / 4.0);
        state.control_pointer_end(x, y + VOLUME.h / 4.0);
        assert_eq!(state.control.pending_volume, Some(25));
        // Do Not Disturb goes to moon-notifyd.
        let (x, y) = tile_center(Tile::Dnd);
        assert!(state.control_pointer_begin(x, y));
        state.control_pointer_end(x, y);
        assert_eq!(state.alerts.outbox.last(), Some(&Outgoing::SetDnd(true)));
    }
}
