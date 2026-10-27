//! The panel's light over the ambient light sensor, and how much of a
//! reading is ambient (docs/auto-brightness-compensation.md §2, §3, §5).
//!
//! Pure. The hub's lux is ambient + leak, with leak = k · (bl / max)^γ. The
//! leak's dependence on screen content (`α · Y_roi + β · Y_screen` in the
//! spec) is folded into `k` for now: `k` is learned from what Moon usually
//! shows, and the uncertainty margin covers the rest.
//!
//! Calibration needs no user action: on every wake the backlight is held
//! dark until the first reading, which is therefore ambient (`A0`); the lit
//! readings of the next 30 s give leak samples `lux − A0` at known backlight
//! levels. At screen-off, with the panel dark again, a second ambient
//! reading (`A1`) closes the window: the wake counts only if A1 is within
//! 15 % of A0 (the light stayed put, judged without the model), and then `k`
//! is updated by least squares with exponential forgetting.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const GAMMA: f32 = 1.5;
/// Seed: the lock screen at full backlight adds about 110 lx (measured).
pub const SEED_K: f32 = 110.0;
/// Per accepted wake.
const FORGETTING: f32 = 0.98;
/// Leak samples come from the first 30 s after a wake.
pub const WINDOW_NS: u64 = 30_000_000_000;
/// A screen-off reading later than this after the wake closes nothing: the
/// light had too long to change and come back.
pub const CLOSE_WITHIN_NS: u64 = 120_000_000_000;
/// The light did not change if A1 is within this fraction of A0 (plus the
/// absolute floor).
const STEADY: f32 = 0.15;
const STEADY_FLOOR_LUX: f32 = 3.0;
/// Below this g(bl) a residual says nothing about k (the leak is tiny).
const MIN_G: f32 = 0.05;
/// Dark rooms: below this ambient, with the leak over half the reading, lit
/// readings are not followed (spec §5).
pub const DARK_LUX: f32 = 20.0;

