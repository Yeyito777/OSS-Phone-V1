//! Home screen icon badges: the count an app puts on its icon (`badge N`,
//! iOS's red badge: Exocortex's unread conversations), drawn on the home
//! grid, the dock and the App Library (ui.rs). A badge outlives its app's
//! process (frozen in the background or closed most of the time) and Moon's
//! restarts: Moon keeps the counts in a file beside its settings, written
//! atomically when one changes.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::gfx::{Canvas, RectF};
use crate::model::AppId;
use crate::text::{self, Weight};

/// The largest count an app may set.
pub const MAX: u32 = 99_999;
/// Larger counts read "999+".
const SHOWN_MAX: u32 = 999;

/// The badge at the home screen's icon size: iOS's red pill (a circle for
/// one digit) with white digits, its right end's center this far in from
/// the icon's top-right corner, so it overhangs the corner.
pub const HEIGHT: f32 = 64.0;
pub const INSET: f32 = 12.0;
const TEXT_SIZE: f32 = 44.0;
const TEXT_WEIGHT: Weight = Weight::Regular;
/// Room beside the digits of a longer count.
const PAD: f32 = 19.0;
const RED: crate::gfx::Rgb = crate::ui::RED;
/// A layer that holds the widest badge ("999+"), its right end at the
/// layer's right edge less `LAYER_MARGIN`, its middle at half its height.
pub const LAYER_W: i32 = 200;
pub const LAYER_H: i32 = HEIGHT as i32 + 2 * LAYER_MARGIN;
const LAYER_MARGIN: i32 = 2;

/// Every app's count (0: no badge).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Badges([u32; AppId::COUNT]);

impl Badges {
    /// `badges.json` beside the settings (`MOON_SETTINGS` moves both).
    pub fn path() -> PathBuf {
        crate::settings::Settings::path().with_file_name("badges.json")
    }

    /// The saved counts: `{"exocortex": 3}`; anything unreadable is none.
    pub fn load(path: &Path) -> Self {
        let saved: BTreeMap<String, u32> = fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let mut badges = Self::default();
        for (app, count) in saved {
            if let Some(app) = AppId::parse(&app) {
                badges.0[app.index()] = count.min(MAX);
            }
        }
        badges
    }

    /// Writes the file atomically (a temporary file, then rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.new");
        let mut file = fs::File::create(&temporary)?;
        file.write_all(serde_json::to_string(&self.public())?.as_bytes())?;
        file.write_all(b"\n")?;
        fs::rename(&temporary, path)
    }

    /// Returns whether the count changed.
    pub fn set(&mut self, app: AppId, count: u32) -> bool {
        let count = count.min(MAX);
        let changed = self.0[app.index()] != count;
        self.0[app.index()] = count;
        changed
    }

    pub fn get(&self, app: AppId) -> u32 {
        self.0[app.index()]
    }

    /// For `state` (and the file): the apps with a badge, by app.
    pub fn public(&self) -> BTreeMap<&'static str, u32> {
        AppId::ALL
            .into_iter()
            .filter(|app| self.get(*app) > 0)
            .map(|app| (app.cgroup_name(), self.get(app)))
            .collect()
    }
}

/// What a badge reads.
pub fn label(count: u32) -> String {
    if count > SHOWN_MAX {
        format!("{SHOWN_MAX}+")
    } else {
        count.to_string()
    }
}

/// A badge's width at the icon size: a circle, or a pill around more
/// digits (whole pixels, so every place draws it alike).
pub fn width(count: u32) -> f32 {
    (text::measure(&label(count), TEXT_WEIGHT, TEXT_SIZE) + 2.0 * PAD)
        .ceil()
        .max(HEIGHT)
}

/// Where an icon's badge goes: its right end's center, near the icon's
/// top-right corner (`rect` is the icon's square, at any scale), turned
/// with the icon by `angle` (radians, clockwise; the jiggle).
pub fn anchor(rect: RectF, angle: f32) -> (f32, f32) {
    let scale = rect.w / crate::ui::ICON_SIZE as f32;
    let (cx, cy) = rect.center();
    let d = (crate::ui::ICON_SIZE as f32 / 2.0 - INSET) * scale;
    let (sin, cos) = angle.sin_cos();
    (cx + d * cos + d * sin, cy + d * sin - d * cos)
}

