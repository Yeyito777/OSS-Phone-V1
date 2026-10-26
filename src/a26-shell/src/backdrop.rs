//! Full-screen background dims that cost a fraction of a frame.
//!
//! Blending a dim over a 1080x2340 background every frame does not fit a
//! 120 Hz refresh on the A26's unaccelerated Xorg. Instead each background
//! has a few pre-darkened copies (`DimLevels`, rendered once server-side),
//! and a fade between two neighbouring levels is an ordered dither: 1-pixel
//! rows interleave the two levels, and each step of the fade switches one
//! row in every sixteen (`compositor::Node::Stripes`). A step therefore
//! redraws a sixteenth of the screen, spread evenly over it, and the eye
//! sees a uniform in-between brightness rather than edges.
//!
//! A whole fade costs one full-screen redraw per level it crosses, so the
//! levels are few and far apart (rows that far apart in brightness are
//! still too fine to see at the panel's ~400 ppi) and every resting dim is
//! a level of its own, never a dither.

use std::error::Error;

use x11rb::connection::Connection;

use crate::compositor::{Compositor, Node, STRIPE_PHASES, SurfaceId};

/// Stripe steps per frame (a sixteenth of the screen each; `ctl profile
/// steps N`), doubled while more than a level behind so a fast gesture's
/// background trails it by only a few frames.
pub static FADE_STEPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

fn steps_per_frame(behind: u32) -> u32 {
    let steps = FADE_STEPS.load(std::sync::atomic::Ordering::Relaxed);
    if behind > u32::from(STRIPE_PHASES) {
        steps * 2
    } else {
        steps
    }
}

/// Pre-darkened copies of a surface: level `k` (from 1) is darkened by
/// `dims[k - 1]`; level 0 is the surface itself.
pub struct DimLevels {
    source: SurfaceId,
    dims: &'static [f32],
    levels: Vec<SurfaceId>,
    stale: bool,
}

impl DimLevels {
    pub fn new(source: SurfaceId, dims: &'static [f32]) -> Self {
        Self {
            source,
            dims,
            levels: Vec::new(),
            stale: true,
        }
    }

    pub fn max_level(&self) -> u32 {
        self.dims.len() as u32
    }

    /// The (fractional) level showing `dim`: linear between levels.
    pub fn level_of(&self, dim: f32) -> f32 {
        level_of(self.dims, dim)
    }

    /// The source changed; the copies are redrawn before their next use.
    pub fn invalidate(&mut self) {
        self.stale = true;
    }

    pub fn stale(&self) -> bool {
        self.stale
    }

    /// Creates or redraws every level (about one full-screen composite each).
    pub fn ensure<C: Connection>(
        &mut self,
        conn: &C,
        compositor: &mut Compositor,
    ) -> Result<(), Box<dyn Error>> {
        if !self.stale {
            return Ok(());
        }
        for (index, dim) in self.dims.iter().copied().enumerate() {
            match self.levels.get(index) {
                Some(id) => compositor.refresh_dimmed(conn, *id, self.source, dim)?,
                None => self
                    .levels
                    .push(compositor.create_dimmed(conn, self.source, dim)?),
            }
        }
        self.stale = false;
        Ok(())
    }

    /// The surface for `level` (0 is the undimmed source).
    pub fn surface(&self, level: u32) -> SurfaceId {
        match level {
            0 => self.source,
            level => self
                .levels
                .get((level as usize - 1).min(self.levels.len().saturating_sub(1)))
                .copied()
                .unwrap_or(self.source),
        }
    }

    /// The full-screen node for a position in stripe steps.
    pub fn node(&self, steps: u32) -> Node {
        let per_level = u32::from(STRIPE_PHASES);
        let level = steps / per_level;
        Node::Stripes {
            low: self.surface(level),
            high: self.surface(level + 1),
            phases: (steps % per_level) as u8,
        }
    }
}

fn level_of(dims: &[f32], dim: f32) -> f32 {
    let mut below = 0.0;
    for (index, above) in dims.iter().copied().enumerate() {
        if dim <= above {
            return index as f32 + ((dim - below) / (above - below)).max(0.0);
        }
        below = above;
    }
    dims.len() as f32
}

