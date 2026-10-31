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
use crate::badges::{self, Badges};
use crate::calls;
use crate::clock::LocalTime;
use crate::compositor::{Compositor, Frame, Node, SurfaceId};
use crate::gfx::{self, Canvas, Image, Path, RectF, Rgb};
use crate::home::{self, DOCK, DOCK_RADIUS, HomeLayout, IconView, Item, Slot, Widget, WidgetView};
use crate::interaction::{CARD_RADIUS, SCREEN, SCREEN_H, SCREEN_W, SWITCHER_DIM};
use crate::model::{AppId, Press, SHAKE_ANIMATION, ShellState, UNLOCK_ANIMATION, View};
use crate::power;
use crate::present::Damage;
use crate::status_bar;
use crate::telephony_client::Cellular;
use crate::text::{self, Weight};
use crate::voice;

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
const EXOCORTEX_ICON_PATH: &str = "/opt/a26-system/share/exocortex-app.bgrx";
const BROWSER_ICON_PATH: &str = "/opt/vimbrowser-a26/share/browser-app.bgrx";

/// Home-screen icon size (60 pt) and its continuous-corner radius.
pub const ICON_SIZE: i32 = 168;
pub const ICON_RADIUS: f32 = 38.0;
const SPLASH_ICON_SIZE: f32 = 236.0;
/// The widgets' corner radius (their places are home.rs's).
const WIDGET_RADIUS: f32 = 62.0;
/// Widget sprites: a widget and its shadow, turned (home::WIDGET_FRAMES
/// ways) side by side, each frame the widget plus this margin.
const WIDGET_PAD: i32 = 56;
const WIDGET_SPRITE: i32 = home::WIDGET_SIZE as i32 + 2 * WIDGET_PAD;
/// Grid icons' names (as on iOS; the dock's icons have none).
const HOME_LABEL_SIZE: f32 = 33.0;
/// A layer with one name, and where its baseline is.
const NAME_W: i32 = 320;
const NAME_H: i32 = 72;
const NAME_BASELINE: f32 = 50.0;
/// Jiggle sprites: an icon and its shadow, turned (home::JIGGLE_FRAMES
/// ways) side by side, each frame the icon plus this margin.
const SPRITE_PAD: i32 = 16;
const SPRITE: i32 = ICON_SIZE + 2 * SPRITE_PAD;
/// The removal badge's layer (its shadow around it).
const BADGE_LAYER: i32 = 80;
/// How much the App Library darkens the wallpaper.
const LIBRARY_DIM: f32 = 0.38;

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
            AppId::Exocortex => load_icon("A26_EXOCORTEX_ICON", EXOCORTEX_ICON_PATH),
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
        AppId::Exocortex => (0x1d2c52, 0x101b33, "E"),
        AppId::Phone | AppId::Messages => (0x6ce87a, 0x1fc04a, ""),
    };
    canvas.fill_rows(|y| gfx::mix(top, bottom, y as f32 / size as f32));
    let c = size as f32 / 2.0;
    match app {
        AppId::Phone => calls::draw_handset(&mut canvas, c, c, 1.6, false, WHITE),
        AppId::Messages => draw_speech_bubble(&mut canvas, c, c - 4.0, 1.0),
        AppId::System | AppId::Browser | AppId::Exocortex => {}
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
    layout: HomeLayout,
    badges: Badges,
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
    /// A dynamic wallpaper's moving layers (stars, moon), GPU mode only.
    sky: Option<[SurfaceId; 2]>,
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
    /// An app page transition (GPU mode): the outgoing page and the live
    /// X mirror as a node source.
    page_snapshot: Option<SurfaceId>,
    mirror_node: Option<SurfaceId>,
    /// The power menu (or a takeover screen: shutting down, critical
    /// battery), its slider knob, and the lock screen's charging card.
    power: Layer<u64>,
    power_knob: Layer<power::PowerAction>,
    charge_card: Layer<Option<u8>>,
    /// Voice's orb (its glow and its core) and its label (whether it was
    /// started locked).
    voice_halo: Layer<()>,
    voice_core: Layer<()>,
    voice_label: Layer<bool>,
    /// The home screen while live (editing, the App Library; home.rs),
    /// in parts: each widget's jiggle sprite (made when a widget is on the
    /// home screen), the dock's shelf, the status bar, the App Library,
    /// edit mode's + and Done buttons and badge, the removal alert, the App
    /// Library's menu, the widget gallery; and each app's jiggle sprite and
    /// name.
    widget_sprites: [Option<Layer<u64>>; Widget::COUNT],
    dock: Layer<()>,
    home_status: Layer<StatusInfo>,
    /// (With the apps' icon badges.)
    library: Layer<Badges>,
    done: Layer<()>,
    plus: Layer<()>,
    badge: Layer<()>,
    alert: Layer<u64>,
    menu: Layer<u64>,
    gallery: Layer<u64>,
    /// (Drawn in idle time after startup, or when first needed.)
    sprites: [Option<SurfaceId>; AppId::COUNT],
    names: [SurfaceId; AppId::COUNT],
    /// Each app's icon badge (badges.rs), by app: its count.
    count_badges: Vec<Layer<u32>>,
}