/// Draws a badge of `count` at `scale` (1 at the icon size) with its right
/// end's center at `(x, y)`.
pub fn draw(canvas: &mut Canvas, count: u32, x: f32, y: f32, scale: f32) {
    let height = HEIGHT * scale;
    let width = width(count) * scale;
    let right = x + height / 2.0;
    let pill = RectF::new(right - width, y - height / 2.0, width, height);
    canvas.fill_rrect(pill, height / 2.0, RED, 1.0);
    let size = TEXT_SIZE * scale;
    text::draw_centered(
        canvas,
        &label(count),
        right - width / 2.0,
        text::centered_baseline(y, TEXT_WEIGHT, size),
        TEXT_WEIGHT,
        size,
        crate::ui::WHITE,
        1.0,
    );
}

/// A badge's layer (`LAYER_W` × `LAYER_H`) for the live home screen.
pub fn draw_layer(canvas: &mut Canvas, count: u32) {
    canvas.clear(0);
    if count > 0 {
        let (x, y) = layer_anchor();
        draw(canvas, count, x, y, 1.0);
    }
}

/// Where `draw_layer` puts the badge's anchor in its layer.
pub fn layer_anchor() -> (f32, f32) {
    (
        (LAYER_W - LAYER_MARGIN) as f32 - HEIGHT / 2.0,
        LAYER_H as f32 / 2.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_are_kept_bounded_and_saved() {
        let path = std::env::temp_dir().join(format!("moon-badges-{}.json", std::process::id()));
        let mut badges = Badges::default();
        assert!(badges.set(AppId::Exocortex, 3));
        assert!(!badges.set(AppId::Exocortex, 3), "unchanged");
        assert!(badges.set(AppId::Messages, MAX + 5));
        assert_eq!(badges.get(AppId::Messages), MAX);
        badges.save(&path).unwrap();
        assert_eq!(Badges::load(&path), badges);
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "{\"exocortex\":3,\"messages\":99999}\n"
        );
        badges.set(AppId::Messages, 0);
        assert_eq!(badges.public().len(), 1);
        // Unknown apps and junk are dropped, not fatal.
        fs::write(&path, r#"{"exocortex": 2, "nowhere": 4}"#).unwrap();
        assert_eq!(
            Badges::load(&path).public(),
            BTreeMap::from([("exocortex", 2)])
        );
        fs::write(&path, "not json").unwrap();
        assert_eq!(Badges::load(&path), Badges::default());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn badges_read_like_ios_and_grow_with_their_digits() {
        assert_eq!(label(7), "7");
        assert_eq!(label(999), "999");
        assert_eq!(label(1000), "999+");
        assert_eq!(width(7), HEIGHT, "one digit is a circle");
        assert!(width(42) > HEIGHT);
        assert!(width(MAX) > width(42));
        // The widest fits its layer.
        assert!(width(MAX) + 2.0 * LAYER_MARGIN as f32 <= LAYER_W as f32);
    }

    #[test]
    fn the_badge_overhangs_the_icons_top_right_corner() {
        let icon = RectF::new(100.0, 200.0, 168.0, 168.0);
        let (x, y) = anchor(icon, 0.0);
        assert_eq!((x, y), (256.0, 212.0));
        // Turned clockwise, the top-right corner moves right and down.
        let (tx, ty) = anchor(icon, 0.03);
        assert!(tx > x && ty > y && ty - y > tx - x);
        let mut canvas = Canvas::new(400, 400);
        draw(&mut canvas, 5, x, y, 1.0);
        let red = |px: i32, py: i32| canvas.pixels[(py * 400 + px) as usize] & 0xffffff == RED;
        // Beyond the icon's right and top edges, and over its corner.
        assert!(red(282, 212) && red(256, 184) && red(278, 198));
        // White digits in the middle of the circle somewhere.
        assert!(
            (230..282)
                .flat_map(|px| (190..234).map(move |py| (px, py)))
                .any(|(px, py)| canvas.pixels[(py * 400 + px) as usize] & 0xffffff == 0xffffff)
        );
    }
}
