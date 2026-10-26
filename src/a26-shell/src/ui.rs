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

use crate::alerts::{self, CARD_W, CARD_X, Icons, RingInfo};
use crate::backdrop::{self, DimLevels, LEVEL_STEP, RowFade, level_dims};
use crate::calls;
use crate::clock::LocalTime;
use crate::compositor::{Compositor, Frame, Node, SurfaceId};
use crate::gfx::{self, Canvas, Image, Path, RectF, Rgb};
use crate::interaction::{CARD_RADIUS, SCREEN, SCREEN_H, SCREEN_W, SWITCHER_DIM};
use crate::model::{AppId, Press, SHAKE_ANIMATION, ShellState, UNLOCK_ANIMATION, View};
use crate::present::Damage;
use crate::status_bar;
use crate::telephony_client::Cellular;
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
pub const LAUNCHER_APPS: [AppId; 4] =
    [AppId::Phone, AppId::Browser, AppId::Messages, AppId::System];

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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StatusInfo {
    pub time: String,
    pub wifi_connected: bool,
    pub battery_percent: Option<u8>,
    pub battery_charging: bool,
    /// An alarm is set (the status bar shows a small alarm clock).
    pub alarm_set: bool,
    /// Do Not Disturb is on (a crescent).
    pub dnd: bool,
    /// Signal bars or a short state ("No SIM"), from moon-telephonyd.
    pub cellular: Cellular,
    /// The carrier, shown where the time would be (lock screen, Control
    /// Centre).
    pub carrier: Option<String>,
    /// A call is up with its screen tucked away: the time sits in a green
    /// pill.
    pub call_pill: bool,
}

impl StatusInfo {
    pub fn from_state(state: &ShellState, twenty_four_hour: bool) -> Self {
        Self {
            time: LocalTime::now().time_text(twenty_four_hour),
            wifi_connected: state.wifi_connected,
            battery_percent: state.battery_percent.map(|value| value.min(100)),
            battery_charging: state.battery_charging,
            alarm_set: state.alerts.store.alarm_set(),
            dnd: state.alerts.store.do_not_disturb,
            cellular: if state.calls.store.connected {
                state.calls.store.state.cellular()
            } else {
                Cellular::Hidden
            },
            carrier: state
                .calls
                .store
                .connected
                .then(|| state.calls.store.state.carrier())
                .flatten(),
            call_pill: state.calls.pill(),
        }
    }
}

/// The app icons notifications show, by app id.
struct IconSet<'a>(&'a [AppIcon; AppId::COUNT]);

impl Icons for IconSet<'_> {
    fn icon(&self, app: &str) -> Option<&Image> {
        AppId::parse(app).map(|app| &self.0[app.index()].large)
    }
}

