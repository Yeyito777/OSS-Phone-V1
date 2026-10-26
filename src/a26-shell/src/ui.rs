//! Moon's shell surfaces: lock screen, passcode keypad, home screen, app
//! launch/close zooms, and the shared status-bar and volume artwork.
//!
//! Geometry follows the iPhone portrait reference (390 pt wide) scaled to the
//! A26's 1080 physical pixels: one point is ~2.77 pixels. The palette is the
//! iOS dark-appearance system palette so Moon, its keyboard, and the System
//! app read as one product.

use std::error::Error;
use std::fs;
use std::time::Instant;

use x11rb::connection::Connection;

use crate::backdrop::{DimLevels, StripeFade};
use crate::clock::LocalTime;
use crate::compositor::{Compositor, Frame, Node, SurfaceId};
use crate::gfx::{self, Canvas, Image, Path, RectF, Rgb};
use crate::interaction::{CARD_RADIUS, SCREEN, SCREEN_H, SCREEN_W, SWITCHER_DIM};
use crate::model::{AppId, Press, SHAKE_ANIMATION, ShellState, UNLOCK_ANIMATION, View};
use crate::status_bar;
use crate::text::{self, Weight};

pub const BLACK: Rgb = 0x000000;
pub const WHITE: Rgb = 0xffffff;
/// Secondary label color; draw at `SECONDARY_ALPHA`.
pub const SECONDARY: Rgb = 0xebebf5;
pub const SECONDARY_ALPHA: f32 = 0.6;
pub const BLUE: Rgb = 0x0a84ff;
pub const GREEN: Rgb = 0x30d158;
pub const RED: Rgb = 0xff453a;
pub const CARD: Rgb = 0x1c1c1e;
pub const CARD_RAISED: Rgb = 0x2c2c2e;

const ICON_SOURCE_SIZE: i32 = 220;
const ICON_SOURCE_BYTES: usize = (ICON_SOURCE_SIZE * ICON_SOURCE_SIZE * 4) as usize;
const SYSTEM_ICON_PATH: &str = "/opt/a26-system/share/system-app.bgrx";
const BROWSER_ICON_PATH: &str = "/opt/vimbrowser-a26/share/browser-app.bgrx";

/// Home-screen icon size (60 pt) and its continuous-corner radius.
pub const ICON_SIZE: i32 = 168;
pub const ICON_RADIUS: f32 = 38.0;
const SPLASH_ICON_SIZE: f32 = 236.0;
const GRID_MARGIN: f32 = 75.0;
const GRID_PITCH: f32 = 254.0;
const WIDGET_TOP: f32 = 186.0;
const WIDGET_SIZE: f32 = 422.0;
const WIDGET_RADIUS: f32 = 62.0;
/// The dock: a frosted shelf above the bottom edge holding the apps.
const DOCK: RectF = RectF::new(30.0, 2046.0, 1020.0, 264.0);
const DOCK_RADIUS: f32 = 98.0;
/// Center-to-center spacing of dock icons.
const DOCK_PITCH: f32 = 254.0;
/// The order of apps in the dock.
pub const LAUNCHER_APPS: [AppId; 2] = [AppId::Browser, AppId::System];

// Lock screen and passcode geometry.
const PASSCODE_TITLE_BASELINE: f32 = 612.0;
const PASSCODE_DOTS_Y: f32 = 724.0;
const PASSCODE_DOT_RADIUS: f32 = 19.0;
const PASSCODE_DOT_PITCH: f32 = 74.0;
const KEYPAD_COLUMN_OFFSET: f32 = 300.0;
const KEYPAD_FIRST_ROW_Y: f32 = 1012.0;
const KEYPAD_ROW_PITCH: f32 = 262.0;
const KEYPAD_RADIUS: f32 = 110.0;
const PASSCODE_CORNER_BASELINE: f32 = 2148.0;
const HOME_INDICATOR: RectF = RectF::new(355.0, 2304.0, 370.0, 14.0);

/// The volume HUD sits beside the A26's right-edge volume rocker.
pub const VOLUME_HUD: RectF = RectF::new(938.0, 470.0, 112.0, 392.0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeypadAction {
    Digit(u8),
    Backspace,
    Cancel,
}

/// Everything the status bar shows; surfaces repaint only when it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusInfo {
    pub time: String,
    pub wifi_connected: bool,
    pub battery_percent: Option<u8>,
    pub battery_charging: bool,
}

impl StatusInfo {
    pub fn from_state(state: &ShellState, twenty_four_hour: bool) -> Self {
        Self {
            time: LocalTime::now().time_text(twenty_four_hour),
            wifi_connected: state.wifi_connected,
            battery_percent: state.battery_percent.map(|value| value.min(100)),
            battery_charging: state.battery_charging,
        }
    }
}

fn load_icon(variable: &str, default: &str) -> Option<Image> {
    let path = std::env::var_os(variable).unwrap_or_else(|| default.into());
    fs::read(path)
        .ok()
        .filter(|bytes| bytes.len() == ICON_SOURCE_BYTES)
        .and_then(|bytes| Image::from_bgrx(ICON_SOURCE_SIZE, ICON_SOURCE_SIZE, &bytes))
}

struct AppIcon {
    home: Image,
    large: Image,
    /// Launch-screen color, sampled from the artwork's corner so the icon
    /// appears to grow out of its own background.
    splash: Rgb,
}

impl AppIcon {
    fn new(app: AppId) -> Self {
        let artwork = match app {
            AppId::System => load_icon("A26_SYSTEM_ICON", SYSTEM_ICON_PATH),
            AppId::Browser => load_icon("A26_BROWSER_ICON", BROWSER_ICON_PATH),
        }
        .unwrap_or_else(|| fallback_icon(app));
        let splash = artwork.pixels[(6 * artwork.width + 6) as usize] & 0x00ff_ffff;
        Self {
            home: artwork.icon_mask(ICON_SIZE, ICON_RADIUS),
            large: artwork.icon_mask(
                ICON_SOURCE_SIZE,
                ICON_RADIUS * ICON_SOURCE_SIZE as f32 / ICON_SIZE as f32,
            ),
            splash,
        }
    }
}

/// Artwork for an app whose installation provides none (development hosts).
fn fallback_icon(app: AppId) -> Image {
    let size = ICON_SOURCE_SIZE;
    let mut canvas = Canvas::new(size, size);
    let (top, bottom, letter) = match app {
        AppId::System => (0x8e8e93, 0x48484a, "S"),
        AppId::Browser => (0x5ac8fa, 0x0a64d8, "B"),
    };
    canvas.fill_rows(|y| gfx::mix(top, bottom, y as f32 / size as f32));
    let baseline = text::centered_baseline(size as f32 / 2.0, Weight::SemiBold, 120.0);
    text::draw_centered(
        &mut canvas,
        letter,
        size as f32 / 2.0,
        baseline,
        Weight::SemiBold,
        120.0,
        WHITE,
        1.0,
    );
    Image {
        width: size,
        height: size,
        pixels: canvas.pixels.iter().map(|p| 0xff00_0000 | p).collect(),
    }
}

