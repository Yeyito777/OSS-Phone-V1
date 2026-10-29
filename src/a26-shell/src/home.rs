//! The home screen's layout and its editing, as on iPhone. Holding an app
//! icon (or empty space) makes every icon jiggle with a "−" badge; icons are
//! then dragged around the grid and in and out of the dock, and a badge
//! takes its app off the Home Screen (after an alert). Removed apps stay in
//! the App Library, the page beyond the home page: it opens every app and
//! puts a removed one back (hold it for "Add to Home Screen", or drag it
//! home). docs/home-screen.md.
//!
//! The layout persists in Moon's settings (settings.rs, key `home`).
//! Everything here is geometry and state over time, unit tested without X:
//! the launcher's touches come here (the `impl ShellState` below), and
//! `ui.rs` draws `icon_views`.

use std::time::{Duration, Instant};

use serde::{Deserialize, Deserializer, Serialize};

use crate::gfx::{self, RectF};
use crate::interaction::{SCREEN_H, SCREEN_W};
use crate::model::{AppId, ShellState, View};
use crate::motion::{Spring, VelocityTracker, rubber_band};
use crate::notify_client::Haptic;
use crate::ui::{ICON_RADIUS, ICON_SIZE};

/// A home-screen icon's side.
const ICON: f32 = ICON_SIZE as f32;
/// The grid: four columns under the widgets (which span two each).
pub const GRID_MARGIN: f32 = 75.0;
pub const GRID_PITCH: f32 = 254.0;
pub const GRID_COLUMNS: usize = 4;
pub const GRID_ROWS: usize = 4;
/// Apps the grid holds (one page).
pub const GRID_MAX: usize = GRID_COLUMNS * GRID_ROWS;
/// The first grid row, under the widgets, and the rows' pitch.
pub const ROW_TOP: f32 = crate::ui::WIDGET_TOP + crate::ui::WIDGET_SIZE + 66.0;
pub const ROW_PITCH: f32 = 290.0;
/// A grid icon's name: its baseline below the icon.
pub const LABEL_DROP: f32 = 50.0;
/// The dock: a frosted shelf above the bottom edge, at most four apps,
/// centered as a group.
pub const DOCK: RectF = RectF::new(30.0, 2046.0, 1020.0, 264.0);
pub const DOCK_RADIUS: f32 = 98.0;
pub const DOCK_MAX: usize = 4;
const DOCK_PITCH: f32 = 254.0;
/// Touches starting this close to the bottom edge are the home gesture's.
const EDGE_ZONE: f32 = 56.0;
/// How far up a swipe from that edge goes home.
const EDGE_SWIPE: f32 = 60.0;

/// The App Library: a search field, then every app, alphabetically.
pub const LIBRARY_SEARCH: RectF = RectF::new(48.0, 152.0, 984.0, 100.0);
const LIBRARY_TOP: f32 = 300.0;
pub const LIBRARY_ROW: f32 = 168.0;
pub const LIBRARY_ICON: f32 = 120.0;
pub const LIBRARY_ICON_X: f32 = 72.0;

/// Edit mode's Done button (top right, under the status bar), and the area
/// that takes its taps.
pub const DONE: RectF = RectF::new(858.0, 132.0, 156.0, 54.0);
const DONE_TARGET: RectF = RectF::new(800.0, 131.0, 280.0, 110.0);
/// The "−" badge: its radius, its center's inset from the icon's top-left
/// corner, and the reach of a tap on it.
pub const BADGE_RADIUS: f32 = 28.0;
const BADGE_INSET: f32 = 12.0;
const BADGE_TARGET: f32 = 50.0;

/// The removal alert, centered: its text, then two stacked buttons.
pub const ALERT_W: f32 = 748.0;
pub const ALERT_TEXT_H: f32 = 300.0;
pub const ALERT_BUTTON_H: f32 = 122.0;
/// The App Library's menu for a held app: one item, "Add to Home Screen".
pub const MENU_W: f32 = 690.0;
pub const MENU_H: f32 = 124.0;

/// The jiggle shows one of these pre-drawn rotations of each icon (ui.rs),
/// evenly from -JIGGLE_ANGLE to +JIGGLE_ANGLE (radians); the middle one is
/// unrotated.
pub const JIGGLE_FRAMES: usize = 11;
pub const JIGGLE_ANGLE: f32 = 0.03;
/// One wobble (seconds), and the bounce that goes with it (pixels).
const JIGGLE_PERIOD: f32 = 0.26;
const BOUNCE: f32 = 2.0;

/// Holding this long (still) enters edit mode, or holds an App Library app.
const LONG_PRESS: Duration = Duration::from_millis(500);
/// Movement that turns a touch into a drag or a swipe.
const SLOP: f32 = 14.0;
/// A dragged icon rests over a new place this long before the others make
/// room, so passing over icons does not shuffle them.
const DWELL: Duration = Duration::from_millis(90);
/// The dragged icon is this much larger than at rest.
const LIFT_SCALE: f32 = 1.15;
/// Jiggle and badges fade in and out over this long.
const EDIT_FADE: Duration = Duration::from_millis(160);
/// A removed icon shrinks away over this long.
const REMOVE_TIME: Duration = Duration::from_millis(260);
/// The alert and the menu fade out over this long.
const POPUP_FADE: Duration = Duration::from_millis(180);
/// Icons making room move on this spring (response, damping).
const MOVE_RESPONSE: f32 = 0.38;
const MOVE_DAMPING: f32 = 0.84;

/// A place on the home screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Dock(usize),
    Grid(usize),
}

/// The home screen's apps: the dock's, the grid's (in reading order), and
/// those taken off it (still in the App Library). Every app is in exactly
/// one of the three.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct HomeLayout {
    pub dock: Vec<AppId>,
    pub grid: Vec<AppId>,
    pub removed: Vec<AppId>,
}

impl Default for HomeLayout {
    fn default() -> Self {
        Self {
            dock: vec![AppId::Phone, AppId::Browser, AppId::Messages, AppId::System],
            grid: vec![AppId::Exocortex],
            removed: Vec::new(),
        }
    }
}

impl<'de> Deserialize<'de> for HomeLayout {
    /// Never fails: what cannot be read falls back (`from_value`), so a
    /// damaged layout never costs the other settings.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        Ok(Self::from_value(&value))
    }
}

fn unique(apps: Vec<AppId>, seen: &mut Vec<AppId>) -> Vec<AppId> {
    apps.into_iter()
        .filter(|app| {
            let new = !seen.contains(app);
            if new {
                seen.push(*app);
            }
            new
        })
        .collect()
}

impl HomeLayout {
    /// A layout from its parts, made whole: repeated apps count once, the
    /// dock's extra apps lead the grid and the grid's leave it, and an app
    /// the parts never mention (one newer than the file) joins the grid.
    pub fn new(dock: Vec<AppId>, grid: Vec<AppId>, removed: Vec<AppId>) -> Self {
        let mut seen = Vec::new();
        let mut dock = unique(dock, &mut seen);
        let mut grid = unique(grid, &mut seen);
        let mut removed = unique(removed, &mut seen);
        if dock.len() > DOCK_MAX {
            let extra = dock.split_off(DOCK_MAX);
            grid.splice(0..0, extra);
        }
        if grid.len() > GRID_MAX {
            removed.extend(grid.split_off(GRID_MAX));
        }
        for app in AppId::ALL {
            if !seen.contains(&app) {
                if grid.len() < GRID_MAX {
                    grid.push(app);
                } else {
                    removed.push(app);
                }
            }
        }
        Self {
            dock,
            grid,
            removed,
        }
    }

    /// The layout `layout set DOCK GRID` asks for: apps in neither list are
    /// removed.
    pub fn with(dock: Vec<AppId>, grid: Vec<AppId>) -> Self {
        let removed = AppId::ALL
            .into_iter()
            .filter(|app| !dock.contains(app) && !grid.contains(app))
            .collect();
        Self::new(dock, grid, removed)
    }

    /// Reads a saved layout leniently: unknown app ids are skipped, and
    /// anything that is not a layout gives the default.
    fn from_value(value: &serde_json::Value) -> Self {
        let Some(object) = value.as_object() else {
            return Self::default();
        };
        let keys = ["dock", "grid", "removed"];
        if !keys
            .iter()
            .any(|key| object.get(*key).is_some_and(serde_json::Value::is_array))
        {
            return Self::default();
        }
        let list = |key: &str| -> Vec<AppId> {
            object
                .get(key)
                .and_then(serde_json::Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().and_then(AppId::parse))
                        .collect()
                })
                .unwrap_or_default()
        };
        Self::new(list("dock"), list("grid"), list("removed"))
    }

    pub fn slot(&self, app: AppId) -> Option<Slot> {
        if let Some(index) = self.dock.iter().position(|a| *a == app) {
            return Some(Slot::Dock(index));
        }
        self.grid.iter().position(|a| *a == app).map(Slot::Grid)
    }

    pub fn on_home(&self, app: AppId) -> bool {
        self.slot(app).is_some()
    }

    /// Every app on the home screen with its place.
    pub fn slots(&self) -> impl Iterator<Item = (AppId, Slot)> + '_ {
        let dock = self.dock.iter().enumerate();
        let grid = self.grid.iter().enumerate();
        dock.map(|(index, app)| (*app, Slot::Dock(index)))
            .chain(grid.map(|(index, app)| (*app, Slot::Grid(index))))
    }

    pub fn rect(&self, slot: Slot) -> RectF {
        match slot {
            Slot::Dock(index) => dock_rect(index, self.dock.len()),
            Slot::Grid(index) => grid_rect(index),
        }
    }

    pub fn icon_rect(&self, app: AppId) -> Option<RectF> {
        self.slot(app).map(|slot| self.rect(slot))
    }

    fn take(&mut self, app: AppId) {
        self.dock.retain(|a| *a != app);
        self.grid.retain(|a| *a != app);
        self.removed.retain(|a| *a != app);
    }

    /// Takes an app off the Home Screen (it stays in the App Library).
    pub fn remove(&mut self, app: AppId) -> bool {
        if !self.on_home(app) {
            return false;
        }
        self.take(app);
        self.removed.push(app);
        true
    }

    /// Puts a removed app back: the grid's first free place (the dock's
    /// when the grid is full).
    pub fn add(&mut self, app: AppId) -> bool {
        if self.on_home(app) {
            return false;
        }
        let slot = if self.grid.len() < GRID_MAX {
            Slot::Grid(self.grid.len())
        } else {
            Slot::Dock(self.dock.len())
        };
        self.place(app, slot)
    }

    /// Moves `app` to `slot` (at most to the end of its row of apps), from
    /// wherever it was. Nothing changes when the dock or grid is full.
    pub fn place(&mut self, app: AppId, slot: Slot) -> bool {
        let before = self.clone();
        self.take(app);
        match slot {
            Slot::Dock(index) if self.dock.len() < DOCK_MAX => {
                self.dock.insert(index.min(self.dock.len()), app);
            }
            Slot::Grid(index) if self.grid.len() < GRID_MAX => {
                self.grid.insert(index.min(self.grid.len()), app);
            }
            _ => {
                *self = before;
                return false;
            }
        }
        *self != before
    }
}

