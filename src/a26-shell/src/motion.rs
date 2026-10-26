//! Motion primitives for interactive gestures: an analytic damped spring
//! (iOS-style "response" and "damping fraction" parameters) and a finger
//! velocity estimator.

use std::time::Instant;

/// A one-dimensional damped spring settling from `from` (with an initial
/// velocity) to `to`. It is evaluated analytically, so frames may sample it
/// at any time without accumulating integration error.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spring {
    pub from: f32,
    pub to: f32,
    /// Units per second at `started`.
    pub velocity: f32,
    pub started: Instant,
    omega: f32,
    damping: f32,
}

impl Spring {
    /// `response` is the undamped period in seconds; `damping` is the damping
    /// fraction (1.0 = critically damped, below 1 overshoots).
    pub fn new(
        from: f32,
        to: f32,
        velocity: f32,
        started: Instant,
        response: f32,
        damping: f32,
    ) -> Self {
        Self {
            from,
            to,
            velocity,
            started,
            omega: std::f32::consts::TAU / response.max(0.01),
            damping: damping.clamp(0.05, 2.0),
        }
    }

    fn elapsed(&self, now: Instant) -> f32 {
        now.saturating_duration_since(self.started).as_secs_f32()
    }

    /// Displacement from the target and its velocity at time `t` seconds.
    fn state(&self, t: f32) -> (f32, f32) {
        let x0 = self.from - self.to;
        let v0 = self.velocity;
        let w = self.omega;
        let z = self.damping;
        if z < 1.0 {
            let wd = w * (1.0 - z * z).sqrt();
            let decay = (-z * w * t).exp();
            let a = x0;
            let b = (v0 + z * w * x0) / wd;
            let (sin, cos) = (wd * t).sin_cos();
            let x = decay * (a * cos + b * sin);
            let v = decay * ((b * wd - z * w * a) * cos - (a * wd + z * w * b) * sin);
            (x, v)
        } else {
            // Critically (or over-) damped: treated as critical.
            let decay = (-w * t).exp();
            let b = v0 + w * x0;
            let x = (x0 + b * t) * decay;
            let v = (b - w * (x0 + b * t)) * decay;
            (x, v)
        }
    }

    pub fn value(&self, now: Instant) -> f32 {
        self.to + self.state(self.elapsed(now)).0
    }

    pub fn velocity_at(&self, now: Instant) -> f32 {
        self.state(self.elapsed(now)).1
    }

    /// Settled once within `epsilon` of the target and nearly still.
    pub fn settled(&self, now: Instant, epsilon: f32) -> bool {
        let (x, v) = self.state(self.elapsed(now));
        x.abs() < epsilon && v.abs() < epsilon * 10.0
    }
}

/// Estimates finger velocity (pixels per second) from recent samples with a
/// least-squares fit over the last 80 ms, ignoring older motion.
#[derive(Debug, Clone, Default)]
pub struct VelocityTracker {
    samples: Vec<(Instant, f32, f32)>,
}

const VELOCITY_WINDOW_SECONDS: f32 = 0.08;
const MAX_SAMPLES: usize = 16;

impl VelocityTracker {
    pub fn push(&mut self, at: Instant, x: f32, y: f32) {
        if self.samples.len() == MAX_SAMPLES {
            self.samples.remove(0);
        }
        self.samples.push((at, x, y));
    }

    pub fn velocity(&self, now: Instant) -> (f32, f32) {
        let recent: Vec<(f32, f32, f32)> = self
            .samples
            .iter()
            .filter_map(|(at, x, y)| {
                let age = now.saturating_duration_since(*at).as_secs_f32();
                (age <= VELOCITY_WINDOW_SECONDS).then_some((-age, *x, *y))
            })
            .collect();
        if recent.len() < 2 {
            return (0.0, 0.0);
        }
        let n = recent.len() as f32;
        let mean_t = recent.iter().map(|s| s.0).sum::<f32>() / n;
        let mean_x = recent.iter().map(|s| s.1).sum::<f32>() / n;
        let mean_y = recent.iter().map(|s| s.2).sum::<f32>() / n;
        let var_t = recent.iter().map(|s| (s.0 - mean_t).powi(2)).sum::<f32>();
        if var_t < 1e-6 {
            return (0.0, 0.0);
        }
        let cov_x = recent
            .iter()
            .map(|s| (s.0 - mean_t) * (s.1 - mean_x))
            .sum::<f32>();
        let cov_y = recent
            .iter()
            .map(|s| (s.0 - mean_t) * (s.2 - mean_y))
            .sum::<f32>();
        (cov_x / var_t, cov_y / var_t)
    }
}

/// iOS-like rubber banding: past `limit`, motion is increasingly resisted.
pub fn rubber_band(offset: f32, limit: f32) -> f32 {
    if offset.abs() <= limit {
        return offset;
    }
    let excess = offset.abs() - limit;
    let resisted = limit + excess * 0.55 / (1.0 + excess / 900.0);
    resisted.copysign(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn spring_starts_at_from_and_settles_at_to() {
        let start = Instant::now();
        for damping in [0.7, 1.0] {
            let spring = Spring::new(0.0, 1.0, 0.0, start, 0.35, damping);
            assert!(spring.value(start).abs() < 1e-5);
            let late = start + Duration::from_secs(2);
            assert!((spring.value(late) - 1.0).abs() < 1e-3);
            assert!(spring.settled(late, 0.001));
            assert!(!spring.settled(start + Duration::from_millis(50), 0.001));
        }
    }

    #[test]
    fn underdamped_spring_overshoots_and_initial_velocity_is_honored() {
        let start = Instant::now();
        let spring = Spring::new(0.0, 1.0, 0.0, start, 0.35, 0.6);
        let peak = (1..100)
            .map(|ms| spring.value(start + Duration::from_millis(ms * 5)))
            .fold(0.0_f32, f32::max);
        assert!(peak > 1.02);
        let thrown = Spring::new(0.0, 0.0, 10.0, start, 0.35, 1.0);
        assert!((thrown.velocity_at(start) - 10.0).abs() < 1e-3);
        assert!(thrown.value(start + Duration::from_millis(30)) > 0.0);
    }

    #[test]
    fn velocity_tracker_fits_recent_motion() {
        let start = Instant::now();
        let mut tracker = VelocityTracker::default();
        for step in 0..8 {
            let at = start + Duration::from_millis(step * 8);
            tracker.push(at, 100.0, 2000.0 - step as f32 * 16.0);
        }
        let (vx, vy) = tracker.velocity(start + Duration::from_millis(56));
        assert!(vx.abs() < 1.0);
        assert!((vy + 2000.0).abs() < 50.0, "vy {vy}");
        // Stale samples do not count.
        assert_eq!(tracker.velocity(start + Duration::from_secs(1)), (0.0, 0.0));
    }

    #[test]
    fn rubber_band_resists_past_the_limit() {
        assert_eq!(rubber_band(50.0, 100.0), 50.0);
        assert!(rubber_band(500.0, 100.0) < 500.0);
        assert!(rubber_band(-500.0, 100.0) > -500.0);
    }
}