/// Moon's default wallpaper: a deep night-sky gradient lit by soft indigo,
/// blue, and violet glows, dithered so the 8-bit panel shows no banding.
fn wallpaper(width: i32, height: i32) -> Canvas {
    struct Glow {
        x: f32,
        y: f32,
        radius: f32,
        color: (f32, f32, f32),
        strength: f32,
    }
    let glows = [
        Glow {
            x: 0.86,
            y: 0.10,
            radius: 0.62,
            color: (94.0, 92.0, 230.0),
            strength: 0.62,
        },
        Glow {
            x: 0.06,
            y: 0.58,
            radius: 0.64,
            color: (10.0, 110.0, 230.0),
            strength: 0.42,
        },
        Glow {
            x: 0.78,
            y: 0.98,
            radius: 0.56,
            color: (150.0, 70.0, 210.0),
            strength: 0.40,
        },
    ];
    let w = width as f32;
    let mut canvas = Canvas::new(width, height);
    let mut columns: Vec<Vec<f32>> = Vec::with_capacity(glows.len());
    for glow in &glows {
        let sigma = glow.radius * w;
        columns.push(
            (0..width)
                .map(|x| {
                    let d = x as f32 - glow.x * w;
                    (-(d * d) / (2.0 * sigma * sigma)).exp()
                })
                .collect(),
        );
    }
    for y in 0..height {
        let t = y as f32 / height as f32;
        let base = (
            gfx::lerp(10.0, 4.0, t),
            gfx::lerp(12.0, 5.0, t),
            gfx::lerp(30.0, 16.0, t),
        );
        let rows: Vec<f32> = glows
            .iter()
            .map(|glow| {
                let sigma = glow.radius * w;
                let d = y as f32 - glow.y * height as f32;
                (-(d * d) / (2.0 * sigma * sigma)).exp() * glow.strength
            })
            .collect();
        for x in 0..width {
            let (mut r, mut g, mut b) = base;
            for (index, glow) in glows.iter().enumerate() {
                let k = rows[index] * columns[index][x as usize];
                r += glow.color.0 * k;
                g += glow.color.1 * k;
                b += glow.color.2 * k;
            }
            // Ordered-noise dither of +/- half a code value.
            let hash = (x as u32).wrapping_mul(0x9e37_79b1) ^ (y as u32).wrapping_mul(0x85eb_ca77);
            let noise = ((hash >> 13) & 0xff) as f32 / 255.0 - 0.5;
            let channel = |v: f32| (v + noise).clamp(0.0, 255.0) as u32;
            canvas.pixels[(y * width + x) as usize] =
                channel(r) << 16 | channel(g) << 8 | channel(b);
        }
    }
    canvas
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HomeKey {
    status: StatusInfo,
    date: (u8, u8, u8),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClockKey {
    time: String,
    date: String,
    charging: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PasscodeKey {
    title: String,
    pressed: Option<KeypadAction>,
    corner: &'static str,
    enabled: bool,
}

/// A layer drawn on the CPU and uploaded when its content key changes.
struct Layer<K> {
    id: SurfaceId,
    canvas: Canvas,
    key: Option<K>,
}

impl<K: PartialEq> Layer<K> {
    fn new<C: Connection>(
        conn: &C,
        compositor: &mut Compositor,
        width: i32,
        height: i32,
        transparent: bool,
    ) -> Result<Self, Box<dyn Error>> {
        Ok(Self {
            id: compositor.create_layer(conn, width as u16, height as u16, transparent)?,
            canvas: Canvas::new(width, height),
            key: None,
        })
    }

    /// Redraws and uploads the layer when `key` differs from the last one.
    fn update<C: Connection>(
        &mut self,
        conn: &C,
        compositor: &mut Compositor,
        key: K,
        draw: impl FnOnce(&mut Canvas),
    ) -> Result<bool, Box<dyn Error>> {
        if self.key.as_ref() == Some(&key) {
            return Ok(false);
        }
        draw(&mut self.canvas);
        compositor.upload(conn, self.id, &self.canvas)?;
        self.key = Some(key);
        Ok(true)
    }
}

/// What an app's card shows while it moves.
#[derive(Debug, Clone, Copy)]
enum CardFace {
    Snapshot(SurfaceId),
    Splash(AppId),
}

/// Builds the shell's frames. Static content lives in CPU-drawn layers that
/// are re-uploaded only when they change; each frame is a list of compositor
/// nodes sampled at the time it will reach the panel.
pub struct Renderer {
    pub twenty_four_hour: bool,
    pin_length: usize,
    pub compositor: Compositor,
    wallpaper: Canvas,
    wall: SurfaceId,
    /// The wallpaper darkened toward the keypad's dim.
    wall_levels: DimLevels,
    /// The home screen darkened toward the switcher's dim.
    home_levels: DimLevels,
    lock_fade: StripeFade,
    home_fade: StripeFade,
    /// The lock/home split of the previous frame, to start a screen's
    /// background at its level rather than fading into it.
    last_locked: Option<bool>,
    /// Power and wake fades ramp the panel backlight (no frames are drawn);
    /// without a backlight they fade a black overlay instead.
    pub backlight_fades: bool,
    /// The home screen under the app switcher's dim.
    home: Layer<HomeKey>,
    clock: Layer<ClockKey>,
    lock_status: Layer<StatusInfo>,
    passcode: Layer<PasscodeKey>,
    dots: Layer<usize>,
    volume: Layer<u8>,
    spinner: Layer<u32>,
    icons: [AppIcon; 2],
    icon_layers: [SurfaceId; 2],
    labels: [SurfaceId; 2],
    snapshots: [Option<SurfaceId>; 2],
    fade_snapshot: Option<SurfaceId>,
}

const DOTS_TOP: f32 = PASSCODE_DOTS_Y - 32.0;
/// How much the passcode keypad darkens the wallpaper.
const DIMMED: f32 = 0.42;
/// Unlock progress, 1 at unlock falling to 0 as the keypad fades away.
fn unlock_progress(state: &ShellState, t: Instant) -> f32 {
    state.unlock_at.map_or(0.0, |at| {
        1.0 - gfx::ease_out(
            t.saturating_duration_since(at).as_secs_f32() / UNLOCK_ANIMATION.as_secs_f32(),
        )
    })
}

/// How dark the home screen behind everything should be right now.
fn home_dim(state: &ShellState, t: Instant) -> f32 {
    if let Some(switcher) = &state.switcher {
        return SWITCHER_DIM * (1.0 - switcher.exit_progress(t));
    }
    if let Some(gesture) = &state.gesture {
        return gesture.backdrop_dim(t);
    }
    DIMMED * unlock_progress(state, t)
}

/// Wallpaper levels for the keypad's dim.
const LOCK_DIMS: [f32; 2] = [DIMMED / 2.0, DIMMED];
/// Home-screen levels: the keypad's dim (unlock), then the switcher's.
const HOME_DIMS: [f32; 3] = [DIMMED / 2.0, DIMMED, SWITCHER_DIM];
const SPINNER_SIZE: i32 = 96;
const LABEL_W: i32 = 640;
const LABEL_H: i32 = 96;

impl Renderer {
    pub fn new<C: Connection>(
        conn: &C,
        mut compositor: Compositor,
        width: u16,
        height: u16,
        twenty_four_hour: bool,
        pin_length: usize,
    ) -> Result<Self, Box<dyn Error>> {
        let (w, h) = (i32::from(width), i32::from(height));
        let wallpaper = wallpaper(w, h);
        let wall = compositor.create_layer(conn, width, height, false)?;
        compositor.upload(conn, wall, &wallpaper)?;
        let mut wall_levels = DimLevels::new(wall, &LOCK_DIMS);
        wall_levels.ensure(conn, &mut compositor)?;
        let icons = [AppIcon::new(AppId::System), AppIcon::new(AppId::Browser)];
        let mut icon_layers = [wall; 2];
        let mut labels = [wall; 2];
        for app in [AppId::System, AppId::Browser] {
            let icon = &icons[app.index()];
            let id = compositor.create_layer(
                conn,
                ICON_SOURCE_SIZE as u16,
                ICON_SOURCE_SIZE as u16,
                true,
            )?;
            let mut canvas = Canvas::new(ICON_SOURCE_SIZE, ICON_SOURCE_SIZE);
            canvas.draw_image(&icon.large, 0, 0, 1.0);
            compositor.upload(conn, id, &canvas)?;
            icon_layers[app.index()] = id;

            let label = compositor.create_layer(conn, LABEL_W as u16, LABEL_H as u16, true)?;
            let mut canvas = Canvas::new(LABEL_W, LABEL_H);
            canvas.draw_image_scaled(&icon.large, RectF::new(0.0, 12.0, 72.0, 72.0), 1.0);
            let baseline = text::centered_baseline(48.0, Weight::SemiBold, 44.0);
            text::draw(
                &mut canvas,
                app.display_name(),
                96.0,
                baseline,
                Weight::SemiBold,
                44.0,
                WHITE,
                1.0,
            );
            compositor.upload(conn, label, &canvas)?;
            labels[app.index()] = label;
        }
        let mut renderer = Self {
            twenty_four_hour,
            pin_length,
            home: Layer::new(conn, &mut compositor, w, h, false)?,
            clock: Layer::new(conn, &mut compositor, w, h, true)?,
            lock_status: Layer::new(
                conn,
                &mut compositor,
                w,
                i32::from(status_bar::HEIGHT),
                true,
            )?,
            passcode: Layer::new(conn, &mut compositor, w, h, true)?,
            dots: Layer::new(conn, &mut compositor, w, 64, true)?,
            volume: Layer::new(
                conn,
                &mut compositor,
                VOLUME_HUD.w as i32,
                VOLUME_HUD.h as i32,
                true,
            )?,
            spinner: Layer::new(conn, &mut compositor, SPINNER_SIZE, SPINNER_SIZE, true)?,
            compositor,
            wallpaper,
            wall,
            wall_levels,
            home_levels: DimLevels::new(wall, &HOME_DIMS),
            lock_fade: StripeFade::default(),
            home_fade: StripeFade::default(),
            last_locked: None,
            backlight_fades: false,
            icons,
            icon_layers,
            labels,
            snapshots: [None, None],
            fade_snapshot: None,
        };
        renderer.home_levels = DimLevels::new(renderer.home.id, &HOME_DIMS);
        Ok(renderer)
    }

    fn status(&self, state: &ShellState) -> StatusInfo {
        StatusInfo::from_state(state, self.twenty_four_hour)
    }

    /// Snapshots what is on screen as `app`'s card. Call before restacking.
    pub fn capture_app<C: Connection>(
        &mut self,
        conn: &C,
        app: AppId,
    ) -> Result<(), Box<dyn Error>> {
        let id = match self.snapshots[app.index()] {
            Some(id) => id,
            None => {
                let id = self.compositor.create_snapshot(conn)?;
                self.snapshots[app.index()] = Some(id);
                id
            }
        };
        self.compositor.capture_root(conn, id)
    }

    /// Snapshots the screen for the power fade.
    pub fn capture_fade<C: Connection>(&mut self, conn: &C) -> Result<(), Box<dyn Error>> {
        let id = match self.fade_snapshot {
            Some(id) => id,
            None => {
                let id = self.compositor.create_snapshot(conn)?;
                self.fade_snapshot = Some(id);
                id
            }
        };
        self.compositor.capture_root(conn, id)
    }

    /// Forgets an app's card once its process is gone.
    pub fn forget_app<C: Connection>(&mut self, conn: &C, app: AppId) {
        if let Some(id) = self.snapshots[app.index()].take() {
            self.compositor.free(conn, id);
        }
    }

    pub fn has_snapshot(&self, app: AppId) -> bool {
        self.snapshots[app.index()].is_some()
    }

    /// Composes and submits the frame for `display_time`. Returns false when
    /// nothing changed on screen.
    pub fn render<C: Connection>(
        &mut self,
        conn: &C,
        state: &ShellState,
        display_time: Instant,
    ) -> Result<bool, Box<dyn Error>> {
        self.refresh_layers(conn, state, display_time)?;
        if let Some(switcher) = &state.switcher {
            // Cards at rest in the switcher draw a pre-scaled copy.
            let (w, h) = crate::interaction::switcher_card_size();
            for app in &switcher.apps {
                if let Some(id) = self.snapshots[app.index()] {
                    self.compositor
                        .prescale(conn, id, w.round() as u16, h.round() as u16)?;
                }
            }
        }
        if self.home_levels.stale() && home_dim(state, display_time) > 0.0 {
            // Normally prepared while idle (`prepare_idle`).
            self.home_levels.ensure(conn, &mut self.compositor)?;
        }
        let frame = self.frame(state, display_time);
        self.compositor.compose(conn, frame)
    }

    fn refresh_layers<C: Connection>(
        &mut self,
        conn: &C,
        state: &ShellState,
        now: Instant,
    ) -> Result<(), Box<dyn Error>> {
        let status = self.status(state);
        let time = LocalTime::now();
        let compositor = &mut self.compositor;
        if !state.screen_awake {
            return Ok(());
        }
        // The home screen stays current while locked too, so unlocking never
        // waits for it (or for its darkened copies) to be drawn.
        {
            let key = HomeKey {
                status: status.clone(),
                date: (time.weekday, time.day, time.month),
            };
            let wallpaper = &self.wallpaper;
            let icons = &self.icons;
            let redrawn = self.home.update(conn, compositor, key, |canvas| {
                canvas.copy_from(wallpaper);
                draw_calendar_widget(canvas, widget_rect(0), time);
                draw_battery_widget(canvas, widget_rect(1), state);
                canvas.fill_rrect(DOCK, DOCK_RADIUS, WHITE, 0.14);
                for (index, app) in LAUNCHER_APPS.iter().enumerate() {
                    let rect = app_icon_rect(index);
                    canvas.shadow_rrect(rect.inset(6.0), ICON_RADIUS, 20.0, BLACK, 0.18);
                    canvas.draw_image(&icons[app.index()].home, rect.x as i32, rect.y as i32, 1.0);
                }
                draw_status_bar(canvas, &status, None);
            })?;
            if redrawn {
                self.home_levels.invalidate();
            }
        }
        if state.view == View::Locked || state.unlock_at.is_some() {
            let cx = SCREEN_W / 2.0;
            let charging = state.battery_charging.then(|| {
                state
                    .battery_percent
                    .map_or_else(|| "Charging".to_owned(), |p| format!("{p}% Charged"))
            });
            let key = ClockKey {
                time: time.time_text(self.twenty_four_hour),
                date: time.date_text(),
                charging,
            };
            let clock_key = key.clone();
            self.clock.update(conn, compositor, key, |canvas| {
                canvas.clear(0);
                draw_padlock(canvas, cx, 214.0);
                text::draw_centered(
                    canvas,
                    &clock_key.date,
                    cx,
                    358.0,
                    Weight::SemiBold,
                    58.0,
                    WHITE,
                    0.86,
                );
                text::draw_centered(
                    canvas,
                    &clock_key.time,
                    cx,
                    640.0,
                    Weight::DisplaySemiBold,
                    270.0,
                    WHITE,
                    0.94,
                );
                if let Some(line) = &clock_key.charging {
                    let width = text::measure(line, Weight::Medium, 44.0);
                    draw_bolt(canvas, cx - width / 2.0 - 30.0, 742.0, 1.0, GREEN);
                    text::draw_centered(
                        canvas,
                        line,
                        cx + 14.0,
                        758.0,
                        Weight::Medium,
                        44.0,
                        WHITE,
                        0.86,
                    );
                }
                text::draw_centered(
                    canvas,
                    "Swipe up to unlock",
                    cx,
                    2226.0,
                    Weight::Regular,
                    42.0,
                    WHITE,
                    0.72,
                );
                canvas.fill_rrect(HOME_INDICATOR, 7.0, WHITE, 0.92);
            })?;
            // The big clock replaces the status-bar time on the lock screen.
            let lock_status = StatusInfo {
                time: String::new(),
                ..status.clone()
            };
            let drawn = lock_status.clone();
            self.lock_status
                .update(conn, compositor, lock_status, |canvas| {
                    canvas.clear(0);
                    draw_status_bar(canvas, &drawn, None);
                })?;

            let lockout = state
                .lockout_until
                .filter(|deadline| *deadline > now)
                .map(|deadline| deadline.saturating_duration_since(now).as_secs() + 1);
            let key = PasscodeKey {
                title: lockout.map_or_else(
                    || "Enter Passcode".to_owned(),
                    |s| format!("Try again in {s} s"),
                ),
                pressed: match state.pressed {
                    Some(Press::Keypad(action)) => Some(action),
                    _ => None,
                },
                corner: if state.pin_input.is_empty() {
                    "Cancel"
                } else {
                    "Delete"
                },
                enabled: lockout.is_none(),
            };
            let passcode_key = key.clone();
            self.passcode.update(conn, compositor, key, |canvas| {
                canvas.clear(0);
                text::draw_centered(
                    canvas,
                    &passcode_key.title,
                    cx,
                    PASSCODE_TITLE_BASELINE,
                    Weight::Regular,
                    56.0,
                    WHITE,
                    1.0,
                );
                for digit in 0_u8..=9 {
                    let (kx, ky) = keypad_center(digit);
                    let pressed = passcode_key.pressed == Some(KeypadAction::Digit(digit));
                    draw_keypad_key(canvas, kx, ky, digit, pressed, passcode_key.enabled);
                }
                let corner_pressed = matches!(
                    passcode_key.pressed,
                    Some(KeypadAction::Cancel | KeypadAction::Backspace)
                );
                text::draw_centered(
                    canvas,
                    passcode_key.corner,
                    cx + KEYPAD_COLUMN_OFFSET,
                    PASSCODE_CORNER_BASELINE,
                    Weight::Regular,
                    46.0,
                    WHITE,
                    if corner_pressed { 0.45 } else { 1.0 },
                );
                canvas.fill_rrect(HOME_INDICATOR, 7.0, WHITE, 0.92);
            })?;
            let count = self.pin_length.max(1);
            let filled = state.pin_input.len();
            self.dots.update(conn, compositor, filled, |canvas| {
                canvas.clear(0);
                let first = cx - PASSCODE_DOT_PITCH * (count as f32 - 1.0) / 2.0;
                let cy = PASSCODE_DOTS_Y - DOTS_TOP;
                for index in 0..count {
                    let x = first + index as f32 * PASSCODE_DOT_PITCH;
                    if index < filled {
                        canvas.fill_circle(x, cy, PASSCODE_DOT_RADIUS, WHITE, 1.0);
                    } else {
                        canvas.stroke_circle(x, cy, PASSCODE_DOT_RADIUS, 3.5, WHITE, 1.0);
                    }
                }
            })?;
        }
        let volume = state.volume;
        self.volume.update(conn, compositor, volume, |canvas| {
            canvas.clear(0);
            draw_volume_hud(
                canvas,
                RectF::new(0.0, 0.0, VOLUME_HUD.w, VOLUME_HUD.h),
                volume,
            );
        })?;
        if let Some(launch) = state.launch() {
            let step = (now.saturating_duration_since(launch.started).as_millis() / 83) as u32;
            self.spinner.update(conn, compositor, step % 8, |canvas| {
                canvas.clear(0);
                let c = SPINNER_SIZE as f32 / 2.0;
                draw_spinner(canvas, c, c, step, 1.0);
            })?;
        }
        Ok(())
    }

    fn card_face(&self, app: AppId) -> CardFace {
        match self.snapshots[app.index()] {
            Some(id) => CardFace::Snapshot(id),
            None => CardFace::Splash(app),
        }
    }

    fn push_card(
        &self,
        nodes: &mut Vec<Node>,
        face: CardFace,
        rect: RectF,
        radius: f32,
        alpha: f32,
    ) {
        match face {
            CardFace::Snapshot(surface) => nodes.push(Node::Image {
                surface,
                src: SCREEN,
                dst: rect,
                alpha,
                radius,
                // Bilinear scaling of a large card does not fit a 120 Hz
                // frame on the unaccelerated server; at this size nearest
                // sampling is hard to tell apart while the card moves.
                smooth: rect.w < SCREEN_W * 0.72,
            }),
            CardFace::Splash(app) => {
                nodes.push(Node::Fill {
                    dst: rect,
                    color: self.icons[app.index()].splash,
                    alpha,
                    radius,
                });
                let grow =
                    ((rect.w - ICON_SIZE as f32) / (SCREEN_W - ICON_SIZE as f32)).clamp(0.0, 1.0);
                let size = (ICON_SIZE as f32 + (SPLASH_ICON_SIZE - ICON_SIZE as f32) * grow)
                    .min(rect.w)
                    .min(rect.h);
                let (cx, cy) = rect.center();
                nodes.push(Node::Image {
                    surface: self.icon_layers[app.index()],
                    src: RectF::new(0.0, 0.0, ICON_SOURCE_SIZE as f32, ICON_SOURCE_SIZE as f32),
                    dst: RectF::new(cx - size / 2.0, cy - size / 2.0, size, size),
                    alpha,
                    radius: 0.0,
                    smooth: true,
                });
            }
        }
    }

    fn full(surface: SurfaceId, width: f32, height: f32, dx: f32, dy: f32, alpha: f32) -> Node {
        Node::Image {
            surface,
            src: RectF::new(0.0, 0.0, width, height),
            dst: RectF::new(dx.round(), dy.round(), width, height),
            alpha,
            radius: 0.0,
            smooth: false,
        }
    }

    /// The keypad's bands and dots at cascade progress `p` (see
    /// `keypad_stage`), the dots shaken sideways by `shake`.
    fn keypad_nodes(&self, p: f32, shake: f32, nodes: &mut Vec<Node>) {
        for (stage, band) in KEYPAD_BANDS.windows(2).enumerate() {
            let (alpha, rise) = keypad_stage(stage, p);
            let src = RectF::new(0.0, band[0], SCREEN_W, band[1] - band[0]);
            nodes.push(Node::Image {
                surface: self.passcode.id,
                src,
                dst: RectF::new(0.0, (band[0] + rise).round(), src.w, src.h),
                alpha,
                radius: 0.0,
                smooth: false,
            });
        }
        let (alpha, rise) = keypad_stage(0, p);
        nodes.push(Self::full(
            self.dots.id,
            SCREEN_W,
            64.0,
            shake,
            DOTS_TOP + rise,
            alpha,
        ));
    }

    /// Work done while nothing moves: the home screen's darkened copies.
    pub fn prepare_idle<C: Connection>(&mut self, conn: &C) -> Result<(), Box<dyn Error>> {
        if self.home_levels.stale() {
            self.home_levels.ensure(conn, &mut self.compositor)?;
        }
        Ok(())
    }

    /// Background bands still stepping toward their target: keep drawing.
    pub fn settling(&self) -> bool {
        self.lock_fade.settling() || self.home_fade.settling()
    }

    fn frame(&mut self, state: &ShellState, t: Instant) -> Frame {
        let mut nodes = Vec::new();
        if !state.screen_awake {
            nodes.push(Node::Fill {
                dst: SCREEN,
                color: BLACK,
                alpha: 1.0,
                radius: 0.0,
            });
            return Frame {
                base: Some(self.wall),
                nodes,
            };
        }
        if let Some(fade) = state.fade.filter(|fade| fade.out && !self.backlight_fades)
            && let Some(snapshot) = self.fade_snapshot
        {
            nodes.push(Node::Fill {
                dst: SCREEN,
                color: BLACK,
                alpha: fade.black(t),
                radius: 0.0,
            });
            return Frame {
                base: Some(snapshot),
                nodes,
            };
        }
        let locked = state.view == View::Locked;
        if self.last_locked != Some(locked) {
            self.last_locked = Some(locked);
            if locked {
                let shown = state.lock_motion.progress(t).clamp(0.0, 1.0);
                let max = self.wall_levels.max_level();
                self.lock_fade
                    .snap(self.wall_levels.level_of(shown * DIMMED), max);
            } else {
                let max = self.home_levels.max_level();
                self.home_fade
                    .snap(self.home_levels.level_of(home_dim(state, t)), max);
            }
        }
        let base = match state.view {
            View::Locked => self.lock_nodes(state, t, &mut nodes),
            _ => self.home_nodes(state, t, &mut nodes),
        };
        let volume_visible = state
            .volume_overlay_until
            .is_some_and(|deadline| deadline > t);
        nodes.push(Self::full(
            self.volume.id,
            VOLUME_HUD.w,
            VOLUME_HUD.h,
            VOLUME_HUD.x,
            VOLUME_HUD.y,
            if volume_visible { 1.0 } else { 0.0 },
        ));
        let black = state
            .fade
            .filter(|fade| !fade.out && !self.backlight_fades)
            .map_or(0.0, |fade| fade.black(t));
        nodes.push(Node::Fill {
            dst: SCREEN,
            color: BLACK,
            alpha: black,
            radius: 0.0,
        });
        Frame { base, nodes }
    }

    /// Adds the lock screen's nodes and returns its base surface.
    fn lock_nodes(
        &mut self,
        state: &ShellState,
        t: Instant,
        nodes: &mut Vec<Node>,
    ) -> Option<SurfaceId> {
        let p = state.lock_motion.progress(t);
        let shown = p.clamp(0.0, 1.0);
        // The keypad's darker wallpaper dithers through pre-darkened copies
        // (see backdrop.rs): no full-screen blend per frame.
        let target = self.wall_levels.level_of(shown * DIMMED);
        let steps = self.lock_fade.update(target, self.wall_levels.max_level());
        nodes.push(self.wall_levels.node(steps));
        // The clock rises with the finger and fades out first; then the
        // keypad cascades in a row at a time. Only a row or two change per
        // frame, which keeps the drag within a 120 Hz refresh.
        let clock_alpha = (1.0 - p * 2.5).clamp(0.0, 1.0);
        nodes.push(Self::full(
            self.clock.id,
            SCREEN_W,
            SCREEN_H,
            0.0,
            -p * 520.0,
            clock_alpha,
        ));
        let shake = state
            .pin_error_at
            .map(|at| {
                let s = (t.saturating_duration_since(at).as_secs_f32()
                    / SHAKE_ANIMATION.as_secs_f32())
                .clamp(0.0, 1.0);
                (s * std::f32::consts::TAU * 3.5).sin() * 46.0 * (1.0 - s)
            })
            .unwrap_or(0.0);
        self.keypad_nodes(p, shake, nodes);
        nodes.push(Self::full(
            self.lock_status.id,
            SCREEN_W,
            f32::from(status_bar::HEIGHT),
            0.0,
            0.0,
            1.0,
        ));
        None
    }

    /// Adds the home screen's nodes and returns its base surface.
    fn home_nodes(
        &mut self,
        state: &ShellState,
        t: Instant,
        nodes: &mut Vec<Node>,
    ) -> Option<SurfaceId> {
        // The home screen, darkened (dithered between copies) for unlock,
        // the switcher, and the held home gesture.
        let target = self.home_levels.level_of(home_dim(state, t));
        let steps = self.home_fade.update(target, self.home_levels.max_level());
        nodes.push(self.home_levels.node(steps));
        // Unlock: the keypad cascades away over the home screen, bottom row
        // first (the lock swipe's cascade in reverse).
        let unlock = unlock_progress(state, t);
        self.keypad_nodes(unlock, 0.0, nodes);

        let pressed = match state.pressed {
            Some(Press::App(app)) => Some(app),
            _ => None,
        };
        for app in LAUNCHER_APPS {
            nodes.push(Node::Fill {
                dst: launcher_icon_rect(app),
                color: BLACK,
                alpha: if pressed == Some(app) { 0.38 } else { 0.0 },
                radius: ICON_RADIUS,
            });
        }

        if let Some(switcher) = &state.switcher {
            let enter = switcher.entrance(t);
            let fade = 1.0 - switcher.exit_progress(t);
            let base = None;
            nodes.push(Node::Fill {
                dst: SCREEN,
                color: BLACK,
                alpha: 0.0,
                radius: 0.0,
            });
            for (index, app) in switcher.apps.iter().enumerate() {
                let entering = state.switcher_entry.is_some_and(|entry| entry.app == *app);
                let rect = switcher.card_rect(index, t);
                let alpha =
                    switcher.card_alpha(index, t) * fade * if entering { 1.0 } else { enter };
                let label_alpha = alpha * enter;
                nodes.push(Node::Image {
                    surface: self.labels[app.index()],
                    src: RectF::new(0.0, 0.0, LABEL_W as f32, LABEL_H as f32),
                    dst: RectF::new(
                        rect.x.round(),
                        (rect.y - LABEL_H as f32 - 18.0).round(),
                        LABEL_W as f32,
                        LABEL_H as f32,
                    ),
                    alpha: label_alpha,
                    radius: 0.0,
                    smooth: false,
                });
                let (rect, radius) = match state.switcher_entry.filter(|_| entering) {
                    Some(entry) => (entry.rect(t), entry.radius(t)),
                    None => (rect, CARD_RADIUS),
                };
                self.push_card(nodes, self.card_face(*app), rect, radius, alpha);
            }
            return base;
        }

        let base = None;
        if let Some(gesture) = &state.gesture {
            nodes.push(Node::Fill {
                dst: SCREEN,
                color: BLACK,
                alpha: 0.0,
                radius: 0.0,
            });
            self.push_card(
                nodes,
                self.card_face(gesture.app),
                gesture.card_rect(t),
                gesture.card_radius(t),
                1.0,
            );
        } else if let Some(card) = &state.returning {
            nodes.push(Node::Fill {
                dst: SCREEN,
                color: BLACK,
                alpha: 0.0,
                radius: 0.0,
            });
            self.push_card(
                nodes,
                self.card_face(card.app),
                card.rect(t),
                card.radius(t),
                1.0,
            );
        } else if let Some(launch) = state.launch() {
            nodes.push(Node::Fill {
                dst: SCREEN,
                color: BLACK,
                alpha: 0.0,
                radius: 0.0,
            });
            let face = self.card_face(launch.app);
            let rect = launch.card.rect(t);
            self.push_card(nodes, face, rect, launch.card.radius(t), 1.0);
            let waited = t.saturating_duration_since(launch.started).as_secs_f32() - 0.7;
            if matches!(face, CardFace::Splash(_)) && waited > 0.0 {
                let (cx, cy) = rect.center();
                let half = SPINNER_SIZE as f32 / 2.0;
                nodes.push(Self::full(
                    self.spinner.id,
                    SPINNER_SIZE as f32,
                    SPINNER_SIZE as f32,
                    cx - half,
                    cy + 300.0 - half,
                    (waited / 0.25).min(1.0),
                ));
            }
        } else if let Some(card) = &state.closing {
            nodes.push(Node::Fill {
                dst: SCREEN,
                color: BLACK,
                alpha: 0.0,
                radius: 0.0,
            });
            let p = card.progress(t);
            // Cross-fade into the real dock icon at the end of the flight.
            let alpha = 1.0 - ((p - 0.82) / 0.18).clamp(0.0, 1.0);
            self.push_card(
                nodes,
                self.card_face(card.app),
                card.rect(t),
                card.radius(t),
                alpha,
            );
        }
        base
    }

    pub fn destroy<C: Connection>(&mut self, conn: &C) {
        self.compositor.destroy(conn);
    }
}

fn widget_rect(column: usize) -> RectF {
    RectF::new(
        GRID_MARGIN + column as f32 * 2.0 * GRID_PITCH,
        WIDGET_TOP,
        WIDGET_SIZE,
        WIDGET_SIZE,
    )
}

fn app_icon_rect(index: usize) -> RectF {
    let count = LAUNCHER_APPS.len() as f32;
    let (dock_cx, dock_cy) = DOCK.center();
    let cx = dock_cx + (index as f32 - (count - 1.0) / 2.0) * DOCK_PITCH;
    let size = ICON_SIZE as f32;
    RectF::new(cx - size / 2.0, dock_cy - size / 2.0, size, size)
}

pub fn launcher_icon_rect(app: AppId) -> RectF {
    LAUNCHER_APPS
        .iter()
        .position(|a| *a == app)
        .map(app_icon_rect)
        .unwrap_or(RectF::new(
            456.0,
            1086.0,
            ICON_SIZE as f32,
            ICON_SIZE as f32,
        ))
}

/// The home-screen app under a touch, including its label.
pub fn launcher_app_at(x: i16, y: i16) -> Option<AppId> {
    LAUNCHER_APPS.iter().enumerate().find_map(|(index, app)| {
        let target = app_icon_rect(index).inset(-40.0);
        target.contains(f32::from(x), f32::from(y)).then_some(*app)
    })
}

fn draw_widget_card(canvas: &mut Canvas, rect: RectF) {
    canvas.shadow_rrect(
        RectF::new(rect.x, rect.y + 10.0, rect.w, rect.h),
        WIDGET_RADIUS,
        40.0,
        BLACK,
        0.30,
    );
    canvas.fill_rrect(rect, WIDGET_RADIUS, CARD, 0.80);
}

fn draw_calendar_widget(canvas: &mut Canvas, rect: RectF, time: LocalTime) {
    draw_widget_card(canvas, rect);
    let x = rect.x + 48.0;
    text::draw_tracked(
        canvas,
        &time.weekday_name().to_uppercase(),
        x,
        rect.y + 94.0,
        Weight::SemiBold,
        36.0,
        1.5,
        RED,
        1.0,
    );
    text::draw(
        canvas,
        &time.day.to_string(),
        x - 8.0,
        rect.y + 292.0,
        Weight::DisplayLight,
        196.0,
        WHITE,
        1.0,
    );
    text::draw(
        canvas,
        time.month_name(),
        x,
        rect.y + 368.0,
        Weight::Medium,
        40.0,
        SECONDARY,
        SECONDARY_ALPHA,
    );
}

fn draw_battery_widget(canvas: &mut Canvas, rect: RectF, state: &ShellState) {
    draw_widget_card(canvas, rect);
    let cx = rect.x + rect.w / 2.0;
    let cy = rect.y + 176.0;
    let radius = 104.0;
    canvas.ring_arc(
        cx,
        cy,
        radius,
        24.0,
        0.0,
        std::f32::consts::TAU,
        WHITE,
        0.14,
    );
    if let Some(percent) = state.battery_percent {
        let color = battery_color(percent, state.battery_charging, GREEN);
        let fraction = f32::from(percent.min(100)) / 100.0;
        if fraction > 0.0 {
            canvas.ring_arc(
                cx,
                cy,
                radius,
                24.0,
                0.0,
                std::f32::consts::TAU * fraction,
                color,
                1.0,
            );
        }
    }
    let value = state
        .battery_percent
        .map_or_else(|| "--".to_owned(), |percent| format!("{percent}%"));
    // Keep "100%" clear of the ring: fit inside its inner edge with margin.
    let inner = 2.0 * (radius - 12.0) - 36.0;
    let width = text::measure(&value, Weight::DisplaySemiBold, 74.0);
    let size = if width > inner {
        74.0 * inner / width
    } else {
        74.0
    };
    text::draw_centered(
        canvas,
        &value,
        cx,
        text::centered_baseline(cy, Weight::DisplaySemiBold, size),
        Weight::DisplaySemiBold,
        size,
        WHITE,
        1.0,
    );
    let caption = if state.battery_charging {
        "Charging"
    } else {
        "Battery"
    };
    text::draw_centered(
        canvas,
        caption,
        cx,
        rect.y + 372.0,
        Weight::Medium,
        40.0,
        SECONDARY,
        SECONDARY_ALPHA,
    );
}

fn battery_color(percent: u8, charging: bool, normal: Rgb) -> Rgb {
    if charging {
        GREEN
    } else if percent <= 20 {
        RED
    } else {
        normal
    }
}

/// Draws the status bar into the top `status_bar::HEIGHT` rows: time in the
/// left ear, Wi-Fi and battery in the right ear, clear of the camera notch.
pub fn draw_status_bar(canvas: &mut Canvas, status: &StatusInfo, background: Option<Rgb>) {
    let height = f32::from(status_bar::HEIGHT);
    if let Some(color) = background {
        canvas.fill_rect(0, 0, canvas.width, status_bar::HEIGHT.into(), color, 1.0);
    }
    let cy = height / 2.0 + 2.0;
    text::draw_centered(
        canvas,
        &status.time,
        168.0,
        text::centered_baseline(cy, Weight::SemiBold, 47.0),
        Weight::SemiBold,
        47.0,
        WHITE,
        1.0,
    );
    let mut right = canvas.width as f32 - 66.0;
    if status.battery_charging {
        draw_bolt(canvas, right - 12.0, cy, 0.72, WHITE);
        right -= 34.0;
    }
    draw_battery_glyph(
        canvas,
        right,
        cy,
        status.battery_percent,
        status.battery_charging,
    );
    draw_wifi_glyph(
        canvas,
        right - 92.0 - 46.0,
        cy + 15.0,
        status.wifi_connected,
    );
}

/// iOS-style battery: a gray body, a level fill, and the percentage inside
/// drawn dark over the fill and light over the empty part.
fn draw_battery_glyph(
    canvas: &mut Canvas,
    right: f32,
    cy: f32,
    percent: Option<u8>,
    charging: bool,
) {
    let body = RectF::new(right - 6.0 - 72.0, cy - 17.0, 72.0, 34.0);
    canvas.fill_rrect(
        RectF::new(right - 5.0, cy - 6.5, 5.0, 13.0),
        2.5,
        WHITE,
        0.4,
    );
    canvas.fill_rrect(body, 10.0, WHITE, 0.36);
    let level = percent.map_or(0.0, |p| f32::from(p) / 100.0);
    let fill_x = (body.x + body.w * level).round() as i32;
    let color = percent.map_or(WHITE, |p| battery_color(p, charging, WHITE));
    canvas.set_clip(0, 0, fill_x, canvas.height);
    canvas.fill_rrect(body, 10.0, color, 1.0);
    canvas.reset_clip();
    let label = percent.map_or_else(|| "--".to_owned(), |p| p.to_string());
    let size = if label.len() >= 3 { 25.0 } else { 27.0 };
    let width = text::measure(&label, Weight::Bold, size);
    let x = body.x + (body.w - width) / 2.0;
    let baseline = text::centered_baseline(cy, Weight::Bold, size);
    canvas.set_clip(0, 0, fill_x, canvas.height);
    text::draw(canvas, &label, x, baseline, Weight::Bold, size, BLACK, 0.92);
    canvas.set_clip(fill_x, 0, canvas.width, canvas.height);
    text::draw(canvas, &label, x, baseline, Weight::Bold, size, WHITE, 1.0);
    canvas.reset_clip();
}

/// Three-band Wi-Fi fan with its apex at (`cx`, `apex_y`); dimmed when
/// disconnected.
fn draw_wifi_glyph(canvas: &mut Canvas, cx: f32, apex_y: f32, connected: bool) {
    let alpha = if connected { 1.0 } else { 0.32 };
    let quarter = std::f32::consts::FRAC_PI_4;
    let mut wedge = Path::new();
    wedge.move_to(cx, apex_y);
    wedge.arc(cx, apex_y, 11.5, -3.0 * quarter, -quarter);
    wedge.close();
    canvas.fill_path(&wedge, WHITE, alpha);
    for radius in [23.0, 35.0] {
        canvas.ring_arc(
            cx,
            apex_y,
            radius,
            7.0,
            -quarter + 0.08,
            quarter - 0.08,
            WHITE,
            alpha,
        );
    }
}

fn draw_bolt(canvas: &mut Canvas, cx: f32, cy: f32, scale: f32, color: Rgb) {
    let s = scale;
    let mut bolt = Path::new();
    bolt.polygon(&[
        (cx + 4.0 * s, cy - 22.0 * s),
        (cx - 13.0 * s, cy + 3.0 * s),
        (cx - 1.0 * s, cy + 3.0 * s),
        (cx - 4.0 * s, cy + 22.0 * s),
        (cx + 13.0 * s, cy - 3.0 * s),
        (cx + 1.0 * s, cy - 3.0 * s),
    ]);
    canvas.fill_path(&bolt, color, 1.0);
}

fn draw_padlock(canvas: &mut Canvas, cx: f32, cy: f32) {
    let body = RectF::new(cx - 22.0, cy - 6.0, 44.0, 34.0);
    let mut shackle = Path::new();
    shackle.arc(
        cx,
        cy - 10.0,
        15.5,
        std::f32::consts::PI,
        std::f32::consts::TAU,
    );
    let points: Vec<(f32, f32)> = std::iter::once((cx - 15.5, cy - 2.0))
        .chain((0..=24).map(|step| {
            let angle = std::f32::consts::PI + std::f32::consts::PI * step as f32 / 24.0;
            (cx + 15.5 * angle.cos(), cy - 10.0 + 15.5 * angle.sin())
        }))
        .chain(std::iter::once((cx + 15.5, cy - 2.0)))
        .collect();
    let mut stroke = Path::new();
    stroke.stroke_polyline(&points, 6.0);
    canvas.fill_path(&stroke, WHITE, 0.92);
    canvas.fill_rrect(body, 8.0, WHITE, 0.92);
}

/// Horizontal bands of the passcode layer that cascade in one after
/// another: the title, the three digit rows, and the zero row with the
/// corner button.
const KEYPAD_BANDS: [f32; 6] = [
    0.0,
    KEYPAD_FIRST_ROW_Y - KEYPAD_ROW_PITCH / 2.0,
    KEYPAD_FIRST_ROW_Y + KEYPAD_ROW_PITCH / 2.0,
    KEYPAD_FIRST_ROW_Y + KEYPAD_ROW_PITCH * 1.5,
    KEYPAD_FIRST_ROW_Y + KEYPAD_ROW_PITCH * 2.5,
    SCREEN_H,
];

/// Opacity and downward offset of keypad band `stage` at lock progress
/// `p`: each band fades and rises in over a fifth of the swipe, starting
/// as the clock is nearly gone.
fn keypad_stage(stage: usize, p: f32) -> (f32, f32) {
    const START: f32 = 0.3;
    const STAGGER: f32 = 0.125;
    const SPAN: f32 = 0.2;
    let local = ((p - START - stage as f32 * STAGGER) / SPAN).clamp(0.0, 1.0);
    // 8-bit alpha, as composited: a settling tail stops redrawing once the
    // change is invisible.
    let eased = (gfx::ease_out(local) * 255.0).round() / 255.0;
    (eased, (1.0 - eased) * 60.0)
}

fn keypad_center(digit: u8) -> (f32, f32) {
    let (column, row) = match digit {
        0 => (1, 3),
        digit => ((digit - 1) % 3, (digit - 1) / 3),
    };
    (
        540.0 + (f32::from(column) - 1.0) * KEYPAD_COLUMN_OFFSET,
        KEYPAD_FIRST_ROW_Y + f32::from(row) * KEYPAD_ROW_PITCH,
    )
}

const KEYPAD_LETTERS: [&str; 10] = [
    "", "", "ABC", "DEF", "GHI", "JKL", "MNO", "PQRS", "TUV", "WXYZ",
];

fn draw_keypad_key(canvas: &mut Canvas, cx: f32, cy: f32, digit: u8, pressed: bool, enabled: bool) {
    let fill = if pressed { 0.48 } else { 0.17 };
    let content = if enabled { 1.0 } else { 0.35 };
    canvas.fill_circle(cx, cy, KEYPAD_RADIUS, WHITE, fill * content);
    let letters = KEYPAD_LETTERS[usize::from(digit)];
    let digit_baseline = if letters.is_empty() && digit == 0 {
        text::centered_baseline(cy, Weight::DisplayLight, 104.0)
    } else {
        cy + 22.0
    };
    text::draw_centered(
        canvas,
        &digit.to_string(),
        cx,
        digit_baseline,
        Weight::DisplayLight,
        104.0,
        WHITE,
        content,
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
            content,
        );
    }
}

/// Passcode keypad hit testing. The lower-right corner deletes a digit, or
/// cancels back to the clock when nothing has been typed.
pub fn keypad_action_at(x: i16, y: i16, has_digits: bool) -> Option<KeypadAction> {
    let (x, y) = (f32::from(x), f32::from(y));
    let corner_x = 540.0 + KEYPAD_COLUMN_OFFSET;
    if (x - corner_x).abs() <= 150.0 && (y - (PASSCODE_CORNER_BASELINE - 16.0)).abs() <= 80.0 {
        return Some(if has_digits {
            KeypadAction::Backspace
        } else {
            KeypadAction::Cancel
        });
    }
    (0_u8..=9).find_map(|digit| {
        let (cx, cy) = keypad_center(digit);
        ((x - cx).abs() <= KEYPAD_COLUMN_OFFSET / 2.0 && (y - cy).abs() <= KEYPAD_ROW_PITCH / 2.0)
            .then_some(KeypadAction::Digit(digit))
    })
}

/// iOS-style activity indicator: eight spokes, stepping clockwise.
fn draw_spinner(canvas: &mut Canvas, cx: f32, cy: f32, step: u32, alpha: f32) {
    for spoke in 0..8_u32 {
        let angle = spoke as f32 * std::f32::consts::TAU / 8.0;
        let age = (step + 8 - spoke) % 8;
        let shade = 1.0 - age as f32 * 0.11;
        let (sin, cos) = angle.sin_cos();
        let mut path = Path::new();
        path.stroke_segment(
            (cx + sin * 18.0, cy - cos * 18.0),
            (cx + sin * 36.0, cy - cos * 36.0),
            9.0,
        );
        canvas.fill_path(&path, WHITE, shade * alpha * 0.9);
    }
}

/// The volume HUD: a capsule that fills from the bottom, with a speaker
/// glyph whose waves follow the level.
pub fn draw_volume_hud(canvas: &mut Canvas, rect: RectF, volume: u8) {
    let radius = rect.w / 2.0;
    canvas.shadow_rrect(rect, radius, 18.0, BLACK, 0.35);
    canvas.fill_rrect(rect, radius, CARD_RAISED, 1.0);
    let level = f32::from(volume.min(100)) / 100.0;
    let fill_top = rect.y + rect.h * (1.0 - level);
    canvas.set_clip(
        rect.x.floor() as i32,
        fill_top.round() as i32,
        (rect.x + rect.w).ceil() as i32,
        (rect.y + rect.h).ceil() as i32,
    );
    canvas.fill_rrect(rect, radius, WHITE, 1.0);
    canvas.reset_clip();
    let icon_cy = rect.y + rect.h - rect.w / 2.0;
    let on_fill = fill_top < icon_cy - 24.0;
    let color = if on_fill { 0x48484a } else { WHITE };
    draw_speaker(canvas, rect.x + rect.w / 2.0 - 8.0, icon_cy, volume, color);
}

fn draw_speaker(canvas: &mut Canvas, cx: f32, cy: f32, volume: u8, color: Rgb) {
    let mut body = Path::new();
    body.polygon(&[
        (cx - 18.0, cy - 8.0),
        (cx - 8.0, cy - 8.0),
        (cx + 6.0, cy - 20.0),
        (cx + 6.0, cy + 20.0),
        (cx - 8.0, cy + 8.0),
        (cx - 18.0, cy + 8.0),
    ]);
    canvas.fill_path(&body, color, 1.0);
    if volume == 0 {
        let mut slash = Path::new();
        slash.stroke_segment((cx + 14.0, cy - 9.0), (cx + 30.0, cy + 9.0), 4.5);
        slash.stroke_segment((cx + 30.0, cy - 9.0), (cx + 14.0, cy + 9.0), 4.5);
        canvas.fill_path(&slash, color, 1.0);
        return;
    }
    let waves = if volume > 66 {
        3
    } else if volume > 33 {
        2
    } else {
        1
    };
    for wave in 0..waves {
        let radius = 14.0 + wave as f32 * 9.0;
        canvas.ring_arc(cx + 4.0, cy, radius, 4.0, 0.75, 2.39, color, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_tiles_have_separate_hit_regions() {
        let browser = app_icon_rect(0);
        let system = app_icon_rect(1);
        let (bx, by) = browser.center();
        let (sx, sy) = system.center();
        assert_eq!(launcher_app_at(bx as i16, by as i16), Some(AppId::Browser));
        assert_eq!(launcher_app_at(sx as i16, sy as i16), Some(AppId::System));
        assert_eq!(launcher_app_at(540, 1200), None);
        assert_eq!(launcher_app_at(540, by as i16), None);
        assert_eq!(launcher_app_at(1000, by as i16), None);
    }

    #[test]
    fn keypad_cells_cover_every_digit_and_the_corner_control() {
        for digit in 0_u8..=9 {
            let (x, y) = keypad_center(digit);
            assert_eq!(
                keypad_action_at(x as i16, y as i16, false),
                Some(KeypadAction::Digit(digit))
            );
        }
        let corner = (840_i16, (PASSCODE_CORNER_BASELINE - 16.0) as i16);
        assert_eq!(
            keypad_action_at(corner.0, corner.1, false),
            Some(KeypadAction::Cancel)
        );
        assert_eq!(
            keypad_action_at(corner.0, corner.1, true),
            Some(KeypadAction::Backspace)
        );
        let (_, zero_y) = keypad_center(0);
        assert_eq!(keypad_action_at(240, zero_y as i16, true), None);
        assert_eq!(keypad_action_at(540, 300, true), None);
    }

    #[test]
    fn status_bar_fits_in_the_reserved_strip() {
        let mut canvas = Canvas::new(1080, 300);
        canvas.clear(0x123456);
        let status = StatusInfo {
            time: "12:59".into(),
            wifi_connected: true,
            battery_percent: Some(100),
            battery_charging: true,
        };
        draw_status_bar(&mut canvas, &status, Some(BLACK));
        let below = &canvas.pixels[(status_bar::HEIGHT as usize) * 1080..];
        assert!(below.iter().all(|pixel| *pixel == 0x123456));
        assert!(
            canvas.pixels[..1080 * 124]
                .iter()
                .any(|p| p & 0xffffff == WHITE)
        );
    }

    #[test]
    fn volume_hud_fills_from_the_bottom() {
        let mut canvas = Canvas::new(200, 500);
        let rect = RectF::new(40.0, 40.0, 112.0, 392.0);
        draw_volume_hud(&mut canvas, rect, 50);
        let x = 96;
        assert_eq!(
            canvas.pixels[(120 * 200 + x) as usize] & 0xffffff,
            CARD_RAISED
        );
        assert_eq!(canvas.pixels[(300 * 200 + x) as usize] & 0xffffff, WHITE);
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use std::time::Instant;

    #[test]
    #[ignore]
    fn primitive_costs() {
        let t = Instant::now();
        let wall = wallpaper(1080, 2340);
        eprintln!("wallpaper {:?}", t.elapsed());
        let mut canvas = Canvas::new(1080, 2340);
        for _ in 0..3 {
            let t = Instant::now();
            canvas.copy_from(&wall);
            eprintln!("copy {:?}", t.elapsed());
            let t = Instant::now();
            canvas.fill_rrect(RectF::new(20.0, 30.0, 1000.0, 2200.0), 50.0, 0x223344, 1.0);
            eprintln!("big rrect {:?}", t.elapsed());
            let icon = fallback_icon(AppId::System).icon_mask(220, 50.0);
            let t = Instant::now();
            canvas.draw_image_scaled(&icon, RectF::new(400.0, 900.0, 236.0, 236.0), 1.0);
            eprintln!("scaled icon {:?}", t.elapsed());
            let status = StatusInfo {
                time: "12:59".into(),
                wifi_connected: true,
                battery_percent: Some(80),
                battery_charging: false,
            };
            let t = Instant::now();
            draw_status_bar(&mut canvas, &status, None);
            eprintln!("status {:?}", t.elapsed());
            let t = Instant::now();
            draw_spinner(&mut canvas, 540.0, 1400.0, 3, 1.0);
            eprintln!("spinner {:?}", t.elapsed());
        }
    }
}