/// Grid icon `index`, in reading order.
pub fn grid_rect(index: usize) -> RectF {
    let column = (index % GRID_COLUMNS) as f32;
    let row = (index / GRID_COLUMNS) as f32;
    RectF::new(
        GRID_MARGIN + column * GRID_PITCH,
        ROW_TOP + row * ROW_PITCH,
        ICON,
        ICON,
    )
}

/// Dock icon `index` of `count`.
pub fn dock_rect(index: usize, count: usize) -> RectF {
    let (cx, cy) = DOCK.center();
    let x = cx + (index as f32 - (count.max(1) as f32 - 1.0) / 2.0) * DOCK_PITCH;
    RectF::new(x - ICON / 2.0, cy - ICON / 2.0, ICON, ICON)
}

/// Every app, as the App Library lists them (by name).
pub fn library_apps() -> [AppId; AppId::COUNT] {
    let mut apps = AppId::ALL;
    apps.sort_by_key(|app| app.display_name());
    apps
}

fn library_index(app: AppId) -> usize {
    library_apps()
        .iter()
        .position(|a| *a == app)
        .unwrap_or_default()
}

/// App Library row `index`, on its page.
pub fn library_row(index: usize) -> RectF {
    RectF::new(
        0.0,
        LIBRARY_TOP + index as f32 * LIBRARY_ROW,
        SCREEN_W,
        LIBRARY_ROW,
    )
}

pub fn library_icon_rect(index: usize) -> RectF {
    let row = library_row(index);
    RectF::new(
        LIBRARY_ICON_X,
        row.y + (LIBRARY_ROW - LIBRARY_ICON) / 2.0,
        LIBRARY_ICON,
        LIBRARY_ICON,
    )
}

/// The removal alert's card.
pub fn alert_rect() -> RectF {
    let h = ALERT_TEXT_H + 2.0 * ALERT_BUTTON_H;
    RectF::new(
        ((SCREEN_W - ALERT_W) / 2.0).round(),
        ((SCREEN_H - h) / 2.0).round(),
        ALERT_W,
        h,
    )
}

/// The alert's buttons: 0 removes, 1 cancels.
pub fn alert_button(index: usize) -> RectF {
    let card = alert_rect();
    RectF::new(
        card.x,
        card.y + ALERT_TEXT_H + index as f32 * ALERT_BUTTON_H,
        ALERT_W,
        ALERT_BUTTON_H,
    )
}

/// The held app's menu, under its App Library row (above it near the
/// bottom).
pub fn menu_rect(app: AppId) -> RectF {
    let row = library_row(library_index(app));
    let below = row.y + row.h - 4.0;
    let y = if below + MENU_H > SCREEN_H - 200.0 {
        row.y - MENU_H + 4.0
    } else {
        below
    };
    RectF::new(LIBRARY_ICON_X, y, MENU_W, MENU_H)
}

/// The pre-drawn jiggle frame nearest to `angle`.
pub fn jiggle_frame(angle: f32) -> usize {
    let t = (angle / JIGGLE_ANGLE).clamp(-1.0, 1.0) * 0.5 + 0.5;
    (t * (JIGGLE_FRAMES - 1) as f32).round() as usize
}

/// The rotation jiggle frame `frame` shows.
pub fn frame_angle(frame: usize) -> f32 {
    (frame as f32 / (JIGGLE_FRAMES - 1) as f32 * 2.0 - 1.0) * JIGGLE_ANGLE
}

/// An icon's wobble at `now`: rotation (at full swing) and bounce. Each
/// icon is a little out of phase, and of tempo, with the others.
fn jiggle(app: AppId, since: Instant, now: Instant) -> (f32, f32) {
    let t = now.saturating_duration_since(since).as_secs_f32();
    let i = app.index() as f32;
    let period = JIGGLE_PERIOD * (1.0 + 0.07 * (i * 1.7).sin());
    let phase = i * 2.4;
    let turn = std::f32::consts::TAU * t / period + phase;
    (
        JIGGLE_ANGLE * turn.sin(),
        BOUNCE * (2.0 * turn + phase).cos(),
    )
}

/// The place an icon dragged with its center at (`x`, `y`) would take
/// among the others; None over the widgets or a full dock.
fn hover_slot(layout: &HomeLayout, app: AppId, x: f32, y: f32) -> Option<Slot> {
    let mut others = layout.clone();
    others.take(app);
    if y >= DOCK.y - 20.0 {
        let count = others.dock.len();
        if count >= DOCK_MAX {
            return None;
        }
        // Where it would go among count + 1 icons.
        let first = DOCK.center().0 - count as f32 * DOCK_PITCH / 2.0;
        let index = ((x - first) / DOCK_PITCH).round().clamp(0.0, count as f32);
        return Some(Slot::Dock(index as usize));
    }
    if y < ROW_TOP - ROW_PITCH / 2.0 || others.grid.len() >= GRID_MAX {
        return None;
    }
    let column = ((x - GRID_MARGIN - ICON / 2.0) / GRID_PITCH)
        .round()
        .clamp(0.0, (GRID_COLUMNS - 1) as f32) as usize;
    let row = ((y - ROW_TOP - ICON / 2.0) / ROW_PITCH)
        .round()
        .clamp(0.0, (GRID_ROWS - 1) as f32) as usize;
    Some(Slot::Grid(
        (row * GRID_COLUMNS + column).min(others.grid.len()),
    ))
}

/// What an icon looks like on a frame (ui.rs draws it).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconView {
    pub app: AppId,
    /// The icon's square on screen, scaled about its center.
    pub rect: RectF,
    /// Rotation (radians, clockwise on screen), within ±JIGGLE_ANGLE.
    pub angle: f32,
    pub alpha: f32,
    /// The "−" badge's opacity.
    pub badge: f32,
    /// The name's opacity (grid icons have one).
    pub label: f32,
    /// Under the finger, above everything else.
    pub lifted: bool,
}

impl IconView {
    /// Its badge's center: near the top-left corner, turned with it.
    pub fn badge_center(&self) -> (f32, f32) {
        let scale = self.rect.w / ICON;
        let (cx, cy) = self.rect.center();
        let d = (BADGE_INSET - ICON / 2.0) * scale;
        let (sin, cos) = self.angle.sin_cos();
        (cx + d * cos - d * sin, cy + d * sin + d * cos)
    }

    /// How far its name moves sideways as the icon turns (it turns with it).
    pub fn label_shift(&self) -> f32 {
        let scale = self.rect.w / ICON;
        -(ICON / 2.0 + LABEL_DROP - 14.0) * scale * self.angle.sin()
    }
}

/// Edit mode: since when (the jiggle's time origin), and when it began to
/// end.
#[derive(Debug, Clone, Copy)]
struct Editing {
    since: Instant,
    leaving: Option<Instant>,
}

impl Editing {
    /// How much the icons jiggle and the badges show (0 to 1).
    fn amount(&self, now: Instant) -> f32 {
        let fade = EDIT_FADE.as_secs_f32();
        let enter = now.saturating_duration_since(self.since).as_secs_f32() / fade;
        let leave = self.leaving.map_or(1.0, |at| {
            1.0 - now.saturating_duration_since(at).as_secs_f32() / fade
        });
        enter.min(leave).clamp(0.0, 1.0)
    }
}

/// An icon moving to a new place.
#[derive(Debug, Clone, Copy)]
struct IconMotion {
    from: RectF,
    to: RectF,
    spring: Spring,
}

impl IconMotion {
    fn new(from: RectF, to: RectF, now: Instant) -> Self {
        Self {
            from,
            to,
            spring: Spring::new(0.0, 1.0, 0.0, now, MOVE_RESPONSE, MOVE_DAMPING),
        }
    }

    fn rect(&self, now: Instant) -> RectF {
        self.from.lerp(self.to, self.spring.value(now))
    }

    fn settled(&self, now: Instant) -> bool {
        self.spring.settled(now, 0.002)
    }
}

/// An icon under the finger.
#[derive(Debug, Clone, Copy)]
struct Drag {
    app: AppId,
    finger: (f32, f32),
    /// From the finger to the icon's center when lifted, and as it settles
    /// (from the App Library, the icon comes under the finger).
    grab: (f32, f32),
    grab_to: (f32, f32),
    /// Its size when lifted, and when.
    from_size: f32,
    lifted: Instant,
    /// Where it would drop (the others leave it room), and a new place
    /// under it waiting out the dwell.
    target: Option<Slot>,
    hover: Option<(Slot, Instant)>,
}

impl Drag {
    fn lift(&self, now: Instant) -> f32 {
        Spring::new(0.0, 1.0, 0.0, self.lifted, 0.3, 0.8).value(now)
    }

    fn center(&self, now: Instant) -> (f32, f32) {
        let p = self.lift(now).min(1.0);
        (
            self.finger.0 + gfx::lerp(self.grab.0, self.grab_to.0, p),
            self.finger.1 + gfx::lerp(self.grab.1, self.grab_to.1, p),
        )
    }

    fn rect(&self, now: Instant) -> RectF {
        let size = gfx::lerp(self.from_size, ICON * LIFT_SCALE, self.lift(now));
        let (cx, cy) = self.center(now);
        RectF::new(cx - size / 2.0, cy - size / 2.0, size, size)
    }
}