/// g(bl) = (bl / max)^γ.
pub fn g(backlight: u16, max: u16) -> f32 {
    (f32::from(backlight) / f32::from(max.max(1)))
        .clamp(0.0, 1.0)
        .powf(GAMMA)
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Calibration {
    pub k: f32,
    /// Weighted sums of g² and g·residual (forgetting applied per wake).
    pub sxx: f32,
    pub sxy: f32,
    pub accepted: u32,
    pub rejected: u32,
}

impl Default for Calibration {
    fn default() -> Self {
        // The seed counts as a weak prior: one sample at full backlight.
        Self {
            k: SEED_K,
            sxx: 1.0,
            sxy: SEED_K,
            accepted: 0,
            rejected: 0,
        }
    }
}

impl Calibration {
    pub fn load(path: &Path) -> Option<Self> {
        let calibration: Self = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        (calibration.k.is_finite() && calibration.k >= 0.0).then_some(calibration)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("new");
        fs::write(
            &temporary,
            serde_json::to_vec(self).map_err(io::Error::other)?,
        )?;
        fs::rename(temporary, path)
    }

    pub fn leak(&self, backlight: u16, max: u16) -> f32 {
        self.k * g(backlight, max)
    }

    /// Ambient estimate for a lit reading.
    pub fn estimate(&self, lux: u32, backlight: u16, max: u16) -> Estimate {
        let lux = lux as f32;
        let leak = self.leak(backlight, max);
        Estimate {
            lux,
            leak,
            ambient: (lux - leak).max(0.0),
            margin: 0.3 * leak + 5.0,
        }
    }

    /// One wake's samples: (g, residual) pairs.
    fn update(&mut self, samples: &[(f32, f32)]) {
        self.sxx *= FORGETTING;
        self.sxy *= FORGETTING;
        for &(g, residual) in samples {
            self.sxx += g * g;
            self.sxy += g * residual;
        }
        if self.sxx > 0.0 {
            self.k = (self.sxy / self.sxx).max(0.0);
        }
        self.accepted += 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Estimate {
    pub lux: f32,
    pub leak: f32,
    pub ambient: f32,
    /// u = 0.3 · leak + 5 lx.
    pub margin: f32,
}

impl Estimate {
    /// The dark-room rule: too little light to tell from the leak.
    pub fn untrusted(&self) -> bool {
        self.ambient < DARK_LUX && self.leak > 0.5 * self.lux
    }

    pub fn low(&self) -> f32 {
        (self.ambient - self.margin).max(0.0)
    }

    pub fn high(&self) -> f32 {
        self.ambient + self.margin
    }
}

/// The leak samples of one wake, until the window closes.
#[derive(Debug, Clone, PartialEq)]
pub struct WakeWindow {
    pub a0: f32,
    started_ns: u64,
    samples: Vec<(f32, f32)>,
}

impl WakeWindow {
    pub fn new(a0: u32, started_ns: u64) -> Self {
        Self {
            a0: a0 as f32,
            started_ns,
            samples: Vec::new(),
        }
    }

    /// A lit reading (measured at `at_ns`, with a known, steady backlight).
    pub fn sample(&mut self, lux: u32, at_ns: u64, backlight: u16, max: u16) {
        let g = g(backlight, max);
        if at_ns < self.started_ns + WINDOW_NS && g >= MIN_G {
            self.samples.push((g, lux as f32 - self.a0));
        }
    }

    /// Worth closing with a screen-off reading at `now_ns`.
    pub fn closable(&self, now_ns: u64) -> bool {
        !self.samples.is_empty() && now_ns < self.started_ns + CLOSE_WITHIN_NS
    }

    /// Closes the window with the screen-off ambient `a1`: updates
    /// `calibration` when the light stayed put. Returns whether it did.
    pub fn close(self, a1: u32, calibration: &mut Calibration) -> bool {
        if self.samples.is_empty() {
            return false;
        }
        let steady = (a1 as f32 - self.a0).abs() <= STEADY * self.a0 + STEADY_FLOOR_LUX;
        if steady {
            calibration.update(&self.samples);
        } else {
            calibration.rejected += 1;
        }
        steady
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leak_follows_the_measured_points() {
        let c = Calibration::default();
        // 10 lx at 130 and 110 lx at 665 (the spec's measurements).
        assert!((c.leak(130, 665) - 9.5).abs() < 1.0);
        assert!((c.leak(665, 665) - 110.0).abs() < 0.01);
        assert!(c.leak(1, 665) < 0.01);
        let e = c.estimate(200, 665, 665);
        assert!((e.ambient - 90.0).abs() < 0.01);
        assert!((e.margin - 38.0).abs() < 0.01);
        assert!(!e.untrusted());
        // A dark room at full backlight: mostly leak.
        assert!(c.estimate(115, 665, 665).untrusted());
    }

    #[test]
    fn steady_wakes_teach_k_and_changing_light_is_ignored() {
        let mut c = Calibration::default();
        // True k = 200: a room at 50 lx, readings at backlight 400 and 665.
        const S: u64 = 1_000_000_000;
        for _ in 0..40 {
            let mut w = WakeWindow::new(50, 0);
            for (at, bl) in [(6 * S, 400u16), (12 * S, 665)] {
                let lux = 50.0 + 200.0 * g(bl, 665);
                w.sample(lux.round() as u32, at, bl, 665);
            }
            assert!(w.closable(60 * S));
            assert!(w.close(52, &mut c));
        }
        assert!((c.k - 200.0).abs() < 8.0, "k = {}", c.k);
        let k = c.k;
        // The lights went on (A1 far from A0): rejected, k unchanged.
        let mut w = WakeWindow::new(50, 0);
        w.sample(900, 6 * S, 665, 665);
        assert!(!w.close(400, &mut c));
        assert_eq!(c.k, k);
        assert_eq!(c.rejected, 1);
        // Too dim a backlight, or after the 30 s window: no samples.
        let mut w = WakeWindow::new(50, 0);
        w.sample(51, 6 * S, 60, 665);
        w.sample(250, 40 * S, 665, 665);
        assert!(!w.closable(60 * S));
        assert!(!w.close(50, &mut c));
        assert_eq!(c.rejected, 1);
        // Too long a session to vouch for the light in between.
        let mut w = WakeWindow::new(50, 0);
        w.sample(250, 6 * S, 665, 665);
        assert!(!w.closable(CLOSE_WITHIN_NS));
    }
}