/// A layer key for content that is cheaper to hash than to compare.
fn key_of(value: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// Banner and swiped-card layers draw the card this far down, leaving room
/// for its shadow.
const CARD_LAYER_INSET: f32 = 20.0;

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
            // Moon draws these two itself.
            AppId::Phone | AppId::Messages => None,
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

/// The Messages icon's bubble: an ellipse with a tail at its lower left.
fn draw_speech_bubble(canvas: &mut Canvas, cx: f32, cy: f32, scale: f32) {
    let (rx, ry) = (76.0 * scale, 62.0 * scale);
    let mut bubble = Path::new();
    let steps = 96;
    let points: Vec<(f32, f32)> = (0..steps)
        .map(|step| {
            let angle = std::f32::consts::TAU * step as f32 / steps as f32;
            (cx + rx * angle.cos(), cy + ry * angle.sin())
        })
        .collect();
    bubble.polygon(&points);
    canvas.fill_path(&bubble, WHITE, 1.0);
    let mut tail = Path::new();
    tail.polygon(&[
        (cx - 50.0 * scale, cy + 34.0 * scale),
        (cx - 22.0 * scale, cy + 54.0 * scale),
        (cx - 70.0 * scale, cy + 78.0 * scale),
        (cx - 56.0 * scale, cy + 62.0 * scale),
    ]);
    canvas.fill_path(&tail, WHITE, 1.0);
}

/// Artwork for an app whose installation provides none (development hosts),
/// and Moon's own Phone and Messages icons.
fn fallback_icon(app: AppId) -> Image {
    let size = ICON_SOURCE_SIZE;
    let mut canvas = Canvas::new(size, size);
    let (top, bottom, letter) = match app {
        AppId::System => (0x8e8e93, 0x48484a, "S"),
        AppId::Browser => (0x5ac8fa, 0x0a64d8, "B"),
        AppId::Phone | AppId::Messages => (0x6ce87a, 0x1fc04a, ""),
    };
    canvas.fill_rows(|y| gfx::mix(top, bottom, y as f32 / size as f32));
    let c = size as f32 / 2.0;
    match app {
        AppId::Phone => calls::draw_handset(&mut canvas, c, c, 1.6, false, WHITE),
        AppId::Messages => draw_speech_bubble(&mut canvas, c, c - 4.0, 1.0),
        AppId::System | AppId::Browser => {}
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct HomeKey {
    status: StatusInfo,
    date: (u8, u8, u8),
    wallpaper: String,
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
    /// The regions the last update changed.
    changed: Vec<Damage>,
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
            changed: Vec::new(),
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
        self.changed = compositor.upload(conn, self.id, &self.canvas)?;
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
    wallpaper_id: String,
    /// Control Centre: its toggles, and the brightness and volume sliders.
    control: Layer<u64>,
    sliders: [Layer<u64>; 2],
    wall: SurfaceId,
    /// The wallpaper darkened toward the keypad's dim.
    wall_levels: DimLevels,
    /// The home screen darkened toward the switcher's dim.
    home_levels: DimLevels,
    lock_fade: RowFade,
    home_fade: RowFade,
    /// The lock/home split of the previous frame, to start a screen's
    /// background at its level rather than fading into it.
    last_locked: Option<bool>,
    /// GPU mode: the wallpaper and home screen with an exact dim each,
    /// instead of rows of darkened copies.
    gpu_backdrops: [Option<SurfaceId>; 2],
    /// When idle time may draw the next darkened copy.
    next_idle_copy: Instant,
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
    icons: [AppIcon; AppId::COUNT],
    icon_layers: [SurfaceId; AppId::COUNT],
    labels: [SurfaceId; AppId::COUNT],
    snapshots: [Option<SurfaceId>; AppId::COUNT],
    fade_snapshot: Option<SurfaceId>,
    /// Notifications and alarms (see alerts.rs).
    banner: Layer<u64>,
    banner_height: f32,
    lock_list: Layer<u64>,
    centre: Layer<u64>,
    centre_card: Layer<u64>,
    ring: Layer<u64>,
    /// The call screen, its status line and its slide-to-answer knob
    /// (calls.rs).
    call: Layer<u64>,
    call_line: Layer<u64>,
    call_knob: Layer<u8>,
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

/// Darkened copies between full brightness and these resting dims: the
/// wallpaper toward the keypad's; the home screen toward the keypad's
/// (unlock) and on to the switcher's.
const LOCK_DIM_STOPS: [f32; 1] = [DIMMED];
const HOME_DIM_STOPS: [f32; 2] = [DIMMED, SWITCHER_DIM];
/// How often idle time draws another darkened copy.
const IDLE_COPY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(30);
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
        let wallpaper = crate::wallpaper::render("moon", w, h);
        let wall = compositor.create_layer(conn, width, height, false)?;
        compositor.upload(conn, wall, &wallpaper)?;
        let wall_levels = DimLevels::new(wall, level_dims(&LOCK_DIM_STOPS, LEVEL_STEP));
        let icons = AppId::ALL.map(AppIcon::new);
        let mut icon_layers = [wall; AppId::COUNT];
        let mut labels = [wall; AppId::COUNT];
        for app in AppId::ALL {
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
            banner: Layer::new(
                conn,
                &mut compositor,
                w,
                alerts::BANNER_LAYER_H as i32,
                true,
            )?,
            banner_height: 0.0,
            lock_list: Layer::new(conn, &mut compositor, w, alerts::LOCK_LIST_H as i32, true)?,
            centre: Layer::new(conn, &mut compositor, w, h, true)?,
            centre_card: Layer::new(
                conn,
                &mut compositor,
                w,
                alerts::BANNER_LAYER_H as i32,
                true,
            )?,
            ring: Layer::new(conn, &mut compositor, w, h, true)?,
            call: Layer::new(conn, &mut compositor, w, h, true)?,
            call_line: Layer::new(conn, &mut compositor, w, calls::LINE.h as i32, true)?,
            call_knob: Layer::new(
                conn,
                &mut compositor,
                calls::KNOB as i32,
                calls::KNOB as i32,
                true,
            )?,
            control: Layer::new(conn, &mut compositor, w, h, true)?,
            sliders: [
                Layer::new(
                    conn,
                    &mut compositor,
                    crate::control_centre::BRIGHTNESS.w as i32,
                    crate::control_centre::BRIGHTNESS.h as i32,
                    true,
                )?,
                Layer::new(
                    conn,
                    &mut compositor,
                    crate::control_centre::VOLUME.w as i32,
                    crate::control_centre::VOLUME.h as i32,
                    true,
                )?,
            ],
            compositor,
            wallpaper,
            wallpaper_id: "moon".into(),
            wall,
            wall_levels,
            home_levels: DimLevels::new(wall, Vec::new()),
            lock_fade: RowFade::new(usize::from(height)),
            home_fade: RowFade::new(usize::from(height)),
            last_locked: None,
            gpu_backdrops: [None, None],
            next_idle_copy: Instant::now(),
            backlight_fades: false,
            icons,
            icon_layers,
            labels,
            snapshots: [None; AppId::COUNT],
            fade_snapshot: None,
        };
        renderer.home_levels =
            DimLevels::new(renderer.home.id, level_dims(&HOME_DIM_STOPS, LEVEL_STEP));
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
        self.update_background(conn, state, display_time)?;
        if state.alerts.ring_sheet.visible()
            || state.alerts.centre.visible(display_time)
            || state.calls_cover()
        {
            // Their background: the darkest wallpaper copy.
            let max = self.wall_levels.max_level();
            self.wall_levels
                .ensure(conn, &mut self.compositor, max..=max)?;
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
                wallpaper: self.wallpaper_id.clone(),
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
                // Marked only: copies are brought up to date when a fade
                // next shows them, never while idle.
                self.home_levels.invalidate(&self.home.changed);
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
        self.refresh_alert_layers(conn, state, &status)?;
        let compositor = &mut self.compositor;
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

    /// Banner, lock-screen list, centre and ringing-screen artwork.
    fn refresh_alert_layers<C: Connection>(
        &mut self,
        conn: &C,
        state: &ShellState,
        status: &StatusInfo,
    ) -> Result<(), Box<dyn Error>> {
        let compositor = &mut self.compositor;
        let icons = IconSet(&self.icons);
        let now = crate::clock::unix_now();
        let minute = now / 60;
        let store = &state.alerts.store;
        if let Some(notification) = state
            .alerts
            .banner
            .as_ref()
            .and_then(|banner| store.notification(&banner.id))
        {
            self.banner_height = alerts::card_height(notification);
            let height = self.banner_height;
            self.banner.update(
                conn,
                compositor,
                key_of(&(notification, minute)),
                |canvas| {
                    canvas.clear(0);
                    let rect = RectF::new(CARD_X, CARD_LAYER_INSET, CARD_W, height);
                    alerts::draw_card(canvas, rect, notification, now, &icons, false);
                },
            )?;
        }
        if state.view == View::Locked {
            let unseen = state.alerts.unseen();
            self.lock_list
                .update(conn, compositor, key_of(&(&unseen, minute)), |canvas| {
                    alerts::draw_lock_list(canvas, &unseen, now, &icons);
                })?;
        }
        let centre = &state.alerts.centre;
        if centre.visible(Instant::now()) {
            let skip = centre.swipe.as_ref().map(|swipe| swipe.id.as_str());
            let key = key_of(&(&store.notifications, minute, centre.clear_armed, skip));
            let key = key_of(&(key, status));
            self.centre.update(conn, compositor, key, |canvas| {
                alerts::draw_centre(
                    canvas,
                    store,
                    skip,
                    centre.clear_armed,
                    now,
                    &status.time,
                    &icons,
                );
                draw_status_bar(canvas, status, None);
            })?;
            if let Some(swipe) = &centre.swipe
                && let Some(notification) = store.notification(&swipe.id)
            {
                let height = swipe.rect.h;
                self.centre_card.update(
                    conn,
                    compositor,
                    key_of(&(notification, minute)),
                    |canvas| {
                        canvas.clear(0);
                        let rect = RectF::new(CARD_X, CARD_LAYER_INSET, CARD_W, height);
                        alerts::draw_card(canvas, rect, notification, now, &icons, false);
                    },
                )?;
            }
        }
        if state.control.visible(Instant::now()) {
            use crate::control_centre::{self as cc, ControlKey, Slider};
            let next_alarm = store
                .alarms
                .iter()
                .filter_map(|a| a.snoozed_until.or(a.next_fire_at))
                .min()
                .map(|at| LocalTime::at(at).time_text(self.twenty_four_hour));
            let key = ControlKey {
                wifi: state.wifi_connected.then(|| "Wi-Fi".to_owned()),
                dnd: store.do_not_disturb,
                flashlight: state.control.flashlight.as_ref().map(|f| f.on),
                alarm: next_alarm,
                pressed: state.control.pressed,
            };
            let drawn = key.clone();
            // The carrier stands where the time would be, as on iPhone.
            let cc_status = match &status.carrier {
                Some(_) => StatusInfo {
                    time: String::new(),
                    ..status.clone()
                },
                None => status.clone(),
            };
            self.control
                .update(conn, compositor, key_of(&(key, &cc_status)), |canvas| {
                    cc::draw_control(canvas, &drawn);
                    draw_status_bar(canvas, &cc_status, None);
                })?;
            for (index, slider) in [Slider::Brightness, Slider::Volume].into_iter().enumerate() {
                let value = state
                    .control
                    .slider_value(slider, state.display_view, state.volume);
                let auto = match slider {
                    Slider::Brightness => state
                        .display_view
                        .filter(|view| view.sensor_available)
                        .map(|view| view.auto),
                    Slider::Volume => None,
                };
                self.sliders[index].update(conn, compositor, key_of(&(value, auto)), |canvas| {
                    cc::draw_slider(canvas, slider, value, auto);
                })?;
            }
        }
        if let Some(ring) = state.alerts.ringing() {
            let info = RingInfo::from_store(store, ring);
            let time = LocalTime::now().time_text(self.twenty_four_hour);
            let pressed = state.alerts.ring_pressed;
            let key = key_of(&format!("{info:?}{time}{pressed:?}"));
            self.ring.update(conn, compositor, key, |canvas| {
                alerts::draw_ring_screen(canvas, &info, &time, pressed);
                draw_status_bar(
                    canvas,
                    &StatusInfo {
                        time: time.clone(),
                        ..status.clone()
                    },
                    None,
                );
            })?;
        }
        if let Some(view) = state.call_view() {
            let calls = &state.calls;
            let key = calls::CallKey {
                view: view.clone(),
                keypad: calls.keypad,
                digits: calls.keypad_digits.clone(),
                mute: calls.mute,
                speaker: calls.speaker,
                pressed: calls.pressed,
            };
            let call_status = StatusInfo {
                call_pill: false,
                ..status.clone()
            };
            self.call
                .update(conn, compositor, key_of(&(&key, &call_status)), |canvas| {
                    calls::draw_call_screen(canvas, &key);
                    draw_status_bar(canvas, &call_status, None);
                })?;
            let line = calls::status_line(&view, now);
            self.call_line
                .update(conn, compositor, key_of(&line), |canvas| {
                    calls::draw_status_line(canvas, &line);
                })?;
            self.call_knob
                .update(conn, compositor, 0, calls::draw_knob)?;
        }
        Ok(())
    }

    /// Notification centre, ringing screen and banner nodes, always the same
    /// number of them so showing or hiding one never restructures a frame.
    fn alert_nodes(&self, state: &ShellState, t: Instant, nodes: &mut Vec<Node>) {
        let deep = self.wall_levels.surface(self.wall_levels.max_level());
        let clipped = |surface, top: f32, bottom: f32, alpha: f32| {
            let rect = RectF::new(0.0, top, SCREEN_W, (bottom - top).max(0.0));
            Node::Image {
                surface,
                src: rect,
                dst: rect,
                alpha: if rect.h > 0.0 { alpha } else { 0.0 },
                radius: 0.0,
                smooth: false,
            }
        };
        // The centre: a curtain whose content stays put while its edge
        // moves, so a frame redraws only the strip the edge crossed.
        let centre = &state.alerts.centre;
        let edge = centre.edge(t).round();
        nodes.push(clipped(deep, 0.0, edge, 1.0));
        nodes.push(clipped(self.centre.id, 0.0, edge, 1.0));
        let swipe = centre.swipe.as_ref().filter(|_| edge > 0.0);
        nodes.push(Node::Image {
            surface: self.centre_card.id,
            src: RectF::new(0.0, 0.0, SCREEN_W, alerts::BANNER_LAYER_H),
            dst: RectF::new(
                swipe.map_or(0.0, |s| s.offset(t).round()),
                swipe.map_or(0.0, |s| s.rect.y - CARD_LAYER_INSET),
                SCREEN_W,
                alerts::BANNER_LAYER_H,
            ),
            alpha: swipe.map_or(0.0, |s| {
                (1.0 - s.offset(t).abs() / SCREEN_W * 0.6).clamp(0.0, 1.0)
            }),
            radius: 0.0,
            smooth: false,
        });
        // The ringing screen and the call screen rise from the bottom and
        // leave the same way, as curtains.
        let (top, bottom) = state.alerts.ring_sheet.bounds(t, SCREEN_H);
        let ringing = state.alerts.ring_sheet.visible();
        nodes.push(clipped(deep, top, bottom, 1.0));
        nodes.push(clipped(self.ring.id, top, bottom, 1.0));
        let (top, bottom) = state.calls.sheet.bounds(t, SCREEN_H);
        nodes.push(clipped(deep, top, bottom, 1.0));
        nodes.push(clipped(self.call.id, top, bottom, 1.0));
        // Its status line and slide knob are layers of their own, shown
        // through the same curtain.
        let within = |surface, rect: RectF, alpha: f32| {
            let y0 = rect.y.max(top);
            let y1 = (rect.y + rect.h).min(bottom);
            let h = (y1 - y0).max(0.0);
            Node::Image {
                surface,
                src: RectF::new(0.0, y0 - rect.y, rect.w, h),
                dst: RectF::new(rect.x, y0, rect.w, h),
                alpha: if h > 0.0 { alpha } else { 0.0 },
                radius: 0.0,
                smooth: false,
            }
        };
        nodes.push(within(self.call_line.id, calls::LINE, 1.0));
        let sliding = matches!(
            state.call_view(),
            Some(calls::CallView::Incoming {
                locked: true,
                waiting: false,
                ..
            })
        );
        let (knob_x, knob_y) = calls::knob_origin(state.calls.knob_offset(t).round());
        nodes.push(within(
            self.call_knob.id,
            RectF::new(knob_x, knob_y, calls::KNOB, calls::KNOB),
            if sliding { 1.0 } else { 0.0 },
        ));
        // Banners: drawn here unless they float over an app in their own
        // window (main.rs).
        let banner = state
            .alerts
            .banner
            .as_ref()
            .filter(|_| !banner_in_window(state));
        nodes.push(Node::Image {
            surface: self.banner.id,
            src: RectF::new(0.0, 0.0, SCREEN_W, alerts::BANNER_LAYER_H),
            dst: RectF::new(
                0.0,
                banner.map_or(0.0, |b| (b.y(t) - CARD_LAYER_INSET).round()),
                SCREEN_W,
                alerts::BANNER_LAYER_H,
            ),
            alpha: if banner.is_some() && !ringing {
                1.0
            } else {
                0.0
            },
            radius: 0.0,
            smooth: false,
        });
        // Control Centre: the same curtain, with its sliders.
        let edge = state.control.edge(t).round();
        nodes.push(clipped(deep, 0.0, edge, 1.0));
        nodes.push(clipped(self.control.id, 0.0, edge, 1.0));
        for (index, slider) in [
            crate::control_centre::Slider::Brightness,
            crate::control_centre::Slider::Volume,
        ]
        .into_iter()
        .enumerate()
        {
            let rect = slider.rect();
            let shown = (edge - rect.y).clamp(0.0, rect.h);
            nodes.push(Node::Image {
                surface: self.sliders[index].id,
                src: RectF::new(0.0, 0.0, rect.w, shown),
                dst: RectF::new(rect.x, rect.y, rect.w, shown),
                alpha: if shown > 0.0 { 1.0 } else { 0.0 },
                radius: 0.0,
                smooth: false,
            });
        }
    }

    /// Shows a built-in wallpaper (settings.rs); the darkened copies are
    /// redrawn in idle time.
    pub fn set_wallpaper<C: Connection>(
        &mut self,
        conn: &C,
        id: &str,
    ) -> Result<(), Box<dyn Error>> {
        if self.wallpaper_id == id {
            return Ok(());
        }
        self.wallpaper = crate::wallpaper::render(id, self.wallpaper.width, self.wallpaper.height);
        self.compositor.upload(conn, self.wall, &self.wallpaper)?;
        self.wallpaper_id = id.to_owned();
        if !self.compositor.gpu() {
            self.wall_levels.invalidate_all();
            self.home_levels.invalidate_all();
        }
        Ok(())
    }

    /// The banner over an app (its own window, see banner.rs): its top,
    /// card height, and a key for its content; None when not shown.
    pub fn banner_window(&self, state: &ShellState, now: Instant) -> Option<(i32, f32, u64)> {
        if !banner_in_window(state) || state.alerts.ringing().is_some() {
            return None;
        }
        let banner = state.alerts.banner.as_ref()?;
        let notification = state.alerts.store.notification(&banner.id)?;
        let minute = crate::clock::unix_now() / 60;
        Some((
            banner.y(now).round() as i32,
            alerts::card_height(notification),
            key_of(&(notification, minute)),
        ))
    }

    /// Draws the banner window's card: solid, at its top-left corner.
    pub fn draw_banner_window(&self, canvas: &mut Canvas, state: &ShellState) {
        let Some(notification) = state
            .alerts
            .banner
            .as_ref()
            .and_then(|banner| state.alerts.store.notification(&banner.id))
        else {
            return;
        };
        let rect = RectF::new(0.0, 0.0, CARD_W, alerts::card_height(notification));
        let now = crate::clock::unix_now();
        alerts::draw_card(canvas, rect, notification, now, &IconSet(&self.icons), true);
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

    /// Work done while nothing moves: the backgrounds' darkened copies, one
    /// at a time so input is never kept waiting. Returns when to continue.
    pub fn prepare_idle<C: Connection>(
        &mut self,
        conn: &C,
        now: Instant,
    ) -> Result<Option<Instant>, Box<dyn Error>> {
        if now < self.next_idle_copy {
            return Ok(Some(self.next_idle_copy));
        }
        // Only copies that do not exist yet (after startup); changed ones
        // are redrawn when a fade next shows them.
        let levels = if self.wall_levels.missing() {
            &mut self.wall_levels
        } else if self.home_levels.missing() {
            &mut self.home_levels
        } else {
            return Ok(None);
        };
        levels.create_some(conn, &mut self.compositor, 1)?;
        self.next_idle_copy = now + IDLE_COPY_INTERVAL;
        Ok((self.wall_levels.missing() || self.home_levels.missing())
            .then_some(self.next_idle_copy))
    }

    /// Moves the visible background's rows toward its dim for this frame
    /// and makes sure the copies they show are drawn.
    fn update_background<C: Connection>(
        &mut self,
        conn: &C,
        state: &ShellState,
        t: Instant,
    ) -> Result<(), Box<dyn Error>> {
        if !state.screen_awake
            || (state
                .fade
                .is_some_and(|fade| fade.out && !self.backlight_fades)
                && self.fade_snapshot.is_some())
        {
            return Ok(());
        }
        let locked = state.view == View::Locked;
        if self.compositor.gpu() {
            let (source, dim) = if locked {
                (
                    self.wall,
                    state.lock_motion.progress(t).clamp(0.0, 1.0) * DIMMED,
                )
            } else {
                (self.home.id, home_dim(state, t))
            };
            let slot = usize::from(locked);
            let id = match self.gpu_backdrops[slot] {
                Some(id) => id,
                None => {
                    let id = self.compositor.create_dimmed(conn, source, dim)?;
                    self.gpu_backdrops[slot] = Some(id);
                    id
                }
            };
            self.compositor.set_dim(id, dim);
            self.last_locked = Some(locked);
            return Ok(());
        }
        let (levels, fade, dim) = if locked {
            let shown = state.lock_motion.progress(t).clamp(0.0, 1.0);
            (&mut self.wall_levels, &mut self.lock_fade, shown * DIMMED)
        } else {
            (
                &mut self.home_levels,
                &mut self.home_fade,
                home_dim(state, t),
            )
        };
        let target = levels.level_of(dim);
        if self.last_locked != Some(locked) {
            // Entering a screen starts its background at its level.
            self.last_locked = Some(locked);
            fade.snap(target, levels.max_level());
        } else {
            let budget = backdrop::row_budget(self.compositor.foreground_area, SCREEN_W as u16);
            // Behind the target (a fast swipe, or a release ahead of the
            // fade), neighbouring rows may sit two levels apart (about 7%),
            // which halves the rows to redraw (three would read as grain
            // again). The gaps close as the fade finishes smoothly.
            let behind = (target - fade.mean()).abs();
            let gap = match (state.pointer.is_some(), behind) {
                (true, b) if b > 2.0 => 2,
                (false, b) if b > 0.5 => 2,
                _ => 1,
            };
            fade.update(target, levels.max_level(), budget, gap);
        }
        levels.ensure(conn, &mut self.compositor, fade.span())
    }

    /// The node showing a background's rows.
    fn background_node(compositor: &mut Compositor, levels: &DimLevels, fade: &RowFade) -> Node {
        let rows: Vec<SurfaceId> = fade.levels().iter().map(|l| levels.surface(*l)).collect();
        compositor.set_rows(&rows)
    }

    /// Background rows still moving toward their target: keep drawing.
    pub fn settling(&self) -> bool {
        if self.compositor.gpu() {
            return false;
        }
        // Only the background on screen; the other one is snapped to its
        // level when its screen next appears.
        match self.last_locked {
            Some(true) => self.lock_fade.settling(),
            Some(false) => self.home_fade.settling(),
            None => false,
        }
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
        // The centre or the ringing screen over an app: the app's snapshot
        // lies beneath them.
        let app_beneath = AppId::from_view(state.view).filter(|_| {
            state.alerts_cover_app(t)
                && state.launch().is_none()
                && state.gesture.is_none()
                && state.returning.is_none()
        });
        let base = match (app_beneath, state.view) {
            (Some(app), _) => Some(self.snapshots[app.index()].unwrap_or(self.wall)),
            (None, View::Locked) => self.lock_nodes(state, t, &mut nodes),
            (None, _) => self.home_nodes(state, t, &mut nodes),
        };
        self.alert_nodes(state, t, &mut nodes);
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
        // The keypad's darker wallpaper, row by row from pre-darkened
        // copies (see backdrop.rs): no full-screen blend per frame.
        nodes.push(match self.gpu_backdrops[1] {
            Some(id) if self.compositor.gpu() => backdrop_node(id),
            _ => Self::background_node(&mut self.compositor, &self.wall_levels, &self.lock_fade),
        });
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
        // Notifications under the clock fade out first (a list fading is
        // costly; this keeps it to the swipe's first few frames).
        let list_alpha = if state.alerts.unseen().is_empty() {
            0.0
        } else {
            (1.0 - p * 5.0).clamp(0.0, 1.0)
        };
        nodes.push(Node::Image {
            surface: self.lock_list.id,
            src: RectF::new(0.0, 0.0, SCREEN_W, alerts::LOCK_LIST_H),
            dst: RectF::new(0.0, alerts::LOCK_LIST_TOP, SCREEN_W, alerts::LOCK_LIST_H),
            alpha: (list_alpha * 255.0).round() / 255.0,
            radius: 0.0,
            smooth: false,
        });
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
        // The home screen, darkened (row by row from copies) for unlock,
        // the switcher, and the held home gesture.
        nodes.push(match self.gpu_backdrops[0] {
            Some(id) if self.compositor.gpu() => backdrop_node(id),
            _ => Self::background_node(&mut self.compositor, &self.home_levels, &self.home_fade),
        });
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
    if status.time.is_empty() {
        // The lock screen's big clock stands in for the time; the carrier
        // takes its place, as on iPhone.
        if let Some(carrier) = &status.carrier {
            let carrier = text::fit(carrier, Weight::SemiBold, 44.0, 560.0);
            text::draw(
                canvas,
                &carrier,
                72.0,
                text::centered_baseline(cy, Weight::SemiBold, 44.0),
                Weight::SemiBold,
                44.0,
                WHITE,
                1.0,
            );
        }
    } else {
        if status.call_pill {
            crate::calls::draw_pill(canvas);
        }
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
    }
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
    // Then, leftward: Wi-Fi (or the cellular data technology when Wi-Fi is
    // off), the cellular bars, and the alarm and Do Not Disturb glyphs.
    let wifi_x = right - 92.0 - 46.0;
    let cellular_rat = match &status.cellular {
        Cellular::Bars { rat, .. } => *rat,
        _ => None,
    };
    let mut edge = if status.wifi_connected || status.cellular == Cellular::Hidden {
        draw_wifi_glyph(canvas, wifi_x, cy + 15.0, status.wifi_connected);
        wifi_x - 35.0
    } else if let Some(rat) = cellular_rat {
        let width = text::measure(rat, Weight::SemiBold, 40.0);
        let x = wifi_x + 35.0 - width;
        let baseline = text::centered_baseline(cy, Weight::SemiBold, 40.0);
        text::draw(canvas, rat, x, baseline, Weight::SemiBold, 40.0, WHITE, 1.0);
        x
    } else {
        wifi_x + 35.0
    };
    match &status.cellular {
        Cellular::Hidden => {}
        Cellular::Bars { level, .. } => {
            draw_signal_bars(canvas, edge - 20.0, cy, *level);
            edge -= 20.0 + SIGNAL_W;
        }
        Cellular::Text(label) => {
            let width = text::measure(label, Weight::SemiBold, 38.0);
            let baseline = text::centered_baseline(cy, Weight::SemiBold, 38.0);
            let x = edge - 20.0 - width;
            text::draw(
                canvas,
                label,
                x,
                baseline,
                Weight::SemiBold,
                38.0,
                WHITE,
                1.0,
            );
            edge = x;
        }
        Cellular::Airplane => {
            draw_airplane(canvas, edge - 20.0 - 24.0, cy);
            edge -= 20.0 + 48.0;
        }
    }
    let mut glyph_x = edge - 43.0;
    if status.alarm_set {
        alerts::draw_status_alarm(canvas, (glyph_x, cy), WHITE);
        glyph_x -= 60.0;
    }
    if status.dnd {
        crate::control_centre::draw_status_moon(canvas, (glyph_x, cy));
    }
}

/// Width of the four signal bars.
const SIGNAL_W: f32 = 4.0 * 10.0 + 3.0 * 5.0;

/// iOS-style signal: four rising bars ending at `right`, lit up to `level`.
fn draw_signal_bars(canvas: &mut Canvas, right: f32, cy: f32, level: u8) {
    let bottom = cy + 16.0;
    for bar in 0..4_u8 {
        let x = right - SIGNAL_W + f32::from(bar) * 15.0;
        let h = 11.0 + f32::from(bar) * 7.0;
        let alpha = if bar < level { 1.0 } else { 0.32 };
        canvas.fill_rrect(RectF::new(x, bottom - h, 10.0, h), 3.0, WHITE, alpha);
    }
}

fn draw_airplane(canvas: &mut Canvas, cx: f32, cy: f32) {
    let mut plane = Path::new();
    // Fuselage, wings and tail, nose pointing right.
    plane.stroke_segment((cx - 22.0, cy), (cx + 20.0, cy), 8.0);
    plane.polygon(&[
        (cx + 2.0, cy - 3.0),
        (cx - 8.0, cy - 22.0),
        (cx - 2.0, cy - 22.0),
        (cx + 12.0, cy - 3.0),
    ]);
    plane.polygon(&[
        (cx + 2.0, cy + 3.0),
        (cx - 8.0, cy + 22.0),
        (cx - 2.0, cy + 22.0),
        (cx + 12.0, cy + 3.0),
    ]);
    plane.polygon(&[
        (cx - 16.0, cy - 2.0),
        (cx - 22.0, cy - 11.0),
        (cx - 18.0, cy - 11.0),
        (cx - 10.0, cy - 2.0),
    ]);
    plane.polygon(&[
        (cx - 16.0, cy + 2.0),
        (cx - 22.0, cy + 11.0),
        (cx - 18.0, cy + 11.0),
        (cx - 10.0, cy + 2.0),
    ]);
    canvas.fill_path(&plane, WHITE, 1.0);
}

/// A banner over an app floats in its own small window (the app is not
/// Moon's to draw); everywhere else the shell composes it.
pub fn banner_in_window(state: &ShellState) -> bool {
    state.screen_awake && state.view.is_app() && !state.shell_covers_app()
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
    draw_wifi_glyph_in(canvas, cx, apex_y, connected, WHITE);
}

pub(crate) fn draw_wifi_glyph_in(
    canvas: &mut Canvas,
    cx: f32,
    apex_y: f32,
    connected: bool,
    color: Rgb,
) {
    let alpha = if connected { 1.0 } else { 0.32 };
    let quarter = std::f32::consts::FRAC_PI_4;
    let mut wedge = Path::new();
    wedge.move_to(cx, apex_y);
    wedge.arc(cx, apex_y, 11.5, -3.0 * quarter, -quarter);
    wedge.close();
    canvas.fill_path(&wedge, color, alpha);
    for radius in [23.0, 35.0] {
        canvas.ring_arc(
            cx,
            apex_y,
            radius,
            7.0,
            -quarter + 0.08,
            quarter - 0.08,
            color,
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

pub(crate) fn draw_speaker(canvas: &mut Canvas, cx: f32, cy: f32, volume: u8, color: Rgb) {
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
        for (index, app) in LAUNCHER_APPS.into_iter().enumerate() {
            let (x, y) = app_icon_rect(index).center();
            assert_eq!(launcher_app_at(x as i16, y as i16), Some(app));
        }
        let (_, by) = app_icon_rect(0).center();
        assert_eq!(launcher_app_at(540, 1200), None);
        // Between the middle two icons, and past the dock's ends.
        assert_eq!(launcher_app_at(540, by as i16), None);
        assert_eq!(launcher_app_at(1070, by as i16), None);
        assert_eq!(launcher_app_at(10, by as i16), None);
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
            alarm_set: true,
            dnd: true,
            cellular: Cellular::Bars {
                level: 3,
                rat: Some("LTE"),
            },
            carrier: None,
            call_pill: true,
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
        let wall = crate::wallpaper::render("moon", 1080, 2340);
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
                alarm_set: false,
                dnd: false,
                cellular: Cellular::Hidden,
                carrier: None,
                call_pill: false,
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

/// A full-screen background with an exact dim (GPU mode).
fn backdrop_node(surface: SurfaceId) -> Node {
    Node::Image {
        surface,
        src: SCREEN,
        dst: SCREEN,
        alpha: 1.0,
        radius: 0.0,
        smooth: false,
    }
}