/// What a touch went down on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Icon(AppId),
    Badge(AppId),
    Done,
    Library(AppId),
    Empty,
    /// The bottom edge (the home gesture).
    Edge,
    AlertButton(usize),
    MenuItem,
    /// Off the alert or the menu.
    Outside,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Not decided yet: a tap, a hold, a swipe or a drag.
    Pending,
    /// Held long enough on an app: moving now drags it.
    Held(AppId),
    Paging,
    Dragging,
    /// Decided, and nothing more to do.
    Ignored,
}

#[derive(Debug, Clone, Copy)]
struct Touch {
    start: (f32, f32),
    started: Instant,
    target: Target,
    phase: Phase,
}

/// The removal alert or the App Library's menu, for an app.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Popup {
    pub app: AppId,
    shown: Instant,
    closing: Option<Instant>,
    /// The button under the finger.
    pub pressed: Option<usize>,
}

impl Popup {
    fn new(app: AppId, now: Instant) -> Self {
        Self {
            app,
            shown: now,
            closing: None,
            pressed: None,
        }
    }

    fn open(&self) -> bool {
        self.closing.is_none()
    }

    fn close(&mut self, now: Instant) {
        self.closing.get_or_insert(now);
        self.pressed = None;
    }

    fn entrance(&self, now: Instant) -> f32 {
        gfx::ease_out(now.saturating_duration_since(self.shown).as_secs_f32() / 0.24)
    }

    pub fn alpha(&self, now: Instant) -> f32 {
        let fade = self.closing.map_or(1.0, |at| {
            1.0 - now.saturating_duration_since(at).as_secs_f32() / POPUP_FADE.as_secs_f32()
        });
        (self.entrance(now) * fade).clamp(0.0, 1.0)
    }

    /// It pops in from a little larger, as iOS's alerts do.
    pub fn scale(&self, now: Instant) -> f32 {
        1.0 + 0.12 * (1.0 - self.entrance(now))
    }

    fn animating(&self, now: Instant) -> bool {
        self.entrance(now) < 1.0 || self.closing.is_some()
    }

    fn closed(&self, now: Instant) -> bool {
        self.closing.is_some_and(|at| now >= at + POPUP_FADE)
    }
}

/// An app leaving the home screen: it shrinks away from where it was.
#[derive(Debug, Clone, Copy)]
struct Removal {
    app: AppId,
    rect: RectF,
    label: bool,
    at: Instant,
}

/// The home page and the App Library, side by side: 0 shows the home page,
/// 1 the App Library. Dragged by the finger, settled by a spring.
#[derive(Debug, Clone, Default)]
struct Pager {
    rest: f32,
    spring: Option<Spring>,
    drag: Option<PageDrag>,
}

#[derive(Debug, Clone)]
struct PageDrag {
    start_x: f32,
    start_value: f32,
    x: f32,
    tracker: VelocityTracker,
}

impl PageDrag {
    fn raw(&self) -> f32 {
        self.start_value + (self.start_x - self.x) / SCREEN_W
    }
}

/// Past either page the finger meets resistance.
fn rubber_page(raw: f32) -> f32 {
    let clamped = raw.clamp(0.0, 1.0);
    clamped + rubber_band((raw - clamped) * SCREEN_W, 0.0) / SCREEN_W
}

impl Pager {
    fn value(&self, now: Instant) -> f32 {
        if let Some(drag) = &self.drag {
            return rubber_page(drag.raw());
        }
        self.spring.map_or(self.rest, |spring| spring.value(now))
    }

    fn begin_drag(&mut self, x: f32, now: Instant) {
        let mut tracker = VelocityTracker::default();
        tracker.push(now, x, 0.0);
        self.drag = Some(PageDrag {
            start_x: x,
            start_value: self.value(now),
            x,
            tracker,
        });
        self.spring = None;
    }

    fn move_drag(&mut self, x: f32, now: Instant) {
        if let Some(drag) = self.drag.as_mut() {
            drag.x = x;
            drag.tracker.push(now, x, 0.0);
        }
    }

    fn end_drag(&mut self, now: Instant) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let current = rubber_page(drag.raw());
        let velocity = -drag.tracker.velocity(now).0 / SCREEN_W;
        let projected = current + velocity * 0.2;
        let target = if projected >= 0.5 { 1.0 } else { 0.0 };
        self.rest = target;
        self.spring = Some(Spring::new(current, target, velocity, now, 0.4, 0.92));
    }

    fn animate_to(&mut self, target: f32, now: Instant) {
        let current = self.value(now);
        self.drag = None;
        self.rest = target;
        self.spring = Some(Spring::new(current, target, 0.0, now, 0.4, 1.0));
    }

    fn moving(&self, now: Instant) -> bool {
        self.drag.is_some()
            || self
                .spring
                .is_some_and(|spring| !spring.settled(now, 0.0005))
    }

    /// Drops a settled spring; true when one did.
    fn settle(&mut self, now: Instant) -> bool {
        if self.spring.is_some() && !self.moving(now) {
            self.spring = None;
            return true;
        }
        false
    }
}

/// Something the shell does for the home screen.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Effect {
    /// Open an app, its card growing from its icon (rect, corner radius).
    Launch(AppId, RectF, f32),
    Haptic(Haptic),
    /// The layout changed: save it.
    Save,
    /// For `state`'s `last_action`.
    Action(&'static str),
}

/// The home screen's state between frames; the layout itself lives in the
/// settings.
#[derive(Debug, Default)]
pub struct HomeScreen {
    pager: Pager,
    editing: Option<Editing>,
    motions: [Option<IconMotion>; AppId::COUNT],
    /// Where each icon rests (as last laid out, a drag's room included), on
    /// the home page: when that changes, the icon moves there from where it
    /// is.
    rests: [Option<RectF>; AppId::COUNT],
    laid_out: bool,
    touch: Option<Touch>,
    drag: Option<Drag>,
    pub alert: Option<Popup>,
    pub menu: Option<Popup>,
    removing: Option<Removal>,
    pub effects: Vec<Effect>,
}

impl HomeScreen {
    /// In edit mode (and not on its way out).
    pub fn editing(&self) -> bool {
        self.editing
            .is_some_and(|editing| editing.leaving.is_none())
    }

    /// How much the icons jiggle and their badges show (0 to 1).
    pub fn edit_amount(&self, now: Instant) -> f32 {
        self.editing.map_or(0.0, |editing| editing.amount(now))
    }

    /// 0 on the home page, 1 on the App Library.
    pub fn page(&self, now: Instant) -> f32 {
        self.pager.value(now)
    }

    /// The App Library is the page shown (or settling).
    pub fn on_library(&self) -> bool {
        self.pager.rest >= 0.5
    }

    pub fn dragging(&self) -> Option<AppId> {
        self.drag.map(|drag| drag.app)
    }

    /// The app a finger rests on, highlighted until it lifts or moves.
    pub fn pressed(&self) -> Option<AppId> {
        let touch = self.touch.filter(|touch| touch.phase == Phase::Pending)?;
        match touch.target {
            Target::Icon(app) if !self.editing() => Some(app),
            Target::Library(app) => Some(app),
            _ => None,
        }
    }

    /// Anything beyond the plain home page is on screen: the frame is
    /// composed from the icons (ui.rs) instead of the drawn home screen.
    pub fn live(&self, now: Instant) -> bool {
        self.editing.is_some()
            || self.pager.rest != 0.0
            || self.pager.moving(now)
            || self.motions.iter().any(Option::is_some)
            || self.drag.is_some()
            || self.removing.is_some()
            || self.alert.is_some()
            || self.menu.is_some()
    }

    /// Frames are needed (the jiggle always moves).
    pub fn animating(&self, now: Instant) -> bool {
        self.editing.is_some()
            || self.pager.moving(now)
            || self.motions.iter().any(Option::is_some)
            || self.drag.is_some()
            || self.removing.is_some()
            || self.alert.is_some_and(|popup| popup.animating(now))
            || self.menu.is_some_and(|popup| popup.animating(now))
    }

    /// When a still finger becomes a long press.
    pub fn deadline(&self) -> Option<Instant> {
        let touch = self.touch.filter(|touch| touch.phase == Phase::Pending)?;
        matches!(
            touch.target,
            Target::Icon(_) | Target::Empty | Target::Library(_)
        )
        .then_some(touch.started + LONG_PRESS)
    }

    /// The layout as drawn: with a dragged icon's room.
    fn working(&self, layout: &HomeLayout) -> HomeLayout {
        let mut working = layout.clone();
        if let Some(drag) = &self.drag {
            working.take(drag.app);
            if let Some(slot) = drag.target {
                working.place(drag.app, slot);
            }
        }
        working
    }

    /// Every icon on this frame: the home screen's (dragged room and
    /// motions included), one shrinking away, and the one under the
    /// finger, last.
    pub fn icon_views(&self, layout: &HomeLayout, now: Instant) -> Vec<IconView> {
        let working = self.working(layout);
        let amount = self.edit_amount(now);
        let page = self.pager.value(now);
        let shown = (1.0 - page).clamp(0.0, 1.0);
        let mut views = Vec::with_capacity(AppId::COUNT + 1);
        for (app, slot) in working.slots() {
            if self.drag.is_some_and(|drag| drag.app == app) {
                continue;
            }
            let mut rect = self.motions[app.index()]
                .map_or_else(|| working.rect(slot), |motion| motion.rect(now));
            let (angle, bounce) = self
                .editing
                .map_or((0.0, 0.0), |editing| jiggle(app, editing.since, now));
            rect.y += bounce * amount;
            // The dock leaves (down, fading) for the App Library; the grid
            // goes with its page.
            let (alpha, label) = match slot {
                Slot::Dock(_) => {
                    rect.y += 80.0 * page.clamp(0.0, 1.0);
                    (shown, 0.0)
                }
                Slot::Grid(_) => {
                    rect.x -= page * SCREEN_W;
                    (1.0, 1.0)
                }
            };
            views.push(IconView {
                app,
                rect,
                angle: angle * amount,
                alpha,
                badge: amount,
                label,
                lifted: false,
            });
        }
        if let Some(removal) = self.removing {
            let p = gfx::ease_out(
                now.saturating_duration_since(removal.at).as_secs_f32() / REMOVE_TIME.as_secs_f32(),
            );
            let (cx, cy) = removal.rect.center();
            let size = removal.rect.w * (1.0 - p);
            views.push(IconView {
                app: removal.app,
                rect: RectF::new(cx - size / 2.0, cy - size / 2.0, size, size),
                angle: 0.0,
                alpha: 1.0 - p,
                badge: amount * (1.0 - p),
                label: if removal.label { 1.0 - p } else { 0.0 },
                lifted: false,
            });
        }
        if let Some(drag) = &self.drag {
            views.push(IconView {
                app: drag.app,
                rect: drag.rect(now),
                angle: 0.0,
                alpha: 1.0,
                badge: 0.0,
                label: 0.0,
                lifted: true,
            });
        }
        views
    }