/// A background's position in sixteenths of a dim level, stepping toward a
/// (fractional) target level a stripe or two per frame. A target that holds
/// still (a finger resting mid-swipe) settles on the nearest whole level, so
/// the dither never stays on screen.
#[derive(Debug, Clone, Default)]
pub struct StripeFade {
    steps: u32,
    goal: u32,
    /// Updates the goal has stayed the same for.
    still: u32,
}

/// Updates (~250 ms) after which an unchanging goal rounds to a level; long
/// enough that a slow drag keeps following the finger.
const REST_FRAMES: u32 = 30;

fn goal_steps(target: f32, max: u32) -> u32 {
    (target.clamp(0.0, max as f32) * f32::from(STRIPE_PHASES)).round() as u32
}

impl StripeFade {
    /// Advances toward `target` (in levels, 0..=max) and returns the
    /// current position in stripe steps.
    pub fn update(&mut self, target: f32, max: u32) -> u32 {
        // A step of jitter (a resting finger is never perfectly still)
        // does not count as moving.
        let goal = goal_steps(target, max);
        if goal.abs_diff(self.goal) <= 1 && goal % u32::from(STRIPE_PHASES) != 0 {
            self.still = self.still.saturating_add(1);
        } else {
            self.goal = goal;
            self.still = 0;
        }
        let aim = self.aim();
        let behind = aim.abs_diff(self.steps);
        let step = behind.min(steps_per_frame(behind));
        if aim > self.steps {
            self.steps += step;
        } else {
            self.steps -= step;
        }
        self.steps
    }

    fn aim(&self) -> u32 {
        let per_level = u32::from(STRIPE_PHASES);
        if self.still >= REST_FRAMES {
            (self.goal + per_level / 2) / per_level * per_level
        } else {
            self.goal
        }
    }

    /// True while frames are still needed to reach the goal (or, for a
    /// goal between levels, to notice that it holds still).
    pub fn settling(&self) -> bool {
        self.steps != self.aim()
            || (self.goal % u32::from(STRIPE_PHASES) != 0 && self.still < REST_FRAMES)
    }

    /// Jumps straight to a level (entering a screen, not a transition).
    pub fn snap(&mut self, target: f32, max: u32) {
        self.goal = goal_steps(target, max);
        self.steps = self.goal;
        self.still = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fades_step_a_bounded_number_of_stripes_per_frame() {
        let mut fade = StripeFade::default();
        // Two levels behind: two stripes a frame until within a level.
        assert_eq!(fade.update(2.0, 2), 2);
        let mut frames = 1;
        while fade.settling() {
            fade.update(2.0, 2);
            frames += 1;
        }
        assert_eq!(fade.update(2.0, 2), 2 * u32::from(STRIPE_PHASES));
        assert_eq!(frames, 8 + 16);
        // Back down a stripe at a time, and clamped to the range.
        fade.snap(0.5, 2);
        assert_eq!(fade.update(-3.0, 2), 7);
    }

    #[test]
    fn a_resting_target_settles_on_a_whole_level() {
        let mut fade = StripeFade::default();
        // A finger held at 1.3 levels: the dither reaches it, then the
        // background moves on to level 1 and stops.
        let mut frames = 0;
        while frames == 0 || fade.settling() {
            fade.update(1.3, 2);
            frames += 1;
            assert!(frames < 100);
        }
        assert_eq!(fade.update(1.3, 2), u32::from(STRIPE_PHASES));
        // A step of jitter keeps it there; moving on follows the finger.
        assert_eq!(fade.update(1.36, 2), u32::from(STRIPE_PHASES));
        assert_eq!(fade.update(1.5, 2), u32::from(STRIPE_PHASES) + 1);
    }

    #[test]
    fn resting_dims_are_whole_levels() {
        let dims = [0.21, 0.42, 0.55];
        assert_eq!(level_of(&dims, 0.0), 0.0);
        assert_eq!(level_of(&dims, 0.42), 2.0);
        assert_eq!(level_of(&dims, 0.55), 3.0);
        assert!((level_of(&dims, 0.105) - 0.5).abs() < 1e-6);
        assert!((level_of(&dims, 0.485) - 2.5).abs() < 1e-5);
        assert_eq!(level_of(&dims, 0.9), 3.0);
    }
}
