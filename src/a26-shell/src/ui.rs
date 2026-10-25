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
use x11rb::protocol::xproto::Window;

use crate::clock::LocalTime;
use crate::gfx::{self, Canvas, Image, Path, RectF, Rgb};
use crate::model::{
    AppId, CLOSE_ANIMATION, LockFace, OPEN_ANIMATION, Press, SHAKE_ANIMATION, ShellState, View,
};
use crate::present::Presenter;
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
pub const YELLOW: Rgb = 0xffd60a;
pub const CARD: Rgb = 0x1c1c1e;
pub const CARD_RAISED: Rgb = 0x2c2c2e;
pub const FILL_GRAY: Rgb = 0x3a3a3c;

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

/// The A26 panel has rounded corners of roughly this radius.
const DISPLAY_CORNER_RADIUS: f32 = 56.0;

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
    Submit,
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

fn darken(source: &Canvas, amount: f32) -> Canvas {
    let mut canvas = Canvas::new(source.width, source.height);
    let keep = ((1.0 - amount) * 256.0) as u32;
    for (out, pixel) in canvas.pixels.iter_mut().zip(&source.pixels) {
        let rb = ((pixel & 0x00ff_00ff) * keep >> 8) & 0x00ff_00ff;
        let g = ((pixel & 0x0000_ff00) * keep >> 8) & 0x0000_ff00;
        *out = rb | g;
    }
    canvas
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HomeKey {
    status: StatusInfo,
    date: (u8, u8, u8),
}

pub struct Renderer {
    pub window: Window,
    pub width: u16,
    pub height: u16,
    /// Screen row at which the shell window currently begins.
    pub window_top: i32,
    pub twenty_four_hour: bool,
    pin_length: usize,
    profile: bool,
    canvas: Canvas,
    presenter: Presenter,
    wallpaper: Canvas,
    dimmed_wallpaper: Canvas,
    home_layer: Canvas,
    home_key: Option<HomeKey>,
    icons: [AppIcon; 2],
}

impl Renderer {
    pub fn new<C: Connection>(
        conn: &C,
        window: Window,
        depth: u8,
        width: u16,
        height: u16,
        twenty_four_hour: bool,
        pin_length: usize,
    ) -> Result<Self, Box<dyn Error>> {
        let presenter = Presenter::create(conn, window, depth, width, height)?;
        let (w, h) = (i32::from(width), i32::from(height));
        let wallpaper = wallpaper(w, h);
        let dimmed_wallpaper = darken(&wallpaper, 0.42);
        Ok(Self {
            window,
            width,
            height,
            window_top: 0,
            twenty_four_hour,
            pin_length,
            profile: std::env::var_os("MOON_PROFILE").is_some(),
            canvas: Canvas::new(w, h),
            presenter,
            wallpaper,
            dimmed_wallpaper,
            home_layer: Canvas::new(w, h),
            home_key: None,
            icons: [AppIcon::new(AppId::System), AppIcon::new(AppId::Browser)],
        })
    }

    fn icon(&self, app: AppId) -> &AppIcon {
        &self.icons[app.index()]
    }

    pub fn render<C: Connection>(
        &mut self,
        conn: &C,
        state: &ShellState,
    ) -> Result<(), Box<dyn Error>> {
        let now = Instant::now();
        if !state.screen_awake {
            // The runtime also turns off the panel backlight, but retain a
            // black X framebuffer as a safe fallback.
            self.canvas.clear(BLACK);
        } else {
            match state.view {
                View::Locked => self.draw_lock(state, now),
                View::Launcher => {
                    self.draw_home(state, None);
                    if let Some(closing) = state.closing {
                        self.draw_closing(closing.app, closing.started, now);
                    }
                }
                View::System | View::Browser => {
                    if let Some(launch) = state.launch() {
                        self.draw_home(state, Some(launch.app));
                        self.draw_opening(state, launch.app, launch.started, now);
                    }
                }
            }
            if !state.view.is_app()
                && state
                    .volume_overlay_until
                    .is_some_and(|deadline| deadline > now)
            {
                draw_volume_hud(&mut self.canvas, VOLUME_HUD, state.volume);
            }
        }
        let drawn = now.elapsed();
        let damage = self
            .presenter
            .present(conn, &self.canvas, self.window_top)?;
        if self.profile {
            eprintln!(
                "  draw {:.2} ms, present {:.2} ms, damage {:?}",
                drawn.as_secs_f64() * 1000.0,
                (now.elapsed() - drawn).as_secs_f64() * 1000.0,
                damage
            );
        }
        if state.shell_exposed {
            self.presenter.expose(conn, self.window_top)?;
        }
        conn.flush()?;
        Ok(())
    }

    pub fn destroy<C: Connection>(&self, conn: &C) {
        self.presenter.destroy(conn);
    }

    fn status(&self, state: &ShellState) -> StatusInfo {
        StatusInfo::from_state(state, self.twenty_four_hour)
    }

    fn draw_lock(&mut self, state: &ShellState, now: Instant) {
        let status = self.status(state);
        let width = self.canvas.width as f32;
        let cx = width / 2.0;
        match state.lock_face {
            LockFace::Clock => {
                self.canvas.copy_from(&self.wallpaper);
                let time = LocalTime::now();
                draw_padlock(&mut self.canvas, cx, 214.0);
                text::draw_centered(
                    &mut self.canvas,
                    &time.date_text(),
                    cx,
                    358.0,
                    Weight::SemiBold,
                    58.0,
                    WHITE,
                    0.86,
                );
                text::draw_centered(
                    &mut self.canvas,
                    &time.time_text(self.twenty_four_hour),
                    cx,
                    640.0,
                    Weight::DisplaySemiBold,
                    270.0,
                    WHITE,
                    0.94,
                );
                if state.battery_charging {
                    let line = state
                        .battery_percent
                        .map_or_else(|| "Charging".to_owned(), |p| format!("{p}% Charged"));
                    draw_bolt(
                        &mut self.canvas,
                        cx - text::measure(&line, Weight::Medium, 44.0) / 2.0 - 30.0,
                        742.0,
                        1.0,
                        GREEN,
                    );
                    text::draw_centered(
                        &mut self.canvas,
                        &line,
                        cx + 14.0,
                        758.0,
                        Weight::Medium,
                        44.0,
                        WHITE,
                        0.86,
                    );
                }
                text::draw_centered(
                    &mut self.canvas,
                    "Swipe up to unlock",
                    cx,
                    2226.0,
                    Weight::Regular,
                    42.0,
                    WHITE,
                    0.72,
                );
                self.canvas.fill_rrect(HOME_INDICATOR, 7.0, WHITE, 0.92);
            }
            LockFace::Passcode => {
                self.canvas.copy_from(&self.dimmed_wallpaper);
                let lockout = state
                    .lockout_until
                    .map(|deadline| deadline.saturating_duration_since(now).as_secs() + 1)
                    .filter(|_| state.lockout_until.is_some_and(|deadline| deadline > now));
                let title = match lockout {
                    Some(seconds) => format!("Try again in {seconds} s"),
                    None => "Enter Passcode".to_owned(),
                };
                text::draw_centered(
                    &mut self.canvas,
                    &title,
                    cx,
                    PASSCODE_TITLE_BASELINE,
                    Weight::Regular,
                    56.0,
                    WHITE,
                    1.0,
                );
                let shake = state
                    .pin_error_at
                    .map(|at| {
                        let t = (now.saturating_duration_since(at).as_secs_f32()
                            / SHAKE_ANIMATION.as_secs_f32())
                        .clamp(0.0, 1.0);
                        (t * std::f32::consts::TAU * 3.5).sin() * 46.0 * (1.0 - t)
                    })
                    .unwrap_or(0.0);
                let count = self.pin_length.max(1);
                let first = cx - PASSCODE_DOT_PITCH * (count as f32 - 1.0) / 2.0 + shake;
                for index in 0..count {
                    let x = first + index as f32 * PASSCODE_DOT_PITCH;
                    if index < state.pin_input.len() {
                        self.canvas.fill_circle(
                            x,
                            PASSCODE_DOTS_Y,
                            PASSCODE_DOT_RADIUS,
                            WHITE,
                            1.0,
                        );
                    } else {
                        self.canvas.stroke_circle(
                            x,
                            PASSCODE_DOTS_Y,
                            PASSCODE_DOT_RADIUS,
                            3.5,
                            WHITE,
                            1.0,
                        );
                    }
                }
                let enabled = lockout.is_none();
                for digit in 0_u8..=9 {
                    let (kx, ky) = keypad_center(digit);
                    let pressed = state.pressed == Some(Press::Keypad(KeypadAction::Digit(digit)));
                    draw_keypad_key(&mut self.canvas, kx, ky, digit, pressed, enabled);
                }
                let corner = if state.pin_input.is_empty() {
                    "Cancel"
                } else {
                    "Delete"
                };
                let corner_pressed = matches!(
                    state.pressed,
                    Some(Press::Keypad(
                        KeypadAction::Cancel | KeypadAction::Backspace
                    ))
                );
                text::draw_centered(
                    &mut self.canvas,
                    corner,
                    cx + KEYPAD_COLUMN_OFFSET,
                    PASSCODE_CORNER_BASELINE,
                    Weight::Regular,
                    46.0,
                    WHITE,
                    if corner_pressed { 0.45 } else { 1.0 },
                );
                self.canvas.fill_rrect(HOME_INDICATOR, 7.0, WHITE, 0.92);
            }
        }
        draw_status_bar(&mut self.canvas, &status, None);
    }

    /// Draws the home screen; `hidden` leaves an app's icon slot empty while
    /// that app zooms out of it.
    fn draw_home(&mut self, state: &ShellState, hidden: Option<AppId>) {
        let time = LocalTime::now();
        let key = HomeKey {
            status: self.status(state),
            date: (time.weekday, time.day, time.month),
        };
        if self.home_key.as_ref() != Some(&key) {
            self.home_layer.copy_from(&self.wallpaper);
            draw_calendar_widget(&mut self.home_layer, widget_rect(0), time);
            draw_battery_widget(&mut self.home_layer, widget_rect(1), state);
            self.home_layer.fill_rrect(DOCK, DOCK_RADIUS, WHITE, 0.14);
            for (index, app) in LAUNCHER_APPS.iter().enumerate() {
                let rect = app_icon_rect(index);
                self.home_layer
                    .shadow_rrect(rect.inset(6.0), ICON_RADIUS, 20.0, BLACK, 0.18);
                self.home_layer.draw_image(
                    &self.icons[app.index()].home,
                    rect.x as i32,
                    rect.y as i32,
                    1.0,
                );
            }
            draw_status_bar(&mut self.home_layer, &key.status, None);
            self.home_key = Some(key);
        }
        self.canvas.copy_from(&self.home_layer);
        if let Some(app) = hidden
            && let Some(index) = LAUNCHER_APPS.iter().position(|a| *a == app)
        {
            let rect = app_icon_rect(index).inset(-30.0);
            let (y0, y1) = (rect.y as i32, (rect.y + rect.h) as i32);
            for y in y0.max(0)..y1.min(self.canvas.height) {
                let start = (y * self.canvas.width + rect.x as i32) as usize;
                let end = start + rect.w as usize;
                self.canvas.pixels[start..end].copy_from_slice(&self.wallpaper.pixels[start..end]);
            }
        }
        if let Some(Press::App(app)) = state.pressed
            && let Some(index) = LAUNCHER_APPS.iter().position(|a| *a == app)
        {
            self.canvas
                .fill_rrect(app_icon_rect(index), ICON_RADIUS, BLACK, 0.38);
        }
    }

    fn draw_opening(&mut self, state: &ShellState, app: AppId, started: Instant, now: Instant) {
        let elapsed = now.saturating_duration_since(started);
        let t = (elapsed.as_secs_f32() / OPEN_ANIMATION.as_secs_f32()).clamp(0.0, 1.0);
        let e = gfx::ease_out(t);
        let from = launcher_icon_rect(app);
        let screen = RectF::new(
            0.0,
            0.0,
            self.canvas.width as f32,
            self.canvas.height as f32,
        );
        let rect = from.lerp(screen, e);
        let radius = if t >= 1.0 {
            0.0
        } else {
            gfx::lerp(ICON_RADIUS, DISPLAY_CORNER_RADIUS, e)
        };
        let icon = self.icon(app);
        let splash = icon.splash;
        self.canvas.fill_rrect(rect, radius, splash, 1.0);
        let size = gfx::lerp(ICON_SIZE as f32, SPLASH_ICON_SIZE, e);
        let (cx, cy) = rect.center();
        self.canvas.draw_image_scaled(
            &self.icons[app.index()].large,
            RectF::new(cx - size / 2.0, cy - size / 2.0, size, size),
            1.0,
        );
        // A slow first paint gets an activity indicator under the icon.
        let waited = elapsed.as_secs_f32() - OPEN_ANIMATION.as_secs_f32() - 0.35;
        if waited > 0.0 {
            draw_spinner(
                &mut self.canvas,
                cx,
                cy + 300.0,
                (elapsed.as_millis() / 83) as u32,
                (waited / 0.25).min(1.0),
            );
        }
        let status = self.status(state);
        draw_status_bar(&mut self.canvas, &status, None);
    }

    fn draw_closing(&mut self, app: AppId, started: Instant, now: Instant) {
        let t = (now.saturating_duration_since(started).as_secs_f32()
            / CLOSE_ANIMATION.as_secs_f32())
        .clamp(0.0, 1.0);
        let e = gfx::ease_out(t);
        let to = launcher_icon_rect(app);
        let screen = RectF::new(
            0.0,
            0.0,
            self.canvas.width as f32,
            self.canvas.height as f32,
        );
        let rect = screen.lerp(to, e);
        let radius = gfx::lerp(DISPLAY_CORNER_RADIUS, ICON_RADIUS, e);
        // Crossfade into the real icon over the last part of the motion.
        let alpha = 1.0 - ((t - 0.7) / 0.3).clamp(0.0, 1.0);
        let splash = self.icon(app).splash;
        self.canvas.fill_rrect(rect, radius, splash, alpha);
        let size = gfx::lerp(SPLASH_ICON_SIZE, ICON_SIZE as f32, e);
        let (cx, cy) = rect.center();
        self.canvas.draw_image_scaled(
            &self.icons[app.index()].large,
            RectF::new(cx - size / 2.0, cy - size / 2.0, size, size),
            alpha,
        );
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

fn launcher_icon_rect(app: AppId) -> RectF {
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
    let size = 74.0;
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
                .any(|pixel| *pixel == WHITE)
        );
    }

    #[test]
    fn volume_hud_fills_from_the_bottom() {
        let mut canvas = Canvas::new(200, 500);
        let rect = RectF::new(40.0, 40.0, 112.0, 392.0);
        draw_volume_hud(&mut canvas, rect, 50);
        let x = 96;
        assert_eq!(canvas.pixels[(120 * 200 + x) as usize], CARD_RAISED);
        assert_eq!(canvas.pixels[(300 * 200 + x) as usize], WHITE);
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