    /// Where an app's icon is now (its card opens from and closes into it):
    /// the App Library's row when that page is shown.
    pub fn icon_rect(&self, layout: &HomeLayout, app: AppId) -> Option<RectF> {
        if self.on_library() {
            return Some(library_icon_rect(library_index(app)));
        }
        layout.icon_rect(app)
    }

    /// Records where icons rest without moving them (before the first
    /// change that should animate).
    fn ensure_laid_out(&mut self, layout: &HomeLayout) {
        if !self.laid_out {
            let working = self.working(layout);
            self.rests = [None; AppId::COUNT];
            for (app, slot) in working.slots() {
                self.rests[app.index()] = Some(working.rect(slot));
            }
            self.laid_out = true;
        }
    }

    /// The layout (or a drag's room) changed: icons whose place changed
    /// move there from where they are.
    fn relayout(&mut self, layout: &HomeLayout, now: Instant) {
        let working = self.working(layout);
        let mut rests = [None; AppId::COUNT];
        for (app, slot) in working.slots() {
            rests[app.index()] = Some(working.rect(slot));
        }
        for app in AppId::ALL {
            let index = app.index();
            match (self.rests[index], rests[index]) {
                (Some(old), Some(new)) if self.laid_out && old != new => {
                    if self.drag.is_none_or(|drag| drag.app != app) {
                        let from = self.motions[index].map_or(old, |motion| motion.rect(now));
                        self.motions[index] = Some(IconMotion::new(from, new, now));
                    }
                }
                (_, None) => self.motions[index] = None,
                _ => {}
            }
        }
        self.rests = rests;
        self.laid_out = true;
    }

    /// Changes the layout (a command, an alert's button, a menu): icons
    /// move to their new places and the layout is saved.
    pub fn change(
        &mut self,
        layout: &mut HomeLayout,
        now: Instant,
        change: impl FnOnce(&mut HomeLayout) -> bool,
    ) -> bool {
        self.ensure_laid_out(layout);
        let changed = change(layout);
        if changed {
            self.relayout(layout, now);
            self.effects.push(Effect::Save);
        }
        changed
    }

    /// Enters edit mode (on the home page).
    pub fn begin_editing(&mut self, now: Instant) -> bool {
        if self.editing() {
            return false;
        }
        // Back in before the jiggle had faded: carry on from there.
        let amount = self.edit_amount(now);
        self.editing = Some(Editing {
            since: now - EDIT_FADE.mul_f32(amount),
            leaving: None,
        });
        if self.pager.rest != 0.0 {
            self.pager.animate_to(0.0, now);
        }
        self.menu = None;
        self.effects.push(Effect::Haptic(Haptic::Medium));
        self.effects.push(Effect::Action("home_edit"));
        true
    }

    /// Leaves edit mode: a dragged icon drops where it is headed, an alert
    /// closes as cancelled, and the jiggle fades out.
    pub fn end_editing(&mut self, layout: &mut HomeLayout, now: Instant) -> bool {
        if !self.editing() {
            return false;
        }
        self.drop_drag(layout, now, true);
        if let Some(alert) = self.alert.as_mut() {
            alert.close(now);
        }
        if let Some(editing) = self.editing.as_mut() {
            editing.leaving = Some(now);
        }
        self.effects.push(Effect::Action("home_edit_done"));
        true
    }

    /// Shows the home page (false) or the App Library (true).
    pub fn show_library(&mut self, library: bool, layout: &mut HomeLayout, now: Instant) {
        if library {
            self.end_editing(layout, now);
        } else if let Some(menu) = self.menu.as_mut() {
            menu.close(now);
        }
        let target = if library { 1.0 } else { 0.0 };
        if self.pager.rest != target || self.pager.drag.is_some() {
            self.pager.animate_to(target, now);
            if !library {
                self.effects.push(Effect::Action("home_page"));
            }
        }
    }

    /// The home gesture (a swipe up from the bottom edge, `ctl home`): out
    /// of edit mode, or back from the App Library. False when there was
    /// nothing to leave.
    pub fn home_button(&mut self, layout: &mut HomeLayout, now: Instant) -> bool {
        if self.editing() {
            return self.end_editing(layout, now);
        }
        if self.pager.rest != 0.0 || self.pager.drag.is_some() {
            self.show_library(false, layout, now);
            return true;
        }
        false
    }

    /// The phone locks: a drag keeps the place it was headed for, and the
    /// home page comes back as it rests.
    pub fn reset(&mut self, layout: &mut HomeLayout, now: Instant) {
        self.drop_drag(layout, now, false);
        *self = Self {
            effects: std::mem::take(&mut self.effects),
            ..Self::default()
        };
    }

    /// An app came forward: edit mode, menus and motions end at once (the
    /// page stays, so the app closes back into the App Library).
    pub fn leave(&mut self, layout: &mut HomeLayout, now: Instant) {
        self.drop_drag(layout, now, false);
        let pager = std::mem::take(&mut self.pager);
        self.reset(layout, now);
        self.pager = Pager {
            drag: None,
            ..pager
        };
    }

    fn target_at(&self, layout: &HomeLayout, x: f32, y: f32, now: Instant) -> Target {
        if let Some(_alert) = self.alert.filter(Popup::open) {
            return (0..2)
                .find(|index| alert_button(*index).contains(x, y))
                .map_or(Target::Outside, Target::AlertButton);
        }
        if let Some(menu) = self.menu.filter(Popup::open) {
            return if menu_rect(menu.app).contains(x, y) {
                Target::MenuItem
            } else {
                Target::Outside
            };
        }
        if y >= SCREEN_H - EDGE_ZONE {
            return Target::Edge;
        }
        let page = self.pager.value(now);
        if !self.editing() && page >= 0.5 {
            let shift = (1.0 - page) * SCREEN_W;
            return library_apps()
                .into_iter()
                .enumerate()
                .find(|(index, _)| library_row(*index).contains(x - shift, y))
                .map_or(Target::Empty, |(_, app)| Target::Library(app));
        }
        let removing = self.removing.map(|removal| removal.app);
        let views: Vec<IconView> = self
            .icon_views(layout, now)
            .into_iter()
            .filter(|view| !view.lifted && Some(view.app) != removing && view.alpha > 0.0)
            .collect();
        if self.editing() {
            for view in &views {
                let (bx, by) = view.badge_center();
                if (x - bx).powi(2) + (y - by).powi(2) <= BADGE_TARGET * BADGE_TARGET {
                    return Target::Badge(view.app);
                }
            }
            if DONE_TARGET.contains(x, y) {
                return Target::Done;
            }
        }
        for view in &views {
            // A grid icon's name below it is part of its target.
            let mut target = view.rect.inset(-40.0);
            if view.label > 0.0 {
                target.h += 40.0;
            }
            target.h = target.h.min(SCREEN_H - EDGE_ZONE - target.y);
            if target.contains(x, y) {
                return Target::Icon(view.app);
            }
        }
        Target::Empty
    }

    pub fn touch_begin(&mut self, layout: &HomeLayout, x: f32, y: f32, now: Instant) {
        self.ensure_laid_out(layout);
        let target = self.target_at(layout, x, y, now);
        if let (Some(alert), Target::AlertButton(index)) = (self.alert.as_mut(), target) {
            alert.pressed = Some(index);
        }
        if let (Some(menu), Target::MenuItem) = (self.menu.as_mut(), target) {
            menu.pressed = Some(0);
        }
        if self.pager.moving(now) && !self.editing() {
            // A finger catches the page where it is.
            self.pager.begin_drag(x, now);
            self.touch = Some(Touch {
                start: (x, y),
                started: now,
                target,
                phase: Phase::Paging,
            });
            return;
        }
        self.touch = Some(Touch {
            start: (x, y),
            started: now,
            target,
            phase: Phase::Pending,
        });
    }

    fn set_phase(&mut self, phase: Phase) {
        if let Some(touch) = self.touch.as_mut() {
            touch.phase = phase;
        }
    }

    pub fn touch_move(&mut self, layout: &mut HomeLayout, x: f32, y: f32, now: Instant) {
        let Some(touch) = self.touch else {
            return;
        };
        let (dx, dy) = (x - touch.start.0, y - touch.start.1);
        let moved = dx.abs().max(dy.abs()) > SLOP;
        // A finger slid off a button no longer presses it.
        if let Some(alert) = self.alert.as_mut() {
            alert.pressed = match touch.target {
                Target::AlertButton(index) if alert_button(index).contains(x, y) => Some(index),
                _ => None,
            };
        }
        if let (Some(menu), Target::MenuItem) = (self.menu.as_mut(), touch.target) {
            menu.pressed = menu_rect(menu.app).contains(x, y).then_some(0);
        }
        match touch.phase {
            Phase::Pending if moved => match touch.target {
                Target::Icon(app) if self.editing() => {
                    if let Some(rect) = self.view_rect(layout, app, now) {
                        self.lift(layout, app, touch.start, rect, false, now);
                        self.set_phase(Phase::Dragging);
                        self.drag_to(layout, x, y, now);
                    }
                }
                Target::Edge if -dy > EDGE_SWIPE => {
                    self.set_phase(Phase::Ignored);
                    self.home_button(layout, now);
                }
                Target::Edge
                | Target::Badge(_)
                | Target::Done
                | Target::AlertButton(_)
                | Target::MenuItem
                | Target::Outside => {}
                _ if !self.editing() && dx.abs() > dy.abs() => {
                    self.pager.begin_drag(touch.start.0, now);
                    self.pager.move_drag(x, now);
                    self.set_phase(Phase::Paging);
                }
                _ => self.set_phase(Phase::Ignored),
            },
            Phase::Held(app) if moved => self.drag_held(layout, app, touch.start, x, y, now),
            Phase::Paging => self.pager.move_drag(x, now),
            Phase::Dragging => self.drag_to(layout, x, y, now),
            _ => {}
        }
    }

