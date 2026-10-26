//! Full-screen background dims that cost a fraction of a frame.
//!
//! Blending a dim over a 1080x2340 background every frame does not fit a
//! 120 Hz refresh on the A26's unaccelerated Xorg. Instead each background
//! has pre-darkened copies (`DimLevels`, rendered once server-side, one per
//! `DIM_STEP`), and a fade between two neighbouring levels is an ordered
//! dither: fine 2-pixel rows interleave the two levels, and each step of the
//! fade switches one row in every eight (`compositor::Node::Stripes`). A
//! frame therefore redraws an eighth of the screen, spread evenly over it,
//! and the eye sees a uniform in-between brightness rather than edges.

use std::error::Error;

use x11rb::connection::Connection;

use crate::compositor::{Compositor, Node, STRIPE_PHASES, SurfaceId};

/// Brightness removed per level.
pub const DIM_STEP: f32 = 0.06;
/// Stripe steps allowed per frame (2 by default, a quarter of the screen;
/// `ctl profile steps N`); a faster fade trails by a frame or two.
pub static FADE_STEPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(2);

fn max_steps_per_frame() -> u32 {
    FADE_STEPS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Pre-darkened copies of a surface: level `k` is darkened by `k * DIM_STEP`.
pub struct DimLevels {
    source: SurfaceId,
    count: usize,
    levels: Vec<SurfaceId>,
    stale: bool,
}

impl DimLevels {
    pub fn new(source: SurfaceId, count: usize) -> Self {
        Self {
            source,
            count,
            levels: Vec::new(),
            stale: true,
        }
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
        for level in 1..=self.count {
            let dim = level as f32 * DIM_STEP;
            match self.levels.get(level - 1) {
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

    /// The full-screen node for a position in eighths of a level.
    pub fn node(&self, eighths: u32) -> Node {
        let per_level = u32::from(STRIPE_PHASES);
        let level = eighths / per_level;
        Node::Stripes {
            low: self.surface(level),
            high: self.surface(level + 1),
            phases: (eighths % per_level) as u8,
        }
    }
}

/// A background's position in eighths of a dim level, stepping toward a
/// (fractional) target level a few stripes per frame.
#[derive(Debug, Clone, Default)]
pub struct StripeFade {
    eighths: u32,
}

impl StripeFade {
    /// Advances toward `target` (in levels, 0..=max) and returns the
    /// current position in eighths of a level.
    pub fn update(&mut self, target: f32, max: u32) -> u32 {
        let goal = (target.clamp(0.0, max as f32) * f32::from(STRIPE_PHASES)).round() as u32;
        if goal > self.eighths {
            self.eighths += (goal - self.eighths).min(max_steps_per_frame());
        } else {
            self.eighths -= (self.eighths - goal).min(max_steps_per_frame());
        }
        self.eighths
    }

    /// True until the last update reached its goal.
    pub fn settling(&self, target: f32, max: u32) -> bool {
        let goal = (target.clamp(0.0, max as f32) * f32::from(STRIPE_PHASES)).round() as u32;
        goal != self.eighths
    }

    /// Jumps straight to a level (entering a screen, not a transition).
    pub fn snap(&mut self, target: f32, max: u32) {
        self.eighths = (target.clamp(0.0, max as f32) * f32::from(STRIPE_PHASES)).round() as u32;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fades_step_a_bounded_number_of_stripes_per_frame() {
        let mut fade = StripeFade::default();
        assert_eq!(fade.update(7.0, 7), max_steps_per_frame());
        let mut frames = 1;
        while fade.settling(7.0, 7) {
            fade.update(7.0, 7);
            frames += 1;
        }
        assert_eq!(fade.update(7.0, 7), 7 * u32::from(STRIPE_PHASES));
        assert_eq!(frames, 28);
        // Back down, and clamped to the range.
        fade.snap(0.5, 7);
        assert_eq!(fade.update(-3.0, 7), 2);
    }
}