const DOTS_TOP: f32 = PASSCODE_DOTS_Y - 32.0;
/// How much the passcode keypad darkens the wallpaper.
const DIMMED: f32 = 0.42;
/// A dynamic wallpaper's star layer: this much larger than the screen on
/// every side, room for it to move (far, so it moves least).
const SKY_MARGIN: f32 = 40.0;
/// Its moon: the layer's size (the disc is half of it), centre on screen,
/// and how far it moves (near, so the most).
const MOON_SIZE: f32 = 640.0;
const MOON_CENTER: (f32, f32) = (770.0, 1180.0);
const MOON_TRAVEL: f32 = 90.0;
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
        let mut names = [wall; AppId::COUNT];
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

            let mut canvas = Canvas::new(NAME_W, NAME_H);
            draw_app_name(
                &mut canvas,
                app.display_name(),
                NAME_W as f32 / 2.0,
                NAME_BASELINE,
            );
            let id = compositor.create_layer(conn, NAME_W as u16, NAME_H as u16, true)?;
            compositor.upload(conn, id, &canvas)?;
            names[app.index()] = id;
        }
        let mut count_badges = Vec::with_capacity(AppId::COUNT);
        for _ in AppId::ALL {
            count_badges.push(Layer::new(
                conn,
                &mut compositor,
                badges::LAYER_W,
                badges::LAYER_H,
                true,
            )?);
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
            power: Layer::new(conn, &mut compositor, w, h, true)?,
            power_knob: Layer::new(
                conn,
                &mut compositor,
                power::KNOB as i32,
                power::KNOB as i32,
                true,
            )?,
            charge_card: Layer::new(
                conn,
                &mut compositor,
                power::CARD.w as i32,
                power::CARD.h as i32,
                true,
            )?,
            voice_halo: Layer::new(
                conn,
                &mut compositor,
                voice::HALO as i32,
                voice::HALO as i32,
                true,
            )?,
            voice_core: Layer::new(
                conn,
                &mut compositor,
                voice::CORE as i32,
                voice::CORE as i32,
                true,
            )?,
            voice_label: Layer::new(
                conn,
                &mut compositor,
                voice::LABEL.w as i32,
                voice::LABEL.h as i32,
                true,
            )?,
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
            widget_sprites: [None, None],
            plus: Layer::new(
                conn,
                &mut compositor,
                home::PLUS.w as i32,
                home::PLUS.h as i32,
                true,
            )?,
            gallery: Layer::new(conn, &mut compositor, w, home::GALLERY_H as i32, true)?,
            dock: Layer::new(conn, &mut compositor, DOCK.w as i32, DOCK.h as i32, true)?,
            home_status: Layer::new(
                conn,
                &mut compositor,
                w,
                i32::from(status_bar::HEIGHT),
                true,
            )?,
            library: Layer::new(conn, &mut compositor, w, LIBRARY_H as i32, true)?,
            done: Layer::new(
                conn,
                &mut compositor,
                home::DONE.w as i32,
                home::DONE.h as i32,
                true,
            )?,
            badge: Layer::new(conn, &mut compositor, BADGE_LAYER, BADGE_LAYER, true)?,
            alert: Layer::new(
                conn,
                &mut compositor,
                home::ALERT_W as i32,
                home::alert_rect().h as i32,
                true,
            )?,
            menu: Layer::new(
                conn,
                &mut compositor,
                home::MENU_W as i32,
                home::MENU_H as i32,
                true,
            )?,
            sprites: [None; AppId::COUNT],
            names,
            count_badges,
            compositor,
            wallpaper,
            wallpaper_id: "moon".into(),
            wall,
            sky: None,
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
            page_snapshot: None,
            mirror_node: None,
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

    /// Snapshots the page on screen for an app's page transition. GPU mode
    /// only (the live app moves inside Moon's frame there); false otherwise.
    pub fn capture_page<C: Connection>(&mut self, conn: &C) -> Result<bool, Box<dyn Error>> {
        if !self.compositor.gpu() {
            return Ok(false);
        }
        if self.mirror_node.is_none() {
            self.mirror_node = self.compositor.create_mirror_node();
        }
        let id = match self.page_snapshot {
            Some(id) => id,
            None => {
                let id = self.compositor.create_snapshot(conn)?;
                self.page_snapshot = Some(id);
                id
            }
        };
        self.compositor.capture_root(conn, id)?;
        Ok(self.mirror_node.is_some())
    }

    pub fn page_snapshot_taken(&self) -> bool {
        self.page_snapshot
            .is_some_and(|id| self.compositor.snapshot_taken(id))
    }

    /// The page transition: the status bar stays put (from the live
    /// mirror); below it, a push slides the new page in from the right over
    /// the old one, which drifts left and darkens; a pop is the reverse.
    fn page_nodes(
        &self,
        transition: crate::model::PageTransition,
        t: Instant,
        nodes: &mut Vec<Node>,
    ) -> bool {
        let (Some(old), Some(live)) = (self.page_snapshot, self.mirror_node) else {
            return false;
        };
        let p = transition.progress(t);
        let top = f32::from(status_bar::HEIGHT);
        let body = RectF::new(0.0, top, SCREEN_W, SCREEN_H - top);
        let image = |surface, x: f32| Node::Image {
            surface,
            src: body,
            dst: RectF::new(x.round(), top, SCREEN_W, SCREEN_H - top),
            alpha: 1.0,
            radius: 0.0,
            smooth: false,
        };
        let shade = |x: f32, alpha: f32| Node::Fill {
            dst: RectF::new(x.round(), top, SCREEN_W, SCREEN_H - top),
            color: BLACK,
            alpha,
            radius: 0.0,
        };
        nodes.push(Node::Fill {
            dst: SCREEN,
            color: BLACK,
            alpha: 1.0,
            radius: 0.0,
        });
        let strip = RectF::new(0.0, 0.0, SCREEN_W, top);
        nodes.push(Node::Image {
            surface: live,
            src: strip,
            dst: strip,
            alpha: 1.0,
            radius: 0.0,
            smooth: false,
        });
        // (under, its shade, over): the page beneath drifts by a third.
        let (under, under_x, shade_alpha, over, over_x) = if transition.push {
            (
                old,
                -SCREEN_W * 0.3 * p,
                0.25 * p,
                live,
                SCREEN_W * (1.0 - p),
            )
        } else {
            (
                live,
                -SCREEN_W * 0.3 * (1.0 - p),
                0.25 * (1.0 - p),
                old,
                SCREEN_W * p,
            )
        };
        nodes.push(image(under, under_x));
        nodes.push(shade(under_x, shade_alpha));
        nodes.push(image(over, over_x));
        true
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
            || state.power.menu_visible(display_time)
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
        // waits for it (or for its darkened copies) to be drawn. While the
        // home screen is live (editing, the App Library) its frames are
        // composed from the parts below instead, and it catches up after.
        let live = state.home.live(now);
        if !live {
            let key = HomeKey {
                status: status.clone(),
                date: (time.weekday, time.day, time.month),
                wallpaper: self.wallpaper_id.clone(),
                layout: state.settings.home.clone(),
                badges: state.badges,
            };
            let wallpaper = &self.wallpaper;
            let icons = &self.icons;
            let redrawn = self.home.update(conn, compositor, key, |canvas| {
                canvas.copy_from(wallpaper);
                let layout = &state.settings.home;
                for widget in &layout.widgets {
                    if let Some(rect) = layout.widget_rect(*widget) {
                        draw_widget(canvas, *widget, rect, time, state);
                    }
                }
                canvas.fill_rrect(DOCK, DOCK_RADIUS, WHITE, 0.14);
                for (app, slot) in layout.slots() {
                    let rect = layout.rect(slot);
                    draw_icon_shadow(canvas, rect);
                    canvas.draw_image(&icons[app.index()].home, rect.x as i32, rect.y as i32, 1.0);
                    if let Slot::Grid(_) = slot {
                        draw_app_name(
                            canvas,
                            app.display_name(),
                            rect.center().0,
                            rect.y + rect.h + home::LABEL_DROP,
                        );
                    }
                    let count = state.badges.get(app);
                    if count > 0 {
                        let (x, y) = badges::anchor(rect, 0.0);
                        badges::draw(canvas, count, x.round(), y.round(), 1.0);
                    }
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
        if live {
            self.refresh_live_layers(conn, state, &status, time)?;
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

    /// The live home screen's parts (see `live_home_nodes`).
    fn refresh_live_layers<C: Connection>(
        &mut self,
        conn: &C,
        state: &ShellState,
        status: &StatusInfo,
        time: LocalTime,
    ) -> Result<(), Box<dyn Error>> {
        for app in AppId::ALL {
            self.sprite(conn, app)?;
        }
        self.widget_sprites(conn, state, time)?;
        let compositor = &mut self.compositor;
        self.dock.update(conn, compositor, (), |canvas| {
            canvas.clear(0);
            let shelf = RectF::new(0.0, 0.0, DOCK.w, DOCK.h);
            canvas.fill_rrect(shelf, DOCK_RADIUS, WHITE, 0.14);
        })?;
        let drawn = status.clone();
        self.home_status
            .update(conn, compositor, status.clone(), |canvas| {
                canvas.clear(0);
                draw_status_bar(canvas, &drawn, None);
            })?;
        self.done.update(conn, compositor, (), draw_done_button)?;
        self.plus.update(conn, compositor, (), draw_plus_button)?;
        self.badge.update(conn, compositor, (), draw_badge)?;
        for app in AppId::ALL {
            let count = state.badges.get(app);
            self.count_badges[app.index()].update(conn, compositor, count, |canvas| {
                badges::draw_layer(canvas, count);
            })?;
        }
        let icons = &self.icons;
        self.library
            .update(conn, compositor, state.badges, |canvas| {
                draw_library(canvas, icons, &state.badges);
            })?;
        if let Some(alert) = state.home.alert
            && let Some(item) = alert.item
        {
            self.alert
                .update(conn, compositor, key_of(&(item, alert.pressed)), |canvas| {
                    draw_remove_alert(canvas, item, alert.pressed)
                })?;
        }
        if let Some(gallery) = state.home.gallery {
            let layout = &state.settings.home;
            let rows = Widget::ALL.map(|widget| {
                if layout.widgets.contains(&widget) {
                    GalleryRow::Added
                } else if layout.can_add_widget(widget) {
                    GalleryRow::Add
                } else {
                    GalleryRow::NoRoom
                }
            });
            let key = key_of(&(
                rows,
                gallery.pressed,
                (time.weekday, time.day, time.month),
                state.battery_percent,
                state.battery_charging,
            ));
            self.gallery.update(conn, compositor, key, |canvas| {
                draw_gallery(canvas, &rows, gallery.pressed, time, state);
            })?;
        }
        if let Some(menu) = state.home.menu {
            let pressed = menu.pressed.is_some();
            self.menu
                .update(conn, compositor, key_of(&pressed), |canvas| {
                    draw_library_menu(canvas, pressed);
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
        // The power menu, or a takeover screen in its place.
        let takeover = if let Some(action) = state.power.shutting_down {
            Some(power::TakeoverKey::ShuttingDown(action))
        } else {
            state
                .critical_seconds()
                .map(|seconds| power::TakeoverKey::Critical { seconds })
        };
        if let Some(key) = takeover {
            self.power
                .update(conn, compositor, key_of(&("takeover", &key)), |canvas| {
                    power::draw_takeover(canvas, &key);
                })?;
        } else if let Some(menu) = &state.power.menu {
            let key = power::MenuKey {
                action: menu.action,
                pressed: menu.pressed,
            };
            let drawn = key.clone();
            self.power
                .update(conn, compositor, key_of(&("menu", &key)), |canvas| {
                    power::draw_menu(canvas, &drawn);
                })?;
            let action = menu.action;
            self.power_knob.update(conn, compositor, action, |canvas| {
                power::draw_knob(canvas, action);
            })?;
        }
        if state.power.charging_card.is_some() {
            let percent = state.battery_percent;
            self.charge_card
                .update(conn, compositor, percent, |canvas| {
                    power::draw_charging_card(canvas, percent);
                })?;
        }
        if state.voice.listening.is_some() || state.voice.closing.is_some() {
            self.voice_halo
                .update(conn, compositor, (), voice::draw_halo)?;
            self.voice_core
                .update(conn, compositor, (), voice::draw_core)?;
            let locked = state.voice.locked;
            self.voice_label
                .update(conn, compositor, locked, |canvas| {
                    voice::draw_label(canvas, locked);
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
        // The power menu drops over everything, as a curtain.
        let edge = state
            .power
            .menu
            .as_ref()
            .map_or(0.0, |menu| menu.edge(t).round());
        nodes.push(clipped(deep, 0.0, edge, 1.0));
        nodes.push(clipped(self.power.id, 0.0, edge, 1.0));
        let offset = state
            .power
            .menu
            .as_ref()
            .map_or(0.0, |menu| menu.knob_offset(t).round());
        let (knob_x, knob_y) = power::knob_origin(offset);
        let shown = (edge - knob_y).clamp(0.0, power::KNOB);
        nodes.push(Node::Image {
            surface: self.power_knob.id,
            src: RectF::new(0.0, 0.0, power::KNOB, shown),
            dst: RectF::new(knob_x, knob_y, power::KNOB, shown),
            alpha: if shown > 0.0 { 1.0 } else { 0.0 },
            radius: 0.0,
            smooth: false,
        });
        // The charging card on the lock screen.
        let on_clock =
            state.view == View::Locked && state.lock_face == crate::model::LockFace::Clock;
        let card_alpha = if on_clock {
            state.power.charging_card_alpha(t)
        } else {
            0.0
        };
        nodes.push(Node::Image {
            surface: self.charge_card.id,
            src: RectF::new(0.0, 0.0, power::CARD.w, power::CARD.h),
            dst: power::CARD,
            alpha: (card_alpha * 255.0).round() / 255.0,
            radius: 0.0,
            smooth: false,
        });
        // Voice over everything: the screen dims, the orb breathes and
        // swells with the voice, and the label says what releasing does.
        let alpha = (state.voice.alpha(t) * 255.0).round() / 255.0;
        nodes.push(Node::Fill {
            dst: SCREEN,
            color: BLACK,
            alpha: alpha * voice::DIM,
            radius: 0.0,
        });
        let (cx, cy) = voice::ORB_CENTER;
        for (surface, side, scale) in [
            (self.voice_halo.id, voice::HALO, state.voice.halo_scale(t)),
            (self.voice_core.id, voice::CORE, state.voice.core_scale()),
        ] {
            let size = side * scale;
            nodes.push(Node::Image {
                surface,
                src: RectF::new(0.0, 0.0, side, side),
                dst: RectF::new(cx - size / 2.0, cy - size / 2.0, size, size),
                alpha,
                radius: 0.0,
                smooth: true,
            });
        }
        nodes.push(Node::Image {
            surface: self.voice_label.id,
            src: RectF::new(0.0, 0.0, voice::LABEL.w, voice::LABEL.h),
            dst: voice::LABEL,
            alpha,
            radius: 0.0,
            smooth: false,
        });
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
        if crate::wallpaper::is_dynamic(id) && self.compositor.gpu() && self.sky.is_none() {
            let (w, h) = (self.wallpaper.width, self.wallpaper.height);
            let (sw, sh) = (w + 2 * SKY_MARGIN as i32, h + 2 * SKY_MARGIN as i32);
            let stars = self
                .compositor
                .create_layer(conn, sw as u16, sh as u16, true)?;
            self.compositor
                .upload(conn, stars, &crate::wallpaper::sky_stars(sw, sh))?;
            let moon =
                self.compositor
                    .create_layer(conn, MOON_SIZE as u16, MOON_SIZE as u16, true)?;
            self.compositor
                .upload(conn, moon, &crate::wallpaper::sky_moon(MOON_SIZE as i32))?;
            self.sky = Some([stars, moon]);
        }
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
        state: &ShellState,
        now: Instant,
    ) -> Result<Option<Instant>, Box<dyn Error>> {
        if now < self.next_idle_copy {
            return Ok(Some(self.next_idle_copy));
        }
        // Only copies that do not exist yet (after startup); changed ones
        // are redrawn when a fade next shows them. Then the live home
        // screen's parts, so holding an icon never waits for them: each
        // jiggle sprite, and the other layers.
        let sprite = AppId::ALL
            .into_iter()
            .find(|app| self.sprites[app.index()].is_none());
        if self.wall_levels.missing() {
            self.wall_levels
                .create_some(conn, &mut self.compositor, 1)?;
        } else if self.home_levels.missing() {
            self.home_levels
                .create_some(conn, &mut self.compositor, 1)?;
        } else if let Some(app) = sprite {
            self.sprite(conn, app)?;
        } else if state.screen_awake && self.widget_sprites_stale(state) {
            self.widget_sprites(conn, state, LocalTime::now())?;
        } else if self.library.key.is_none() && state.screen_awake {
            let status = self.status(state);
            self.refresh_live_layers(conn, state, &status, LocalTime::now())?;
        } else {
            return Ok(None);
        }
        self.next_idle_copy = now + IDLE_COPY_INTERVAL;
        Ok(Some(self.next_idle_copy))
    }

    fn widget_sprites_stale(&self, state: &ShellState) -> bool {
        let time = LocalTime::now();
        state.settings.home.widgets.iter().any(|widget| {
            self.widget_sprites[widget.index()]
                .as_ref()
                .is_none_or(|layer| layer.key != Some(widget_key(*widget, time, state)))
        })
    }

    /// The jiggle sprites of the widgets on the home screen, made when
    /// first needed and redrawn when what they show changes.
    fn widget_sprites<C: Connection>(
        &mut self,
        conn: &C,
        state: &ShellState,
        time: LocalTime,
    ) -> Result<(), Box<dyn Error>> {
        for widget in &state.settings.home.widgets {
            let slot = &mut self.widget_sprites[widget.index()];
            if slot.is_none() {
                let width = WIDGET_SPRITE * home::WIDGET_FRAMES as i32;
                *slot = Some(Layer::new(
                    conn,
                    &mut self.compositor,
                    width,
                    WIDGET_SPRITE,
                    true,
                )?);
            }
            let Some(layer) = slot.as_mut() else {
                continue;
            };
            let key = widget_key(*widget, time, state);
            layer.update(conn, &mut self.compositor, key, |canvas| {
                draw_widget_sprite(canvas, *widget, time, state);
            })?;
        }
        Ok(())
    }

    /// An app's jiggle sprite, drawn and uploaded if it is not yet.
    fn sprite<C: Connection>(&mut self, conn: &C, app: AppId) -> Result<SurfaceId, Box<dyn Error>> {
        if let Some(id) = self.sprites[app.index()] {
            return Ok(id);
        }
        let sprite = jiggle_sprite(&self.icons[app.index()].home);
        let id =
            self.compositor
                .create_layer(conn, sprite.width as u16, sprite.height as u16, true)?;
        self.compositor.upload(conn, id, &sprite)?;
        self.sprites[app.index()] = Some(id);
        Ok(id)
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

    /// A dynamic wallpaper's layers over its base, each shifted by the
    /// phone's tilt in proportion to its depth (tilt.rs).
    fn sky_nodes(&self, alpha: f32, nodes: &mut Vec<Node>) {
        let Some([stars, moon]) = self.sky else {
            return;
        };
        if !crate::wallpaper::is_dynamic(&self.wallpaper_id) || !self.compositor.gpu() {
            return;
        }
        let (tx, ty) = crate::tilt::offset();
        let alpha = quantized(alpha);
        let far = SKY_MARGIN * 0.8;
        nodes.push(Node::Image {
            surface: stars,
            src: RectF::new(
                SKY_MARGIN - tx * far,
                SKY_MARGIN - ty * far,
                SCREEN_W,
                SCREEN_H,
            ),
            dst: SCREEN,
            alpha,
            radius: 0.0,
            smooth: true,
        });
        let (cx, cy) = MOON_CENTER;
        nodes.push(Node::Image {
            surface: moon,
            src: RectF::new(0.0, 0.0, MOON_SIZE, MOON_SIZE),
            dst: RectF::new(
                cx - MOON_SIZE / 2.0 + tx * MOON_TRAVEL,
                cy - MOON_SIZE / 2.0 + ty * MOON_TRAVEL,
                MOON_SIZE,
                MOON_SIZE,
            ),
            alpha,
            radius: 0.0,
            smooth: true,
        });
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
        if let Some(square) = state.light_probe.filter(|_| state.screen_awake) {
            for (dst, color) in [(SCREEN, BLACK), (square, WHITE)] {
                nodes.push(Node::Fill {
                    dst,
                    color,
                    alpha: 1.0,
                    radius: 0.0,
                });
            }
            return Frame {
                base: Some(self.wall),
                nodes,
            };
        }
        if state.screen_awake && state.power_takes_over() {
            // Shutting down, or the battery's last seconds: nothing else.
            nodes.push(Node::Fill {
                dst: SCREEN,
                color: BLACK,
                alpha: 1.0,
                radius: 0.0,
            });
            nodes.push(Self::full(self.power.id, SCREEN_W, SCREEN_H, 0.0, 0.0, 1.0));
            return Frame { base: None, nodes };
        }
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
        if let Some(transition) = state.page_transition
            && self.page_nodes(transition, t, &mut nodes)
        {
            self.alert_nodes(state, t, &mut nodes);
            return Frame { base: None, nodes };
        }
        // The centre or the ringing screen over an app: the app's snapshot
        // lies beneath them.
        // (Also on the frame where an overlay over the app has just closed:
        // frames are sampled ahead, at `t`, so the overlay may already be
        // gone there while the app is not back on screen yet. The home
        // screen must never flash in between.)
        let app_beneath = AppId::from_view(state.view).filter(|_| {
            state.launch().is_none() && state.gesture.is_none() && state.returning.is_none()
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
        self.sky_nodes(1.0 - p.clamp(0.0, 1.0) * DIMMED, nodes);
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
        // the switcher, and the held home gesture; or, while it is live
        // (editing, the App Library), composed from its parts.
        let live = state.home.live(t);
        if live {
            self.live_home_nodes(state, t, nodes);
        } else {
            nodes.push(match self.gpu_backdrops[0] {
                Some(id) if self.compositor.gpu() => backdrop_node(id),
                _ => {
                    Self::background_node(&mut self.compositor, &self.home_levels, &self.home_fade)
                }
            });
        }
        // Unlock: the keypad cascades away over the home screen, bottom row
        // first (the lock swipe's cascade in reverse).
        let unlock = unlock_progress(state, t);
        self.keypad_nodes(unlock, 0.0, nodes);

        if !live {
            // A finger on an icon darkens it.
            let layout = &state.settings.home;
            let pressed = state.home.pressed();
            for app in AppId::ALL {
                let rect = layout.icon_rect(app);
                nodes.push(Node::Fill {
                    dst: rect.unwrap_or(RectF::new(0.0, 0.0, 0.0, 0.0)),
                    color: BLACK,
                    alpha: if rect.is_some() && pressed == Some(app) {
                        0.38
                    } else {
                        0.0
                    },
                    radius: ICON_RADIUS,
                });
            }
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

    /// The home screen composed from its parts (while live; home.rs): the
    /// wallpaper stays put while the widgets and the grid go with their
    /// page, the App Library comes in beside them and the dock leaves for
    /// it; every icon is its own node, a pre-turned sprite frame (the
    /// jiggle), with its name and badge. Always the same nodes, so nothing
    /// restructures a frame.
    fn live_home_nodes(&self, state: &ShellState, t: Instant, nodes: &mut Vec<Node>) {
        let home = &state.home;
        let page = home.page(t);
        let shown = page.clamp(0.0, 1.0);
        let shift = (page * SCREEN_W).round();
        nodes.push(backdrop_node(self.wall));
        let widgets = home.widget_views(&state.settings.home, t);
        for index in 0..Widget::COUNT {
            self.widget_nodes(widgets.get(index), nodes);
        }
        nodes.push(Self::full(
            self.dock.id,
            DOCK.w,
            DOCK.h,
            DOCK.x,
            DOCK.y + 80.0 * shown,
            quantized(1.0 - shown),
        ));
        nodes.push(Node::Fill {
            dst: SCREEN,
            color: BLACK,
            alpha: quantized(LIBRARY_DIM * shown),
            radius: 0.0,
        });
        nodes.push(Self::full(
            self.library.id,
            SCREEN_W,
            LIBRARY_H,
            SCREEN_W - shift,
            0.0,
            if page > 0.0 { 1.0 } else { 0.0 },
        ));
        // A finger on an App Library app darkens its icon.
        let pressed = home.pressed();
        let library_pressed = pressed.filter(|_| page >= 0.5);
        let mut held = RectF::new(0.0, 0.0, 0.0, 0.0);
        if let Some(app) = library_pressed {
            held = library_icon(app);
            held.x += SCREEN_W - shift;
        }
        nodes.push(Node::Fill {
            dst: held,
            color: BLACK,
            alpha: if library_pressed.is_some() { 0.38 } else { 0.0 },
            radius: ICON_RADIUS * home::LIBRARY_ICON / ICON_SIZE as f32,
        });
        let views = home.icon_views(&state.settings.home, t);
        let pressed = pressed.filter(|_| page < 0.5);
        for index in 0..AppId::COUNT {
            self.icon_nodes(views.get(index), pressed, &state.badges, nodes);
        }
        nodes.push(Self::full(
            self.done.id,
            home::DONE.w,
            home::DONE.h,
            home::DONE.x,
            home::DONE.y,
            quantized(home.edit_amount(t)),
        ));
        nodes.push(Self::full(
            self.plus.id,
            home::PLUS.w,
            home::PLUS.h,
            home::PLUS.x,
            home::PLUS.y,
            quantized(home.edit_amount(t)),
        ));
        nodes.push(Self::full(
            self.home_status.id,
            SCREEN_W,
            f32::from(status_bar::HEIGHT),
            0.0,
            0.0,
            1.0,
        ));
        // The App Library's menu: the page dims, the held app's icon rises
        // a little over it, and the menu shows under its row.
        let menu = home.menu;
        let alpha = quantized(menu.map_or(0.0, |menu| menu.alpha(t)));
        nodes.push(Node::Fill {
            dst: SCREEN,
            color: BLACK,
            alpha: 0.35 * alpha,
            radius: 0.0,
        });
        let app = menu.and_then(|menu| menu.app()).unwrap_or(AppId::System);
        let (cx, cy) = library_icon(app).center();
        let size = SPRITE as f32 * home::LIBRARY_ICON * 1.08 / ICON_SIZE as f32;
        nodes.push(Node::Image {
            surface: self.sprites[app.index()].unwrap_or(self.wall),
            src: sprite_frame(home::JIGGLE_FRAMES / 2),
            dst: RectF::new(
                cx + SCREEN_W - shift - size / 2.0,
                cy - size / 2.0,
                size,
                size,
            ),
            alpha,
            radius: 0.0,
            smooth: true,
        });
        let rect = home::menu_rect(app);
        nodes.push(Self::full(
            self.menu.id,
            rect.w,
            rect.h,
            rect.x + SCREEN_W - shift,
            rect.y,
            alpha,
        ));
        // The removal alert pops in over a dimmed screen.
        let alert = home.alert;
        let alpha = quantized(alert.map_or(0.0, |alert| alert.alpha(t)));
        nodes.push(Node::Fill {
            dst: SCREEN,
            color: BLACK,
            alpha: 0.4 * alpha,
            radius: 0.0,
        });
        let card = home::alert_rect();
        let scale = alert.map_or(1.0, |alert| alert.scale(t));
        let (cx, cy) = card.center();
        let (w, h) = (card.w * scale, card.h * scale);
        let still = (scale - 1.0).abs() < 1e-3;
        nodes.push(Node::Image {
            surface: self.alert.id,
            src: RectF::new(0.0, 0.0, card.w, card.h),
            dst: if still {
                card
            } else {
                RectF::new(cx - w / 2.0, cy - h / 2.0, w, h)
            },
            alpha,
            radius: 0.0,
            smooth: !still,
        });
        // The widget gallery rises from the bottom edge over a dimmed
        // screen.
        let shown = quantized(home.gallery.map_or(0.0, |gallery| gallery.alpha(t)));
        nodes.push(Node::Fill {
            dst: SCREEN,
            color: BLACK,
            alpha: 0.4 * shown,
            radius: 0.0,
        });
        let sheet = home::gallery_rect();
        nodes.push(Self::full(
            self.gallery.id,
            sheet.w,
            sheet.h,
            0.0,
            sheet.y + (1.0 - shown) * sheet.h,
            if shown > 0.0 { 1.0 } else { 0.0 },
        ));
        // The switcher's (or the home gesture's) dim over all of it.
        nodes.push(Node::Fill {
            dst: SCREEN,
            color: BLACK,
            alpha: quantized(home_dim(state, t)),
            radius: 0.0,
        });
    }

    /// A widget's sprite frame and its badge (invisible without a `view`).
    fn widget_nodes(&self, view: Option<&WidgetView>, nodes: &mut Vec<Node>) {
        let surface = view.and_then(|view| {
            self.widget_sprites[view.widget.index()]
                .as_ref()
                .map(|layer| layer.id)
        });
        let (Some(view), Some(surface)) = (view, surface) else {
            for surface in [self.wall, self.badge.id] {
                nodes.push(Node::Image {
                    surface,
                    src: RectF::new(0.0, 0.0, 1.0, 1.0),
                    dst: RectF::new(0.0, 0.0, 1.0, 1.0),
                    alpha: 0.0,
                    radius: 0.0,
                    smooth: false,
                });
            }
            return;
        };
        let scale = view.rect.w / home::WIDGET_SIZE;
        let size = WIDGET_SPRITE as f32 * scale;
        let still = (scale - 1.0).abs() < 1e-3;
        let (cx, cy) = view.rect.center();
        let (x, y) = (cx - size / 2.0, cy - size / 2.0);
        let frame = home::jiggle_frame(view.angle, home::WIDGET_FRAMES);
        nodes.push(Node::Image {
            surface,
            src: RectF::new(
                (frame as i32 * WIDGET_SPRITE) as f32,
                0.0,
                WIDGET_SPRITE as f32,
                WIDGET_SPRITE as f32,
            ),
            dst: if still {
                RectF::new(x.round(), y.round(), size, size)
            } else {
                RectF::new(x, y, size, size)
            },
            alpha: quantized(view.alpha),
            radius: 0.0,
            smooth: !still,
        });
        let (bx, by) = view.badge_center();
        let half = BADGE_LAYER as f32 / 2.0;
        nodes.push(Self::full(
            self.badge.id,
            BADGE_LAYER as f32,
            BADGE_LAYER as f32,
            bx - half,
            by - half,
            quantized(view.badge * view.alpha),
        ));
    }

    /// An icon's name, its sprite frame, its pressed shade, its "−" badge
    /// and its count badge (all invisible without a `view`).
    fn icon_nodes(
        &self,
        view: Option<&IconView>,
        pressed: Option<AppId>,
        badges: &Badges,
        nodes: &mut Vec<Node>,
    ) {
        let hidden = |surface| Node::Image {
            surface,
            src: RectF::new(0.0, 0.0, 1.0, 1.0),
            dst: RectF::new(0.0, 0.0, 1.0, 1.0),
            alpha: 0.0,
            radius: 0.0,
            smooth: false,
        };
        let Some(view) = view else {
            nodes.push(hidden(self.names[0]));
            nodes.push(hidden(self.wall));
            nodes.push(Node::Fill {
                dst: RectF::new(0.0, 0.0, 0.0, 0.0),
                color: BLACK,
                alpha: 0.0,
                radius: 0.0,
            });
            nodes.push(hidden(self.badge.id));
            nodes.push(hidden(self.count_badges[0].id));
            return;
        };
        let index = view.app.index();
        let icon = ICON_SIZE as f32;
        let scale = view.rect.w / icon;
        let (cx, cy) = view.rect.center();
        let name_x = cx - NAME_W as f32 / 2.0 + view.label_shift();
        let name_y = cy + (icon / 2.0 + home::LABEL_DROP) * scale - NAME_BASELINE;
        nodes.push(Node::Image {
            surface: self.names[index],
            src: RectF::new(0.0, 0.0, NAME_W as f32, NAME_H as f32),
            dst: RectF::new(name_x.round(), name_y.round(), NAME_W as f32, NAME_H as f32),
            alpha: quantized(view.label * view.alpha),
            radius: 0.0,
            smooth: false,
        });
        // At rest size the frame lands on whole pixels, as the drawn home
        // screen has it; lifted or shrinking, it is scaled smoothly.
        let size = SPRITE as f32 * scale;
        let still = (scale - 1.0).abs() < 1e-3;
        let (x, y) = (cx - size / 2.0, cy - size / 2.0);
        nodes.push(Node::Image {
            surface: self.sprites[index].unwrap_or(self.wall),
            src: sprite_frame(home::jiggle_frame(view.angle, home::JIGGLE_FRAMES)),
            dst: if still {
                RectF::new(x.round(), y.round(), size, size)
            } else {
                RectF::new(x, y, size, size)
            },
            alpha: quantized(view.alpha),
            radius: 0.0,
            smooth: !still,
        });
        let shade = pressed == Some(view.app) && !view.lifted;
        nodes.push(Node::Fill {
            dst: RectF::new(
                view.rect.x.round(),
                view.rect.y.round(),
                view.rect.w,
                view.rect.h,
            ),
            color: BLACK,
            alpha: if shade { 0.38 } else { 0.0 },
            radius: ICON_RADIUS * scale,
        });
        let (bx, by) = view.badge_center();
        let half = BADGE_LAYER as f32 / 2.0;
        nodes.push(Self::full(
            self.badge.id,
            BADGE_LAYER as f32,
            BADGE_LAYER as f32,
            bx - half,
            by - half,
            quantized(view.badge * view.alpha),
        ));
        // The count badge rides its icon's top-right corner, turning and
        // scaling with it; at rest it lands where the home page draws it.
        let (ax, ay) = badges::anchor(view.rect, view.angle);
        let (lx, ly) = badges::layer_anchor();
        let (w, h) = (badges::LAYER_W as f32, badges::LAYER_H as f32);
        let (x, y) = (ax - lx * scale, ay - ly * scale);
        nodes.push(Node::Image {
            surface: self.count_badges[index].id,
            src: RectF::new(0.0, 0.0, w, h),
            dst: if still {
                RectF::new(x.round(), y.round(), w, h)
            } else {
                RectF::new(x, y, w * scale, h * scale)
            },
            alpha: if badges.get(view.app) > 0 {
                quantized(view.alpha)
            } else {
                0.0
            },
            radius: 0.0,
            smooth: !still,
        });
    }

    pub fn destroy<C: Connection>(&mut self, conn: &C) {
        self.compositor.destroy(conn);
    }
}

/// Opacity in the compositor's 8-bit steps, so a settling fade stops
/// redrawing once its change is invisible.
fn quantized(alpha: f32) -> f32 {
    (alpha.clamp(0.0, 1.0) * 255.0).round() / 255.0
}

/// The App Library's height (its search field and rows).
const LIBRARY_H: f32 = 1200.0;

/// An app's icon in the App Library, on its page.
fn library_icon(app: AppId) -> RectF {
    let index = home::library_apps()
        .iter()
        .position(|a| *a == app)
        .unwrap_or_default();
    home::library_icon_rect(index)
}

/// Jiggle sprite frame `frame`, in its layer.
fn sprite_frame(frame: usize) -> RectF {
    RectF::new(
        (frame as i32 * SPRITE) as f32,
        0.0,
        SPRITE as f32,
        SPRITE as f32,
    )
}

/// An icon's soft shadow, on the home screen and in its sprite.
fn draw_icon_shadow(canvas: &mut Canvas, rect: RectF) {
    canvas.shadow_rrect(rect.inset(6.0), ICON_RADIUS, 20.0, BLACK, 0.18);
}

/// A grid icon's name, centered on `cx`; a soft shadow keeps it legible
/// on any wallpaper.
fn draw_app_name(canvas: &mut Canvas, name: &str, cx: f32, baseline: f32) {
    text::draw_centered(
        canvas,
        name,
        cx + 1.5,
        baseline + 2.0,
        Weight::Medium,
        HOME_LABEL_SIZE,
        BLACK,
        0.45,
    );
    text::draw_centered(
        canvas,
        name,
        cx,
        baseline,
        Weight::Medium,
        HOME_LABEL_SIZE,
        WHITE,
        1.0,
    );
}

/// An icon and its shadow turned each of the jiggle's ways, side by side.
/// The middle frame is drawn exactly as the home screen draws the icon, so
/// edit mode starts and ends without a visible step.
fn jiggle_sprite(icon: &Image) -> Canvas {
    // The shadow stays put (soft enough not to show the turn): drawn once.
    let at = SPRITE_PAD as f32;
    let (w, h) = (icon.width as f32, icon.height as f32);
    let mut shadow = Canvas::new(SPRITE, SPRITE);
    draw_icon_shadow(&mut shadow, RectF::new(at, at, w, h));
    let width = SPRITE * home::JIGGLE_FRAMES as i32;
    let mut canvas = Canvas::new(width, SPRITE);
    for (y, row) in shadow.pixels.chunks_exact(SPRITE as usize).enumerate() {
        for frame in 0..home::JIGGLE_FRAMES {
            let start = y * width as usize + frame * SPRITE as usize;
            canvas.pixels[start..start + SPRITE as usize].copy_from_slice(row);
        }
    }
    for frame in 0..home::JIGGLE_FRAMES {
        let x = frame as i32 * SPRITE + SPRITE_PAD;
        let angle = home::frame_angle(frame, home::JIGGLE_FRAMES);
        if angle == 0.0 {
            canvas.draw_image(icon, x, SPRITE_PAD, 1.0);
        } else {
            let (cx, cy) = RectF::new(x as f32, at, w, h).center();
            canvas.draw_image_rotated(icon, cx, cy, angle, 1.0);
        }
    }
    canvas
}

/// Edit mode's "−" badge: a light disc with a dark bar, centered.
fn draw_badge(canvas: &mut Canvas) {
    canvas.clear(0);
    let c = BADGE_LAYER as f32 / 2.0;
    let r = home::BADGE_RADIUS;
    canvas.shadow_rrect(
        RectF::new(c - r, c - r + 2.0, 2.0 * r, 2.0 * r),
        r,
        8.0,
        BLACK,
        0.28,
    );
    canvas.fill_circle(c, c, r, 0xe5e5ea, 0.96);
    canvas.fill_rrect(RectF::new(c - 13.0, c - 3.0, 26.0, 6.0), 3.0, 0x1c1c1e, 1.0);
}

/// Edit mode's + button: iOS's light translucent circle with a dark plus.
fn draw_plus_button(canvas: &mut Canvas) {
    canvas.clear(0);
    let r = home::PLUS.w / 2.0;
    canvas.fill_circle(r, r, r, WHITE, 0.78);
    let mut plus = Path::new();
    plus.stroke_segment((r - 15.0, r), (r + 15.0, r), 6.5);
    plus.stroke_segment((r, r - 15.0), (r, r + 15.0), 6.5);
    canvas.fill_path(&plus, 0x1c1c1e, 1.0);
}

/// Edit mode's Done button: iOS's light translucent pill with dark
/// semibold text.
fn draw_done_button(canvas: &mut Canvas) {
    canvas.clear(0);
    let rect = RectF::new(0.0, 0.0, home::DONE.w, home::DONE.h);
    canvas.fill_rrect(rect, rect.h / 2.0, WHITE, 0.78);
    let baseline = text::centered_baseline(rect.h / 2.0, Weight::SemiBold, DONE_TEXT);
    text::draw_centered(
        canvas,
        "Done",
        rect.w / 2.0,
        baseline,
        Weight::SemiBold,
        DONE_TEXT,
        0x1c1c1e,
        1.0,
    );
}

/// The Done button's label size (15 pt).
const DONE_TEXT: f32 = 41.0;

/// The App Library: a search field, then every app by name, each with its
/// icon (and its badge), as iOS lists them.
fn draw_library(canvas: &mut Canvas, icons: &[AppIcon; AppId::COUNT], badges: &Badges) {
    canvas.clear(0);
    let field = home::LIBRARY_SEARCH;
    canvas.fill_rrect(field, 30.0, 0x767680, 0.36);
    let label = "App Library";
    let size = 44.0;
    let width = text::measure(label, Weight::Regular, size);
    let (cx, cy) = field.center();
    let glass = cx - width / 2.0 - 26.0;
    canvas.stroke_circle(glass - 4.0, cy - 4.0, 13.0, 4.0, SECONDARY, SECONDARY_ALPHA);
    let mut handle = Path::new();
    handle.stroke_segment((glass + 6.0, cy + 6.0), (glass + 15.0, cy + 15.0), 4.5);
    canvas.fill_path(&handle, SECONDARY, SECONDARY_ALPHA);
    text::draw(
        canvas,
        label,
        cx - width / 2.0 + 14.0,
        text::centered_baseline(cy, Weight::Regular, size),
        Weight::Regular,
        size,
        SECONDARY,
        SECONDARY_ALPHA,
    );
    let apps = home::library_apps();
    for (index, app) in apps.iter().enumerate() {
        let row = home::library_row(index);
        let icon = home::library_icon_rect(index);
        canvas.draw_image_scaled(&icons[app.index()].large, icon, 1.0);
        let count = badges.get(*app);
        if count > 0 {
            let (bx, by) = badges::anchor(icon, 0.0);
            badges::draw(canvas, count, bx, by, icon.w / ICON_SIZE as f32);
        }
        let x = icon.x + icon.w + 40.0;
        text::draw(
            canvas,
            app.display_name(),
            x,
            text::centered_baseline(row.y + row.h / 2.0, Weight::Regular, 48.0),
            Weight::Regular,
            48.0,
            WHITE,
            1.0,
        );
        if index + 1 < apps.len() {
            let y = (row.y + row.h).round() as i32;
            canvas.fill_rect(x as i32, y, (SCREEN_W - 48.0 - x) as i32, 2, WHITE, 0.14);
        }
    }
}

/// The removal alert, as iOS words it: `Remove “App”?` (Remove from Home
/// Screen) or `Remove Widget?` (Remove), what that means, and its buttons
/// (0 removes, 1 cancels; `pressed` is lit).
fn draw_remove_alert(canvas: &mut Canvas, item: Item, pressed: Option<usize>) {
    canvas.clear(0);
    let card = RectF::new(0.0, 0.0, home::ALERT_W, home::alert_rect().h);
    let radius = 38.0;
    canvas.fill_rrect(card, radius, 0x252527, 0.97);
    let cx = card.w / 2.0;
    let (title, message, remove) = match item {
        Item::App(app) => (
            format!("Remove \u{201c}{}\u{201d}?", app.display_name()),
            "Removing from Home Screen will keep the app in your App Library.",
            ("Remove from Home Screen", BLUE),
        ),
        Item::Widget(_) => (
            "Remove Widget?".to_owned(),
            "Removing this widget will not delete any data.",
            ("Remove", RED),
        ),
    };
    text::draw_centered(
        canvas,
        &title,
        cx,
        104.0,
        Weight::SemiBold,
        47.0,
        WHITE,
        1.0,
    );
    for (line, text) in wrap(message, Weight::Regular, 37.0, card.w - 110.0)
        .iter()
        .enumerate()
    {
        let baseline = 176.0 + line as f32 * 50.0;
        text::draw_centered(
            canvas,
            text,
            cx,
            baseline,
            Weight::Regular,
            37.0,
            WHITE,
            0.86,
        );
    }
    let buttons = [
        (remove.0, Weight::Regular, remove.1),
        ("Cancel", Weight::SemiBold, BLUE),
    ];
    for (index, (label, weight, color)) in buttons.into_iter().enumerate() {
        let top = home::ALERT_TEXT_H + index as f32 * home::ALERT_BUTTON_H;
        if pressed == Some(index) {
            canvas.set_clip(
                0,
                top as i32,
                card.w as i32,
                (top + home::ALERT_BUTTON_H) as i32,
            );
            canvas.fill_rrect(card, radius, WHITE, 0.1);
            canvas.reset_clip();
        }
        canvas.fill_rect(0, top as i32, card.w as i32, 2, WHITE, 0.16);
        let baseline = text::centered_baseline(top + home::ALERT_BUTTON_H / 2.0, weight, 47.0);
        text::draw_centered(canvas, label, cx, baseline, weight, 47.0, color, 1.0);
    }
}

/// Words of `text` in lines no wider than `width`.
fn wrap(text: &str, weight: Weight, size: f32, width: f32) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if text::measure(&format!("{line} {word}"), weight, size) <= width => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_owned()),
        }
    }
    lines
}

/// The App Library's menu for a removed app: "Add to Home Screen", with
/// iOS's plus-in-a-square glyph.
fn draw_library_menu(canvas: &mut Canvas, pressed: bool) {
    canvas.clear(0);
    let rect = RectF::new(0.0, 0.0, home::MENU_W, home::MENU_H);
    canvas.fill_rrect(rect, 36.0, 0x343436, 0.98);
    if pressed {
        canvas.fill_rrect(rect, 36.0, WHITE, 0.1);
    }
    let cy = rect.h / 2.0;
    text::draw(
        canvas,
        "Add to Home Screen",
        48.0,
        text::centered_baseline(cy, Weight::Regular, 47.0),
        Weight::Regular,
        47.0,
        WHITE,
        1.0,
    );
    let glyph = RectF::new(rect.w - 96.0, cy - 24.0, 48.0, 48.0);
    canvas.stroke_rrect(glyph, 12.0, 4.0, WHITE, 1.0);
    let mut plus = Path::new();
    let (gx, gy) = glyph.center();
    plus.stroke_segment((gx - 11.0, gy), (gx + 11.0, gy), 4.0);
    plus.stroke_segment((gx, gy - 11.0), (gx, gy + 11.0), 4.0);
    canvas.fill_path(&plus, WHITE, 1.0);
}

/// A widget on the home page.
fn draw_widget(
    canvas: &mut Canvas,
    widget: Widget,
    rect: RectF,
    time: LocalTime,
    state: &ShellState,
) {
    match widget {
        Widget::Calendar => draw_calendar_widget(canvas, rect, time),
        Widget::Battery => draw_battery_widget(canvas, rect, state),
    }
}

/// What a widget shows, for redrawing it when that changes.
fn widget_key(widget: Widget, time: LocalTime, state: &ShellState) -> u64 {
    match widget {
        Widget::Calendar => key_of(&(widget, time.weekday, time.day, time.month)),
        Widget::Battery => key_of(&(widget, state.battery_percent, state.battery_charging)),
    }
}

/// A widget and its shadow on a transparent square of WIDGET_SPRITE.
fn widget_image(widget: Widget, time: LocalTime, state: &ShellState) -> Image {
    let mut canvas = Canvas::new(WIDGET_SPRITE, WIDGET_SPRITE);
    let at = WIDGET_PAD as f32;
    let rect = RectF::new(at, at, home::WIDGET_SIZE, home::WIDGET_SIZE);
    draw_widget(&mut canvas, widget, rect, time, state);
    Image {
        width: canvas.width,
        height: canvas.height,
        pixels: canvas.pixels,
    }
}

/// A widget turned each of its jiggle's ways, side by side. The middle
/// frame is drawn as the home page draws it, so edit mode shows no step.
fn draw_widget_sprite(canvas: &mut Canvas, widget: Widget, time: LocalTime, state: &ShellState) {
    canvas.clear(0);
    let image = widget_image(widget, time, state);
    let c = WIDGET_SPRITE as f32 / 2.0;
    for frame in 0..home::WIDGET_FRAMES {
        let x = frame as i32 * WIDGET_SPRITE;
        let angle = home::frame_angle(frame, home::WIDGET_FRAMES);
        // A turned corner stays in its own frame.
        canvas.set_clip(x, 0, x + WIDGET_SPRITE, WIDGET_SPRITE);
        if angle == 0.0 {
            canvas.draw_image(&image, x, 0, 1.0);
        } else {
            canvas.draw_image_rotated(&image, x as f32 + c, c, angle, 1.0);
        }
        canvas.reset_clip();
    }
}

/// What a widget gallery row offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum GalleryRow {
    Add,
    Added,
    /// The page's apps leave no room for it.
    NoRoom,
}

/// The widget gallery, as a small iOS sheet: a grabber, its title and close
/// button, then each widget with a preview, its name, what it shows and an
/// Add Widget button (`pressed` is lit).
fn draw_gallery(
    canvas: &mut Canvas,
    rows: &[GalleryRow; Widget::COUNT],
    pressed: Option<usize>,
    time: LocalTime,
    state: &ShellState,
) {
    canvas.clear(0);
    let sheet = RectF::new(0.0, 0.0, SCREEN_W, home::GALLERY_H);
    // Rounded at the top only: the bottom corners lie past the panel.
    canvas.fill_rrect(
        RectF::new(0.0, 0.0, SCREEN_W, home::GALLERY_H + 80.0),
        56.0,
        CARD,
        1.0,
    );
    canvas.fill_rrect(
        RectF::new(SCREEN_W / 2.0 - 54.0, 20.0, 108.0, 14.0),
        7.0,
        WHITE,
        0.3,
    );
    text::draw(
        canvas,
        "Widgets",
        home::GRID_MARGIN,
        116.0,
        Weight::Bold,
        64.0,
        WHITE,
        1.0,
    );
    let close = home::gallery_close();
    let (cx, cy) = close.center();
    canvas.fill_circle(cx, cy, close.w / 2.0, CARD_RAISED, 1.0);
    let mut cross = Path::new();
    cross.stroke_segment((cx - 13.0, cy - 13.0), (cx + 13.0, cy + 13.0), 6.0);
    cross.stroke_segment((cx + 13.0, cy - 13.0), (cx - 13.0, cy + 13.0), 6.0);
    canvas.fill_path(&cross, SECONDARY, SECONDARY_ALPHA);
    let preview_scale = home::GALLERY_PREVIEW / home::WIDGET_SIZE;
    for (index, widget) in Widget::ALL.into_iter().enumerate() {
        let row = home::gallery_row(index);
        let image = widget_image(widget, time, state);
        let size = WIDGET_SPRITE as f32 * preview_scale;
        let preview_cy = row.y + row.h / 2.0;
        let preview_cx = home::GRID_MARGIN + home::GALLERY_PREVIEW / 2.0;
        canvas.draw_image_scaled(
            &image,
            RectF::new(preview_cx - size / 2.0, preview_cy - size / 2.0, size, size),
            1.0,
        );
        let x = home::GRID_MARGIN + home::GALLERY_PREVIEW + 44.0;
        text::draw(
            canvas,
            widget.name(),
            x,
            preview_cy - 10.0,
            Weight::SemiBold,
            46.0,
            WHITE,
            1.0,
        );
        text::draw(
            canvas,
            widget.description(),
            x,
            preview_cy + 46.0,
            Weight::Regular,
            34.0,
            SECONDARY,
            SECONDARY_ALPHA,
        );
        let button = home::gallery_button(index);
        let (bx, by) = button.center();
        let label = match rows[index] {
            GalleryRow::Add => {
                let lit = pressed == Some(index);
                canvas.fill_rrect(button, button.h / 2.0, BLUE, if lit { 0.7 } else { 1.0 });
                ("Add Widget", WHITE, 1.0)
            }
            GalleryRow::Added => ("Added", SECONDARY, SECONDARY_ALPHA),
            GalleryRow::NoRoom => ("No Room", SECONDARY, SECONDARY_ALPHA),
        };
        let baseline = text::centered_baseline(by, Weight::SemiBold, 36.0);
        text::draw_centered(
            canvas,
            label.0,
            bx,
            baseline,
            Weight::SemiBold,
            36.0,
            label.1,
            label.2,
        );
        if index + 1 < Widget::COUNT {
            let y = (row.y + row.h).round() as i32;
            canvas.fill_rect(
                x as i32,
                y,
                (sheet.w - home::GRID_MARGIN - x) as i32,
                2,
                WHITE,
                0.12,
            );
        }
    }
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
            let carrier = text::fit(carrier, Weight::SemiBold, 38.0, 520.0);
            text::draw(
                canvas,
                &carrier,
                72.0,
                text::centered_baseline(cy, Weight::SemiBold, 38.0),
                Weight::SemiBold,
                38.0,
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
            let width = text::measure(label, Weight::SemiBold, 34.0);
            let baseline = text::centered_baseline(cy, Weight::SemiBold, 34.0);
            let x = edge - 20.0 - width;
            text::draw(
                canvas,
                label,
                x,
                baseline,
                Weight::SemiBold,
                34.0,
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

    /// The largest channel difference between two canvases.
    fn difference(a: &Canvas, b: &Canvas) -> u32 {
        a.pixels
            .iter()
            .zip(&b.pixels)
            .map(|(p, q)| {
                (0..3)
                    .map(|c| {
                        ((p >> (c * 8) & 0xff) as i32 - (q >> (c * 8) & 0xff) as i32).unsigned_abs()
                    })
                    .max()
                    .unwrap_or(0)
            })
            .max()
            .unwrap_or(0)
    }

    /// A region of a transparent canvas as an image.
    fn crop(canvas: &Canvas, x: i32, y: i32, w: i32, h: i32) -> Image {
        let pixels = (y..y + h)
            .flat_map(|row| {
                let start = (row * canvas.width + x) as usize;
                canvas.pixels[start..start + w as usize].to_vec()
            })
            .collect();
        Image {
            width: w,
            height: h,
            pixels,
        }
    }

    #[test]
    fn edit_mode_icons_and_names_match_the_drawn_home_screen() {
        // The unturned sprite frame and the name layer, composited where
        // the home screen draws them, look as it does: entering and leaving
        // edit mode shows no step.
        let icon = fallback_icon(AppId::Exocortex).icon_mask(ICON_SIZE, ICON_RADIUS);
        let rect = home::cell_rect(1, 0);
        let mut drawn = Canvas::new(SCREEN_W as i32, 1200);
        drawn.clear(0x2a3b5c);
        let mut composed = Canvas::new(SCREEN_W as i32, 1200);
        composed.clear(0x2a3b5c);
        draw_icon_shadow(&mut drawn, rect);
        drawn.draw_image(&icon, rect.x as i32, rect.y as i32, 1.0);
        let baseline = rect.y + rect.h + home::LABEL_DROP;
        draw_app_name(&mut drawn, "Exocortex", rect.center().0, baseline);

        let sprite = jiggle_sprite(&icon);
        let middle = sprite_frame(home::JIGGLE_FRAMES / 2);
        let frame = crop(&sprite, middle.x as i32, 0, SPRITE, SPRITE);
        composed.draw_image(
            &frame,
            rect.x as i32 - SPRITE_PAD,
            rect.y as i32 - SPRITE_PAD,
            1.0,
        );
        let mut name = Canvas::new(NAME_W, NAME_H);
        draw_app_name(&mut name, "Exocortex", NAME_W as f32 / 2.0, NAME_BASELINE);
        let name = crop(&name, 0, 0, NAME_W, NAME_H);
        composed.draw_image(
            &name,
            (rect.center().0 - NAME_W as f32 / 2.0) as i32,
            (baseline - NAME_BASELINE) as i32,
            1.0,
        );
        assert!(difference(&drawn, &composed) <= 2);
        // The turned frames differ, and stay inside their frame.
        let turned = crop(&sprite, 0, 0, SPRITE, SPRITE);
        assert_ne!(turned.pixels, frame.pixels);
        for y in 0..SPRITE {
            assert_eq!(turned.pixels[(y * SPRITE) as usize], 0);
            assert_eq!(turned.pixels[(y * SPRITE + SPRITE - 1) as usize], 0);
        }
    }

    #[test]
    fn edit_mode_count_badges_match_the_drawn_home_screen() {
        // A badge's layer, placed where an icon at rest puts it, looks as
        // the home page draws it: entering edit mode shows no step.
        for (rect, count) in [
            (home::cell_rect(3, 0), 7),
            (home::cell_rect(0, 1), 42),
            (home::dock_rect(2, 4), 1234),
        ] {
            let mut drawn = Canvas::new(SCREEN_W as i32, 2340);
            drawn.clear(0x2a3b5c);
            let mut composed = Canvas::new(SCREEN_W as i32, 2340);
            composed.clear(0x2a3b5c);
            let (x, y) = badges::anchor(rect, 0.0);
            badges::draw(&mut drawn, count, x.round(), y.round(), 1.0);
            let mut layer = Canvas::new(badges::LAYER_W, badges::LAYER_H);
            badges::draw_layer(&mut layer, count);
            let (lx, ly) = badges::layer_anchor();
            let layer = crop(&layer, 0, 0, badges::LAYER_W, badges::LAYER_H);
            composed.draw_image(
                &layer,
                (x - lx).round() as i32,
                (y - ly).round() as i32,
                1.0,
            );
            assert!(difference(&drawn, &composed) <= 2, "{count}");
            assert!(drawn.pixels.iter().any(|p| p & 0xffffff == RED));
        }
    }

    #[test]
    fn edit_mode_widgets_match_the_drawn_home_screen() {
        // The unturned widget frame, composited where the home page draws
        // the widget, looks as it does; turned frames stay in their frame.
        let state = ShellState::new(false, 50);
        let time = LocalTime::now();
        for widget in Widget::ALL {
            let rect = RectF::new(75.0, 228.0, home::WIDGET_SIZE, home::WIDGET_SIZE);
            let mut drawn = Canvas::new(SCREEN_W as i32, 800);
            drawn.clear(0x2a3b5c);
            let mut composed = Canvas::new(SCREEN_W as i32, 800);
            composed.clear(0x2a3b5c);
            draw_widget(&mut drawn, widget, rect, time, &state);
            let mut sprite = Canvas::new(WIDGET_SPRITE * home::WIDGET_FRAMES as i32, WIDGET_SPRITE);
            draw_widget_sprite(&mut sprite, widget, time, &state);
            let middle = home::WIDGET_FRAMES as i32 / 2 * WIDGET_SPRITE;
            let frame = crop(&sprite, middle, 0, WIDGET_SPRITE, WIDGET_SPRITE);
            composed.draw_image(&frame, 75 - WIDGET_PAD, 228 - WIDGET_PAD, 1.0);
            assert!(difference(&drawn, &composed) <= 2, "{widget:?}");
            let turned = crop(&sprite, 0, 0, WIDGET_SPRITE, WIDGET_SPRITE);
            assert_ne!(turned.pixels, frame.pixels);
        }
    }

    #[test]
    fn alert_text_wraps_inside_its_card() {
        let lines = wrap(
            "Removing from Home Screen will keep the app in your App Library.",
            Weight::Regular,
            37.0,
            home::ALERT_W - 110.0,
        );
        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert!(text::measure(line, Weight::Regular, 37.0) <= home::ALERT_W - 110.0);
        }
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
    fn done_sits_below_the_status_bar_glyphs_with_room_for_its_label() {
        // A full status bar draws nothing in Done's row (above it, in its
        // columns); the label leaves the pill iOS's padding.
        let mut canvas = Canvas::new(1080, 300);
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
        draw_status_bar(&mut canvas, &status, None);
        let (x0, x1) = (
            home::DONE.x as usize,
            (home::DONE.x + home::DONE.w) as usize,
        );
        let lowest = (0..300)
            .rev()
            .find(|y| {
                canvas.pixels[y * 1080 + x0..y * 1080 + x1]
                    .iter()
                    .any(|p| *p != 0)
            })
            .unwrap();
        assert!(
            (lowest as f32) < home::STATUS_GLYPHS_BOTTOM,
            "lowest {lowest}"
        );
        let label = text::measure("Done", Weight::SemiBold, DONE_TEXT);
        let padding = (home::DONE.w - label) / 2.0;
        assert!((28.0..=40.0).contains(&padding), "padding {padding}");
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
            let t = Instant::now();
            let sprite =
                jiggle_sprite(&fallback_icon(AppId::System).icon_mask(ICON_SIZE, ICON_RADIUS));
            eprintln!(
                "jiggle sprite {:?} ({} px)",
                t.elapsed(),
                sprite.pixels.len()
            );
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