    pub fn touch_end(&mut self, layout: &mut HomeLayout, x: f32, y: f32, now: Instant) {
        let Some(touch) = self.touch.take() else {
            return;
        };
        if let Some(alert) = self.alert.as_mut() {
            alert.pressed = None;
        }
        if let Some(menu) = self.menu.as_mut() {
            menu.pressed = None;
        }
        match touch.phase {
            Phase::Pending => self.tap(layout, touch, x, y, now),
            Phase::Paging => {
                self.pager.move_drag(x, now);
                self.pager.end_drag(now);
                if self.pager.rest != 0.0 {
                    self.effects.push(Effect::Action("app_library"));
                }
            }
            Phase::Dragging => self.drop_drag(layout, now, true),
            Phase::Held(_) | Phase::Ignored => {}
        }
    }

    /// A touch was lost (the watchdog, the shell taking over): nothing is
    /// tapped, a drag drops and a swipe settles.
    pub fn cancel_touch(&mut self, layout: &mut HomeLayout, now: Instant) {
        let Some(touch) = self.touch.take() else {
            return;
        };
        match touch.phase {
            Phase::Paging => self.pager.end_drag(now),
            Phase::Dragging => self.drop_drag(layout, now, false),
            _ => {}
        }
        if let Some(alert) = self.alert.as_mut() {
            alert.pressed = None;
        }
    }

    fn tap(&mut self, layout: &mut HomeLayout, touch: Touch, x: f32, y: f32, now: Instant) {
        match touch.target {
            Target::AlertButton(index) => {
                let Some(app) = self.alert.filter(Popup::open).map(|alert| alert.app) else {
                    return;
                };
                if !alert_button(index).contains(x, y) {
                    return;
                }
                if index == 0 {
                    self.remove_app(layout, app, now);
                } else {
                    self.effects.push(Effect::Action("home_remove_cancel"));
                }
                if let Some(alert) = self.alert.as_mut() {
                    alert.close(now);
                }
            }
            Target::Outside => {
                // An alert waits for a button; a menu closes.
                if let Some(menu) = self.menu.as_mut() {
                    menu.close(now);
                }
            }
            Target::MenuItem => {
                let Some(app) = self.menu.filter(Popup::open).map(|menu| menu.app) else {
                    return;
                };
                if menu_rect(app).contains(x, y) {
                    if self.change(layout, now, |layout| layout.add(app)) {
                        self.effects.push(Effect::Haptic(Haptic::Light));
                        self.effects.push(Effect::Action("home_added"));
                    }
                    if let Some(menu) = self.menu.as_mut() {
                        menu.close(now);
                    }
                }
            }
            Target::Badge(app) => {
                if (x - touch.start.0).hypot(y - touch.start.1) > BADGE_TARGET {
                    return;
                }
                self.alert = Some(Popup::new(app, now));
                self.effects.push(Effect::Action("home_remove_alert"));
            }
            Target::Done => {
                if DONE_TARGET.contains(x, y) {
                    self.end_editing(layout, now);
                }
            }
            Target::Icon(app) if !self.editing() => {
                if let Some(rect) = self.view_rect(layout, app, now) {
                    self.effects
                        .push(Effect::Launch(app, rect, ICON_RADIUS * rect.w / ICON));
                }
            }
            Target::Library(app) => {
                let shift = (1.0 - self.pager.value(now)) * SCREEN_W;
                let mut rect = library_icon_rect(library_index(app));
                rect.x += shift;
                self.effects
                    .push(Effect::Launch(app, rect, ICON_RADIUS * rect.w / ICON));
            }
            Target::Empty if self.editing() => {
                self.end_editing(layout, now);
            }
            Target::Edge if touch.start.1 - y > EDGE_SWIPE / 2.0 => {
                self.home_button(layout, now);
            }
            Target::Icon(_) | Target::Empty | Target::Edge => {}
        }
    }

    fn view_rect(&self, layout: &HomeLayout, app: AppId, now: Instant) -> Option<RectF> {
        self.icon_views(layout, now)
            .into_iter()
            .find(|view| view.app == app && !view.lifted)
            .map(|view| view.rect)
    }

    /// Picks an icon up under the finger.
    fn lift(
        &mut self,
        layout: &HomeLayout,
        app: AppId,
        finger: (f32, f32),
        from: RectF,
        under_finger: bool,
        now: Instant,
    ) {
        self.ensure_laid_out(layout);
        let (cx, cy) = from.center();
        self.motions[app.index()] = None;
        let grab = (cx - finger.0, cy - finger.1);
        self.drag = Some(Drag {
            app,
            finger,
            grab,
            grab_to: if under_finger { (0.0, 0.0) } else { grab },
            from_size: from.w,
            lifted: now,
            target: layout.slot(app),
            hover: None,
        });
        self.effects.push(Effect::Action("home_drag"));
    }

    /// A held app moved: drag it. From the App Library, the home page comes
    /// back in edit mode to take it.
    fn drag_held(
        &mut self,
        layout: &mut HomeLayout,
        app: AppId,
        start: (f32, f32),
        x: f32,
        y: f32,
        now: Instant,
    ) {
        let library = self.on_library();
        let from = if library {
            if let Some(menu) = self.menu.as_mut() {
                menu.close(now);
            }
            let shift = (1.0 - self.pager.value(now)) * SCREEN_W;
            let mut rect = library_icon_rect(library_index(app));
            rect.x += shift;
            self.begin_editing(now);
            Some(rect)
        } else {
            self.view_rect(layout, app, now)
        };
        let Some(from) = from else {
            self.set_phase(Phase::Ignored);
            return;
        };
        self.lift(layout, app, start, from, library, now);
        self.set_phase(Phase::Dragging);
        self.drag_to(layout, x, y, now);
    }

    fn drag_to(&mut self, layout: &HomeLayout, x: f32, y: f32, now: Instant) {
        let Some(drag) = self.drag.as_mut() else {
            return;
        };
        drag.finger = (x, y);
        let (cx, cy) = drag.center(now);
        match hover_slot(layout, drag.app, cx, cy) {
            Some(slot) if Some(slot) != drag.target => {
                if drag.hover.is_none_or(|(pending, _)| pending != slot) {
                    drag.hover = Some((slot, now));
                }
            }
            _ => drag.hover = None,
        }
    }

    /// Lets go of a dragged icon: it settles into its place (the end of the
    /// grid, for a removed app dropped nowhere in particular).
    fn drop_drag(&mut self, layout: &mut HomeLayout, now: Instant, haptic: bool) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        let from = drag.rect(now);
        // Let go over a new place before the dwell ran out: it goes there.
        let slot = drag
            .hover
            .map(|(slot, _)| slot)
            .or(drag.target)
            .or_else(|| layout.slot(drag.app))
            .unwrap_or(Slot::Grid(GRID_MAX));
        if layout.place(drag.app, slot) {
            self.effects.push(Effect::Save);
        }
        self.relayout(layout, now);
        let index = drag.app.index();
        if let Some(rest) = self.rests[index] {
            self.motions[index] = Some(IconMotion::new(from, rest, now));
        }
        if haptic {
            self.effects.push(Effect::Haptic(Haptic::Light));
        }
        self.effects.push(Effect::Action("home_drop"));
    }

    /// Takes an app off the home screen; it shrinks away and the others
    /// close the gap.
    fn remove_app(&mut self, layout: &mut HomeLayout, app: AppId, now: Instant) {
        let Some(view) = self
            .icon_views(layout, now)
            .into_iter()
            .find(|view| view.app == app)
        else {
            return;
        };
        if self.change(layout, now, |layout| layout.remove(app)) {
            self.removing = Some(Removal {
                app,
                rect: view.rect,
                label: view.label > 0.0,
                at: now,
            });
            self.effects.push(Effect::Action("home_removed"));
        }
    }

    fn long_press(&mut self, layout: &HomeLayout, touch: Touch, now: Instant) {
        let phase = match touch.target {
            Target::Icon(app) if self.editing() => match self.view_rect(layout, app, now) {
                Some(rect) => {
                    self.lift(layout, app, touch.start, rect, false, now);
                    Phase::Dragging
                }
                None => Phase::Ignored,
            },
            Target::Icon(app) => {
                self.begin_editing(now);
                Phase::Held(app)
            }
            Target::Empty if !self.editing() && self.pager.value(now) < 0.5 => {
                self.begin_editing(now);
                Phase::Ignored
            }
            Target::Library(app) => {
                // A removed app offers to come back; any app can be dragged
                // home from here.
                if !layout.on_home(app) {
                    self.menu = Some(Popup::new(app, now));
                    self.effects.push(Effect::Action("library_menu"));
                }
                self.effects.push(Effect::Haptic(Haptic::Medium));
                Phase::Held(app)
            }
            _ => Phase::Ignored,
        };
        self.set_phase(phase);
    }

    /// Timers: the long press, a drag's dwell, and finished animations.
    /// True when what is drawn changed.
    pub fn tick(&mut self, layout: &mut HomeLayout, now: Instant) -> bool {
        let mut changed = false;
        if let Some(touch) = self.touch
            && touch.phase == Phase::Pending
            && self.deadline().is_some_and(|at| now >= at)
        {
            self.long_press(layout, touch, now);
            changed = true;
        }
        if let Some(drag) = self.drag.as_mut()
            && let Some((slot, since)) = drag.hover
            && now >= since + DWELL
        {
            drag.target = Some(slot);
            drag.hover = None;
            self.relayout(layout, now);
            changed = true;
        }
        for motion in &mut self.motions {
            if motion.is_some_and(|motion| motion.settled(now)) {
                *motion = None;
                changed = true;
            }
        }
        if self
            .removing
            .is_some_and(|removal| now >= removal.at + REMOVE_TIME)
        {
            self.removing = None;
            changed = true;
        }
        if self
            .editing
            .is_some_and(|editing| editing.leaving.is_some_and(|at| now >= at + EDIT_FADE))
        {
            self.editing = None;
            changed = true;
        }
        for popup in [&mut self.alert, &mut self.menu] {
            if popup.is_some_and(|popup| popup.closed(now)) {
                *popup = None;
                changed = true;
            }
        }
        changed |= self.pager.settle(now);
        changed
    }
}

/// `layout ...` on the control socket (docs/settings.md).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LayoutCommand {
    /// Reset Home Screen Layout: the default dock and grid, nothing removed.
    Reset,
    /// The dock and the grid, in order; apps in neither are removed.
    Set(Vec<AppId>, Vec<AppId>),
    Remove(AppId),
    Add(AppId),
    /// Enters or leaves edit mode.
    Edit(bool),
    /// Shows the App Library (true) or the home page.
    Library(bool),
}

impl LayoutCommand {
    pub fn parse(mut parts: std::str::SplitWhitespace<'_>) -> Result<Self, String> {
        let app = |value: Option<&str>| {
            value
                .and_then(|value| AppId::parse(&value.to_ascii_lowercase()))
                .ok_or_else(|| "unknown app".to_string())
        };
        let list = |value: Option<&str>| -> Result<Vec<AppId>, String> {
            match value {
                None => {
                    Err("layout set requires DOCK GRID (comma-separated apps, - for none)".into())
                }
                Some("-") => Ok(Vec::new()),
                Some(text) => text.split(',').map(|name| app(Some(name))).collect(),
            }
        };
        let on_off = |value: Option<&str>, what: &str| match value {
            Some("on") => Ok(true),
            Some("off") => Ok(false),
            _ => Err(format!("layout {what} requires on or off")),
        };
        let command = match parts.next() {
            Some("reset") => Self::Reset,
            Some("set") => {
                let dock = list(parts.next())?;
                let grid = list(parts.next())?;
                let mut all = dock.clone();
                all.extend(&grid);
                if dock.len() > DOCK_MAX {
                    return Err(format!("the dock holds at most {DOCK_MAX} apps"));
                }
                if (1..all.len()).any(|i| all[..i].contains(&all[i])) {
                    return Err("an app may appear once".into());
                }
                Self::Set(dock, grid)
            }
            Some("remove") => Self::Remove(app(parts.next())?),
            Some("add") => Self::Add(app(parts.next())?),
            Some("edit") => Self::Edit(on_off(parts.next(), "edit")?),
            Some("library") => Self::Library(on_off(parts.next(), "library")?),
            _ => {
                return Err(
                    "layout requires reset, set DOCK GRID, remove APP, add APP, edit on|off or library on|off"
                        .into(),
                );
            }
        };
        if parts.next().is_some() {
            return Err("too many arguments".into());
        }
        Ok(command)
    }
}

/// The home screen in `state`.
#[derive(Debug, Clone, Serialize)]
pub struct PublicHome {
    pub editing: bool,
    /// `home` or `library`.
    pub page: &'static str,
    pub dragging: Option<AppId>,
    /// The app the removal alert asks about.
    pub alert: Option<AppId>,
    /// The App Library app whose menu is open.
    pub menu: Option<AppId>,
}

impl ShellState {
    fn home_ready(&self) -> bool {
        self.screen_awake && self.view == View::Launcher
    }

    pub fn home_touch_begin(&mut self, x: f32, y: f32) {
        if self.home_ready() && self.switcher.is_none() {
            self.home
                .touch_begin(&self.settings.home, x, y, Instant::now());
            // The icon under the finger darkens.
            self.redraw = true;
            self.home_effects();
        }
    }

    pub fn home_touch_move(&mut self, x: f32, y: f32) {
        let had_press = self.home.pressed();
        self.home
            .touch_move(&mut self.settings.home, x, y, Instant::now());
        if self.home.pressed() != had_press || self.home.live(Instant::now()) {
            self.redraw = true;
        }
        self.home_effects();
    }

    pub fn home_touch_end(&mut self, x: f32, y: f32) {
        self.home
            .touch_end(&mut self.settings.home, x, y, Instant::now());
        self.redraw = true;
        self.home_effects();
    }

    /// A tap on the home screen (`ctl tap`).
    pub fn home_tap(&mut self, x: f32, y: f32) {
        self.home_touch_begin(x, y);
        self.home_touch_end(x, y);
    }

    pub fn home_cancel_touch(&mut self) {
        self.home
            .cancel_touch(&mut self.settings.home, Instant::now());
        self.home_effects();
    }

    /// The home gesture while home: out of edit mode or the App Library.
    pub fn home_button(&mut self) -> bool {
        let left = self
            .home
            .home_button(&mut self.settings.home, Instant::now());
        self.home_effects();
        left
    }

    pub fn home_reset(&mut self) {
        self.home.reset(&mut self.settings.home, Instant::now());
        self.home_effects();
    }

    pub fn home_tick(&mut self, now: Instant) {
        if self.view != View::Launcher || !self.screen_awake {
            // An app came forward (a notification, `ctl launch`): edit mode
            // and the App Library's menu end with the home screen.
            if self.home.editing.is_some() || self.home.alert.is_some() || self.home.menu.is_some()
            {
                self.home.leave(&mut self.settings.home, now);
                self.home_effects();
            }
            return;
        }
        if self.home.tick(&mut self.settings.home, now) {
            self.redraw = true;
        }
        self.home_effects();
    }

    /// Where an app's icon is now: its card opens from and closes into it
    /// (an app not on the home screen zooms from the middle).
    pub fn home_icon_rect(&self, app: AppId) -> RectF {
        self.home
            .icon_rect(&self.settings.home, app)
            .unwrap_or(RectF::new(456.0, 1086.0, ICON, ICON))
    }

    pub fn home_command(&mut self, command: LayoutCommand) -> Result<(), &'static str> {
        let now = Instant::now();
        let ready = self.home_ready();
        let layout = &mut self.settings.home;
        match command {
            LayoutCommand::Reset => {
                self.home.change(layout, now, |layout| {
                    std::mem::take(layout) != HomeLayout::default()
                });
                self.last_action = "home_layout_reset".into();
            }
            LayoutCommand::Set(dock, grid) => {
                let wanted = HomeLayout::with(dock, grid);
                self.home.change(layout, now, |layout| {
                    std::mem::replace(layout, wanted) != *layout
                });
                self.last_action = "home_layout_set".into();
            }
            LayoutCommand::Remove(app) => {
                if !self.home.change(layout, now, |layout| layout.remove(app)) {
                    return Err("that app is not on the home screen");
                }
                self.last_action = "home_removed".into();
            }
            LayoutCommand::Add(app) => {
                if !self.home.change(layout, now, |layout| layout.add(app)) {
                    return Err("that app is already on the home screen");
                }
                self.last_action = "home_added".into();
            }
            LayoutCommand::Edit(on) => {
                if on {
                    if !ready {
                        return Err("the home screen is not shown");
                    }
                    self.home.begin_editing(now);
                } else {
                    self.home.end_editing(layout, now);
                }
            }
            LayoutCommand::Library(on) => {
                if on && !ready {
                    return Err("the home screen is not shown");
                }
                self.home.show_library(on, layout, now);
                if on {
                    self.last_action = "app_library".into();
                }
            }
        }
        self.home_effects();
        self.redraw = true;
        Ok(())
    }

    pub fn home_public(&self) -> PublicHome {
        PublicHome {
            editing: self.home.editing(),
            page: if self.home.on_library() {
                "library"
            } else {
                "home"
            },
            dragging: self.home.dragging(),
            alert: self.home.alert.filter(Popup::open).map(|alert| alert.app),
            menu: self.home.menu.filter(Popup::open).map(|menu| menu.app),
        }
    }

    fn home_effects(&mut self) {
        if self.home.effects.is_empty() {
            return;
        }
        for effect in std::mem::take(&mut self.home.effects) {
            match effect {
                Effect::Launch(app, rect, radius) => self.begin_launch_from(app, rect, radius),
                Effect::Haptic(style) => self.haptic(style),
                Effect::Save => self.settings_changed = true,
                Effect::Action(action) => self.last_action = action.into(),
            }
        }
        self.redraw = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn layouts_are_made_whole_and_read_leniently() {
        let layout = HomeLayout::default();
        assert_eq!(layout.slot(AppId::Phone), Some(Slot::Dock(0)));
        assert_eq!(layout.slot(AppId::Exocortex), Some(Slot::Grid(0)));
        // Repeats count once, a dock of five spills into the grid, and an
        // app the file never mentions joins the grid.
        let made = HomeLayout::new(
            vec![
                AppId::System,
                AppId::System,
                AppId::Browser,
                AppId::Phone,
                AppId::Messages,
                AppId::Exocortex,
            ],
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(made.dock.len(), DOCK_MAX);
        assert_eq!(made.grid, vec![AppId::Exocortex]);
        let read: HomeLayout = serde_json::from_str(
            r#"{"dock":["phone","pager","phone"],"grid":["system"],"removed":["browser"],"x":1}"#,
        )
        .unwrap();
        assert_eq!(read.dock, vec![AppId::Phone]);
        assert_eq!(
            read.grid,
            vec![AppId::System, AppId::Messages, AppId::Exocortex]
        );
        assert_eq!(read.removed, vec![AppId::Browser]);
        for broken in ["7", r#""dock""#, "{}", r#"{"dock":3}"#] {
            let read: HomeLayout = serde_json::from_str(broken).unwrap();
            assert_eq!(read, HomeLayout::default(), "{broken}");
        }
        // Every app is somewhere, exactly once.
        let mut all: Vec<AppId> = read
            .dock
            .iter()
            .chain(&read.grid)
            .chain(&read.removed)
            .copied()
            .collect();
        all.sort_by_key(|app| app.index());
        assert_eq!(all, AppId::ALL.to_vec());
    }

    #[test]
    fn layouts_remove_add_and_place() {
        let mut layout = HomeLayout::default();
        assert!(layout.remove(AppId::Browser));
        assert!(!layout.remove(AppId::Browser));
        assert_eq!(layout.removed, vec![AppId::Browser]);
        assert_eq!(layout.dock.len(), 3);
        // Three dock icons stay centered.
        assert_eq!(layout.rect(Slot::Dock(1)).center().0, DOCK.center().0);
        assert!(layout.add(AppId::Browser));
        assert_eq!(layout.slot(AppId::Browser), Some(Slot::Grid(1)));
        assert!(!layout.add(AppId::Browser));
        // The dock takes a fourth app, not a fifth.
        assert!(layout.place(AppId::Browser, Slot::Dock(0)));
        assert_eq!(layout.dock[0], AppId::Browser);
        assert!(!layout.place(AppId::Exocortex, Slot::Dock(2)));
        assert_eq!(layout.slot(AppId::Exocortex), Some(Slot::Grid(0)));
        // Within the grid, a place past the end is the end.
        assert!(layout.place(AppId::Phone, Slot::Grid(9)));
        assert_eq!(layout.slot(AppId::Phone), Some(Slot::Grid(1)));
        assert_eq!(
            HomeLayout::with(vec![AppId::Phone], vec![AppId::System]).removed,
            vec![AppId::Browser, AppId::Messages, AppId::Exocortex]
        );
    }

    #[test]
    fn geometry_matches_the_drawn_home_screen() {
        // The original grid row under the widgets and the four-icon dock.
        assert_eq!(grid_rect(0), RectF::new(75.0, 674.0, ICON, ICON));
        assert_eq!(grid_rect(5).x, 75.0 + GRID_PITCH);
        assert_eq!(dock_rect(0, 4).x, 540.0 - 1.5 * DOCK_PITCH - ICON / 2.0);
        // The bottom grid row and its names clear the dock.
        let last = grid_rect(GRID_MAX - 1);
        assert!(last.y + last.h + LABEL_DROP + 20.0 < DOCK.y);
        assert!(grid_rect(0).y > crate::ui::WIDGET_TOP + crate::ui::WIDGET_SIZE);
        // Jiggle frames run evenly through zero.
        assert_eq!(jiggle_frame(0.0), JIGGLE_FRAMES / 2);
        assert_eq!(frame_angle(JIGGLE_FRAMES / 2), 0.0);
        assert_eq!(jiggle_frame(JIGGLE_ANGLE * 2.0), JIGGLE_FRAMES - 1);
        assert_eq!(jiggle_frame(-JIGGLE_ANGLE), 0);
        assert_eq!(
            library_apps()[0],
            AppId::Browser,
            "the App Library lists apps by name"
        );
    }

    #[test]
    fn hover_finds_grid_and_dock_places() {
        let layout = HomeLayout::default();
        // Over the second grid column: after Exocortex.
        let (x, y) = grid_rect(1).center();
        assert_eq!(hover_slot(&layout, AppId::Phone, x, y), Some(Slot::Grid(1)));
        // Far down the grid: the end of the grid.
        let (x, y) = grid_rect(14).center();
        assert_eq!(hover_slot(&layout, AppId::Phone, x, y), Some(Slot::Grid(1)));
        // A dock app moves within the dock; a grid app finds the dock full.
        let (x, y) = dock_rect(3, 4).center();
        assert_eq!(hover_slot(&layout, AppId::Phone, x, y), Some(Slot::Dock(3)));
        assert_eq!(hover_slot(&layout, AppId::Exocortex, x, y), None);
        // Over the widgets: nowhere.
        assert_eq!(hover_slot(&layout, AppId::Phone, 300.0, 300.0), None);
    }

    /// Runs `ms` milliseconds of ticks.
    fn run(home: &mut HomeScreen, layout: &mut HomeLayout, from: Instant, ms: u64) {
        for step in (0..=ms).step_by(8) {
            home.tick(layout, at(from, step));
        }
    }

    #[test]
    fn holding_an_icon_enters_edit_mode_and_moving_drags_it() {
        let start = Instant::now();
        let mut layout = HomeLayout::default();
        let mut home = HomeScreen::default();
        let (x, y) = grid_rect(0).center();
        home.touch_begin(&layout, x, y, start);
        assert_eq!(home.pressed(), Some(AppId::Exocortex));
        assert_eq!(home.deadline(), Some(at(start, 500)));
        home.tick(&mut layout, at(start, 499));
        assert!(!home.editing());
        home.tick(&mut layout, at(start, 500));
        assert!(home.editing(), "a long press enters edit mode");
        assert!(home.effects.contains(&Effect::Haptic(Haptic::Medium)));
        assert_eq!(home.pressed(), None);
        // Held and moved: the icon follows the finger, enlarged.
        home.touch_move(&mut layout, x + 40.0, y, at(start, 600));
        assert_eq!(home.dragging(), Some(AppId::Exocortex));
        let lifted = home.icon_views(&layout, at(start, 1000));
        let view = lifted.last().unwrap();
        assert!(view.lifted && view.rect.w > ICON * 1.1);
        assert!((view.rect.center().0 - (x + 40.0)).abs() < 1.0);
        // Over the third column for longer than the dwell, then dropped.
        let (tx, ty) = grid_rect(2).center();
        home.touch_move(&mut layout, tx, ty, at(start, 700));
        home.tick(&mut layout, at(start, 750));
        home.tick(&mut layout, at(start, 800));
        home.touch_end(&mut layout, tx, ty, at(start, 820));
        // Only Exocortex is in the grid: it stays first.
        assert_eq!(layout.slot(AppId::Exocortex), Some(Slot::Grid(0)));
        assert!(home.editing(), "a drop stays in edit mode");
        // It settles into its place.
        run(&mut home, &mut layout, at(start, 820), 1500);
        let views = home.icon_views(&layout, at(start, 2400));
        let view = views.iter().find(|v| v.app == AppId::Exocortex).unwrap();
        assert!((view.rect.x - grid_rect(0).x).abs() < 1.0);
        assert_eq!(view.rect.w, ICON);
    }

    #[test]
    fn dragging_a_dock_icon_into_the_grid_makes_room_and_saves() {
        let start = Instant::now();
        let mut layout = HomeLayout::default();
        let mut home = HomeScreen::default();
        assert!(home.begin_editing(start));
        // In edit mode a touch that moves drags at once.
        let (x, y) = dock_rect(1, 4).center();
        home.touch_begin(&layout, x, y, at(start, 300));
        home.touch_move(&mut layout, x, y - 30.0, at(start, 320));
        assert_eq!(home.dragging(), Some(AppId::Browser));
        // Over Exocortex's place: it moves aside once the dwell passes.
        let (gx, gy) = grid_rect(0).center();
        home.touch_move(&mut layout, gx, gy, at(start, 400));
        home.tick(&mut layout, at(start, 450));
        assert!(home.motions[AppId::Exocortex.index()].is_none());
        home.tick(&mut layout, at(start, 500));
        assert!(home.motions[AppId::Exocortex.index()].is_some());
        // The dock closes up around the gap: three icons, centered.
        run(&mut home, &mut layout, at(start, 500), 1200);
        let views = home.icon_views(&layout, at(start, 1700));
        let messages = views.iter().find(|v| v.app == AppId::Messages).unwrap();
        assert!((messages.rect.x - dock_rect(1, 3).x).abs() < 3.0);
        let exocortex = views.iter().find(|v| v.app == AppId::Exocortex).unwrap();
        assert!((exocortex.rect.x - grid_rect(1).x).abs() < 3.0);
        home.touch_end(&mut layout, gx, gy, at(start, 1700));
        assert_eq!(
            layout.dock,
            vec![AppId::Phone, AppId::Messages, AppId::System]
        );
        assert_eq!(layout.grid, vec![AppId::Browser, AppId::Exocortex]);
        assert!(home.effects.contains(&Effect::Save));
        assert!(home.effects.contains(&Effect::Haptic(Haptic::Light)));
        // Let go over a new place at once: it goes there all the same.
        let (x, y) = grid_rect(1).center();
        home.touch_begin(&layout, x, y, at(start, 3000));
        home.touch_move(&mut layout, x, y + 30.0, at(start, 3010));
        let (dx, dy) = dock_rect(3, 4).center();
        home.touch_move(&mut layout, dx, dy, at(start, 3020));
        home.touch_end(&mut layout, dx, dy, at(start, 3030));
        assert_eq!(layout.slot(AppId::Exocortex), Some(Slot::Dock(3)));
    }

    #[test]
    fn the_badge_asks_then_removes_and_the_library_adds_back() {
        let start = Instant::now();
        let mut layout = HomeLayout::default();
        let mut home = HomeScreen::default();
        home.begin_editing(start);
        let t = at(start, 400);
        let views = home.icon_views(&layout, t);
        let browser = views.iter().find(|v| v.app == AppId::Browser).unwrap();
        let (bx, by) = browser.badge_center();
        home.touch_begin(&layout, bx, by, t);
        home.touch_end(&mut layout, bx, by, t);
        assert_eq!(home.alert.map(|alert| alert.app), Some(AppId::Browser));
        // Cancel keeps it.
        let (cx, cy) = alert_button(1).center();
        home.touch_begin(&layout, cx, cy, at(start, 600));
        home.touch_end(&mut layout, cx, cy, at(start, 620));
        assert!(layout.on_home(AppId::Browser));
        run(&mut home, &mut layout, at(start, 620), 300);
        assert!(home.alert.is_none());
        // Remove takes it off; it shrinks away.
        home.touch_begin(&layout, bx, by, at(start, 1000));
        home.touch_end(&mut layout, bx, by, at(start, 1000));
        let (rx, ry) = alert_button(0).center();
        home.touch_begin(&layout, rx, ry, at(start, 1100));
        home.touch_end(&mut layout, rx, ry, at(start, 1120));
        assert_eq!(layout.removed, vec![AppId::Browser]);
        assert!(home.effects.contains(&Effect::Save));
        let shrinking = home.icon_views(&layout, at(start, 1200));
        let gone = shrinking.iter().find(|v| v.app == AppId::Browser).unwrap();
        assert!(gone.rect.w < ICON && gone.alpha < 1.0);
        // Done leaves edit mode.
        let (dx, dy) = DONE.center();
        home.touch_begin(&layout, dx, dy, at(start, 1500));
        home.touch_end(&mut layout, dx, dy, at(start, 1510));
        assert!(!home.editing());
        run(&mut home, &mut layout, at(start, 1510), 400);
        assert!(home.editing.is_none());
        // The App Library: holding the removed app offers it back.
        home.show_library(true, &mut layout, at(start, 2000));
        run(&mut home, &mut layout, at(start, 2000), 1200);
        assert!(home.on_library() && home.live(at(start, 3300)));
        let row = library_row(library_index(AppId::Browser));
        let (lx, ly) = row.center();
        home.touch_begin(&layout, lx, ly, at(start, 3300));
        home.tick(&mut layout, at(start, 3810));
        assert_eq!(home.menu.map(|menu| menu.app), Some(AppId::Browser));
        home.touch_end(&mut layout, lx, ly, at(start, 3900));
        assert!(home.menu.is_some(), "the menu stays once the finger lifts");
        let (mx, my) = menu_rect(AppId::Browser).center();
        home.touch_begin(&layout, mx, my, at(start, 4000));
        home.touch_end(&mut layout, mx, my, at(start, 4010));
        assert_eq!(layout.slot(AppId::Browser), Some(Slot::Grid(1)));
        assert!(layout.removed.is_empty());
    }

    #[test]
    fn edit_mode_ends_with_a_tap_on_empty_space_or_the_home_gesture() {
        let start = Instant::now();
        let mut layout = HomeLayout::default();
        let mut home = HomeScreen::default();
        // A long press on empty space enters too.
        home.touch_begin(&layout, 540.0, 1500.0, start);
        home.tick(&mut layout, at(start, 520));
        assert!(home.editing());
        home.touch_end(&mut layout, 540.0, 1500.0, at(start, 600));
        assert!(home.editing(), "releasing the hold stays in edit mode");
        // An icon tap in edit mode opens nothing.
        let (x, y) = grid_rect(0).center();
        home.touch_begin(&layout, x, y, at(start, 700));
        home.touch_end(&mut layout, x, y, at(start, 750));
        assert!(!home.effects.iter().any(|e| matches!(e, Effect::Launch(..))));
        home.touch_begin(&layout, 540.0, 1500.0, at(start, 800));
        home.touch_end(&mut layout, 540.0, 1500.0, at(start, 850));
        assert!(!home.editing());
        // The swipe up from the bottom edge.
        home.begin_editing(at(start, 1000));
        home.touch_begin(&layout, 540.0, SCREEN_H - 20.0, at(start, 1100));
        home.touch_move(&mut layout, 540.0, SCREEN_H - 200.0, at(start, 1150));
        assert!(!home.editing());
    }

    #[test]
    fn taps_open_apps_from_where_their_icons_are() {
        let start = Instant::now();
        let mut layout = HomeLayout::default();
        let mut home = HomeScreen::default();
        layout.place(AppId::System, Slot::Grid(1));
        let (x, y) = grid_rect(1).center();
        home.touch_begin(&layout, x, y, start);
        home.touch_end(&mut layout, x, y, at(start, 80));
        assert_eq!(
            home.effects.last(),
            Some(&Effect::Launch(AppId::System, grid_rect(1), ICON_RADIUS))
        );
        assert!(!home.live(at(start, 100)));
    }

    #[test]
    fn a_horizontal_swipe_pages_to_the_app_library_and_back() {
        let start = Instant::now();
        let mut layout = HomeLayout::default();
        let mut home = HomeScreen::default();
        home.touch_begin(&layout, 900.0, 1400.0, start);
        for step in 1..=10 {
            home.touch_move(
                &mut layout,
                900.0 - step as f32 * 50.0,
                1402.0,
                at(start, step * 10),
            );
        }
        assert!(home.page(at(start, 100)) > 0.4);
        home.touch_end(&mut layout, 400.0, 1402.0, at(start, 110));
        assert!(home.on_library());
        run(&mut home, &mut layout, at(start, 110), 1500);
        assert!((home.page(at(start, 1700)) - 1.0).abs() < 0.01);
        // Tapping a row opens its app from its row.
        let row = library_row(0);
        home.touch_begin(&layout, 500.0, row.center().1, at(start, 1800));
        home.touch_end(&mut layout, 500.0, row.center().1, at(start, 1850));
        assert!(matches!(
            home.effects.last(),
            Some(Effect::Launch(AppId::Browser, rect, _)) if (rect.x - LIBRARY_ICON_X).abs() < 1.0
        ));
        assert_eq!(
            home.icon_rect(&layout, AppId::Browser),
            Some(library_icon_rect(0))
        );
        // The home gesture comes back to the home page.
        assert!(home.home_button(&mut layout, at(start, 2000)));
        assert!(!home.on_library());
        // A vertical drag does not page.
        let t = at(start, 4000);
        run(&mut home, &mut layout, at(start, 2000), 1900);
        home.touch_begin(&layout, 540.0, 1400.0, t);
        home.touch_move(&mut layout, 545.0, 1200.0, at(start, 4050));
        assert_eq!(home.page(at(start, 4050)), 0.0);
    }

    #[test]
    fn dragging_from_the_library_brings_an_app_home() {
        let start = Instant::now();
        let mut layout = HomeLayout::default();
        layout.remove(AppId::Messages);
        let mut home = HomeScreen::default();
        home.show_library(true, &mut layout, start);
        run(&mut home, &mut layout, start, 1000);
        let (x, y) = library_icon_rect(library_index(AppId::Messages)).center();
        home.touch_begin(&layout, x, y, at(start, 1100));
        home.tick(&mut layout, at(start, 1610));
        assert!(home.menu.is_some());
        home.touch_move(&mut layout, x + 60.0, y + 60.0, at(start, 1700));
        assert!(home.editing() && !home.on_library());
        assert_eq!(home.dragging(), Some(AppId::Messages));
        // Into the dock's free place, at its right end.
        let (dx, dy) = dock_rect(2, 3).center();
        home.touch_move(&mut layout, dx + 200.0, dy, at(start, 1800));
        run(&mut home, &mut layout, at(start, 1800), 200);
        home.touch_end(&mut layout, dx + 200.0, dy, at(start, 2010));
        assert_eq!(layout.slot(AppId::Messages), Some(Slot::Dock(3)));
    }

    #[test]
    fn locking_mid_drag_keeps_the_new_place() {
        let start = Instant::now();
        let mut layout = HomeLayout::default();
        let mut home = HomeScreen::default();
        home.begin_editing(start);
        let (x, y) = grid_rect(0).center();
        home.touch_begin(&layout, x, y, at(start, 200));
        home.touch_move(&mut layout, x, y + 40.0, at(start, 220));
        let (dx, dy) = dock_rect(0, 4).center();
        home.touch_move(&mut layout, dx, dy, at(start, 300));
        // The dock is full: no place there, Exocortex keeps its own.
        run(&mut home, &mut layout, at(start, 300), 200);
        home.reset(&mut layout, at(start, 600));
        assert_eq!(layout.slot(AppId::Exocortex), Some(Slot::Grid(0)));
        assert!(!home.editing() && !home.live(at(start, 600)));
    }

    #[test]
    fn the_shell_routes_commands_taps_and_locks_here() {
        let mut state = ShellState::new(false, 50);
        assert_eq!(state.view, View::Launcher);
        // A command moves Browser into the grid; apps in neither list go.
        state
            .home_command(LayoutCommand::Set(
                vec![AppId::Phone],
                vec![AppId::Exocortex, AppId::Browser],
            ))
            .unwrap();
        assert!(state.settings_changed);
        assert_eq!(
            state.settings.home.removed,
            vec![AppId::System, AppId::Messages]
        );
        assert_eq!(state.home_icon_rect(AppId::Browser), grid_rect(1));
        assert!(
            state
                .home_command(LayoutCommand::Remove(AppId::System))
                .is_err()
        );
        // Home (the gesture, `ctl home`) leaves edit mode.
        state.home_command(LayoutCommand::Edit(true)).unwrap();
        assert!(state.home_public().editing);
        state.home();
        assert!(!state.home_public().editing);
        // Locking ends it at once.
        state.home_command(LayoutCommand::Edit(true)).unwrap();
        state.lock();
        assert!(state.home.editing.is_none());
        assert!(state.home_command(LayoutCommand::Edit(true)).is_err());
        // A tap opens an app from where its icon is now.
        state.view = View::Launcher;
        let (x, y) = grid_rect(1).center();
        state.home_tap(x, y);
        assert_eq!(state.view, View::Browser);
        assert_eq!(
            state.launch().map(|launch| launch.card.from),
            Some(grid_rect(1))
        );
        state.home_command(LayoutCommand::Reset).unwrap();
        assert_eq!(state.settings.home, HomeLayout::default());
    }

    #[test]
    fn layout_commands_parse() {
        let parse = |text: &str| LayoutCommand::parse(text.split_whitespace());
        assert_eq!(parse("reset"), Ok(LayoutCommand::Reset));
        assert_eq!(
            parse("set phone,system exocortex,browser"),
            Ok(LayoutCommand::Set(
                vec![AppId::Phone, AppId::System],
                vec![AppId::Exocortex, AppId::Browser]
            ))
        );
        assert_eq!(parse("set - -"), Ok(LayoutCommand::Set(vec![], vec![])));
        assert!(parse("set phone,phone -").is_err());
        assert!(parse("set phone,system,browser,messages,exocortex -").is_err());
        assert!(parse("set phone").is_err());
        assert_eq!(
            parse("remove Browser"),
            Ok(LayoutCommand::Remove(AppId::Browser))
        );
        assert!(parse("remove nothing").is_err());
        assert_eq!(parse("edit on"), Ok(LayoutCommand::Edit(true)));
        assert_eq!(parse("library off"), Ok(LayoutCommand::Library(false)));
        assert!(parse("edit").is_err());
        assert!(parse("reset now").is_err());
    }
}
