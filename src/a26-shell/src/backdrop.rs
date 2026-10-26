//! Full-screen background dims that cost a fraction of a frame.
//!
//! Blending a dim over a 1080x2340 background every frame does not fit a
//! 120 Hz refresh on the A26's unaccelerated Xorg. Instead each background
//! has pre-darkened copies (`DimLevels`, rendered server-side while idle)
//! a few percent apart, and every screen row shows one of them
//! (`compositor::Node::Rows`). A fade moves rows from copy to copy, a few
//! hundred rows a frame spread evenly over the screen, so the eye sees one
//! uniform in-between brightness.
//!
//! Neighbouring rows never differ by more than one level (under 4%, below
//! what the eye resolves as grain at the panel's ~400 ppi). Within that
//! rule a row can still jump two levels when both its neighbours sit one
//! above it, so a fade redraws the screen once per two levels it crosses.
//! A background that comes to rest shows a single copy, never a mix.

use std::error::Error;
use std::sync::atomic::{AtomicU32, Ordering};

use x11rb::connection::Connection;

use crate::compositor::{Compositor, SurfaceId};
use crate::present::Damage;

/// Largest brightness step between neighbouring levels (relative): with
/// 8-bit copies, rows then differ by at most about 4%.
pub const LEVEL_STEP: f32 = 0.035;

/// Pixels a frame may redraw in all (`ctl profile budget KPX`): a fade
/// takes what the rest of the frame leaves of it. About 0.75 Mpx fits one
/// 120 Hz refresh on the A26.
pub static FRAME_BUDGET_KPX: AtomicU32 = AtomicU32::new(520);
/// Rows a fade always gets per frame (a sixteenth of the screen), and at
/// most.
const MIN_ROWS: usize = 146;
const MAX_ROWS: usize = 560;

/// Rows a fade may change this frame, given the rest of the last frame's
/// redrawn pixels.
pub fn row_budget(foreground_area: i64, width: u16) -> usize {
    let budget = i64::from(FRAME_BUDGET_KPX.load(Ordering::Relaxed)) * 1000;
    (((budget - foreground_area).max(0) / i64::from(width.max(1))) as usize)
        .clamp(MIN_ROWS, MAX_ROWS)
}

/// Dims from 0 through each of `stops` (ascending), with neighbouring
/// levels at most `step` apart in relative brightness; every stop is a
/// level of its own.
pub fn level_dims(stops: &[f32], step: f32) -> Vec<f32> {
    let mut dims = Vec::new();
    let mut factor = 1.0_f32;
    for &stop in stops {
        let goal = 1.0 - stop;
        let count = ((goal / factor).ln() / (1.0 - step).ln()).ceil().max(1.0) as u32;
        for index in 1..=count {
            let level = factor * (goal / factor).powf(index as f32 / count as f32);
            dims.push(if index == count { stop } else { 1.0 - level });
        }
        factor = goal;
    }
    dims
}

/// Regions of a copy kept separately before they merge into their
/// bounding box.
const MAX_DIRTY_REGIONS: usize = 8;

/// Pre-darkened copies of a surface: level `k` (from 1) is darkened by
/// `dims[k - 1]`; level 0 is the surface itself. Copies are created once
/// (a few at a time while idle after startup, or when a fade first needs
/// one). When the source changes, only the changed regions are marked, and
/// a copy redraws them when a fade next shows it: an idle phone never
/// redraws copies, and the minute ticking on the home screen costs a few
/// small rectangles per copy at the next transition.
pub struct DimLevels {
    source: SurfaceId,
    dims: Vec<f32>,
    levels: Vec<Option<SurfaceId>>,
    /// Per copy, the regions that no longer match the source.
    dirty: Vec<Vec<Damage>>,
    /// Copies whose whole source changed (a new wallpaper): redrawn in idle
    /// time like missing ones.
    whole: Vec<bool>,
}

impl DimLevels {
    pub fn new(source: SurfaceId, dims: Vec<f32>) -> Self {
        Self {
            source,
            levels: vec![None; dims.len()],
            dirty: vec![Vec::new(); dims.len()],
            whole: vec![false; dims.len()],
            dims,
        }
    }

    pub fn max_level(&self) -> u8 {
        self.dims.len() as u8
    }

    /// The (fractional) level showing `dim`: linear between levels.
    pub fn level_of(&self, dim: f32) -> f32 {
        level_of(&self.dims, dim)
    }

    /// `regions` of the source changed.
    pub fn invalidate(&mut self, regions: &[Damage]) {
        for (level, dirty) in self.levels.iter().zip(&mut self.dirty) {
            if level.is_some() {
                add_regions(dirty, regions);
            }
        }
    }

    /// The whole source changed.
    pub fn invalidate_all(&mut self) {
        for (level, whole) in self.levels.iter().zip(&mut self.whole) {
            *whole = level.is_some();
        }
    }

    /// Copies not created yet, or wholly out of date.
    pub fn missing(&self) -> bool {
        self.levels.contains(&None) || self.whole.contains(&true)
    }

    /// Creates up to `count` missing copies (two full-screen composites
    /// each).
    pub fn create_some<C: Connection>(
        &mut self,
        conn: &C,
        compositor: &mut Compositor,
        count: usize,
    ) -> Result<(), Box<dyn Error>> {
        let missing: Vec<u8> = (1..=self.max_level())
            .filter(|level| {
                let index = usize::from(*level) - 1;
                self.levels[index].is_none() || self.whole[index]
            })
            .take(count)
            .collect();
        for level in missing {
            self.bring_up_to_date(conn, compositor, level)?;
        }
        Ok(())
    }

    /// Makes sure the copies for `levels` exist and match the source.
    pub fn ensure<C: Connection>(
        &mut self,
        conn: &C,
        compositor: &mut Compositor,
        levels: std::ops::RangeInclusive<u8>,
    ) -> Result<(), Box<dyn Error>> {
        for level in levels {
            if level > 0 && level <= self.max_level() {
                self.bring_up_to_date(conn, compositor, level)?;
            }
        }
        Ok(())
    }

    fn bring_up_to_date<C: Connection>(
        &mut self,
        conn: &C,
        compositor: &mut Compositor,
        level: u8,
    ) -> Result<(), Box<dyn Error>> {
        let index = usize::from(level) - 1;
        let dim = self.dims[index];
        match self.levels[index] {
            None => self.levels[index] = Some(compositor.create_dimmed(conn, self.source, dim)?),
            Some(id) if self.whole[index] => {
                compositor.refresh_dimmed(conn, id, self.source, dim)?
            }
            Some(id) if !self.dirty[index].is_empty() => {
                compositor.refresh_dimmed_regions(conn, id, self.source, dim, &self.dirty[index])?
            }
            Some(_) => {}
        }
        self.dirty[index].clear();
        self.whole[index] = false;
        Ok(())
    }

    /// The surface for `level` (0 is the undimmed source).
    pub fn surface(&self, level: u8) -> SurfaceId {
        match level {
            0 => self.source,
            level => self
                .levels
                .get(usize::from(level) - 1)
                .copied()
                .flatten()
                .unwrap_or(self.source),
        }
    }
}

/// Adds `regions` to a dirty list, merging overlapping ones and falling
/// back to one bounding box when the list grows long.
fn add_regions(dirty: &mut Vec<Damage>, regions: &[Damage]) {
    for region in regions {
        let mut merged = *region;
        dirty.retain(|existing| {
            let overlaps = existing.x0 <= merged.x1
                && merged.x0 <= existing.x1
                && existing.y0 <= merged.y1
                && merged.y0 <= existing.y1;
            if overlaps {
                merged = merged.union(*existing);
            }
            !overlaps
        });
        dirty.push(merged);
    }
    if dirty.len() > MAX_DIRTY_REGIONS
        && let Some(bounds) = dirty.iter().copied().reduce(Damage::union)
    {
        *dirty = vec![bounds];
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

/// Target resolution: sixteenths of a level.
const GOAL_SCALE: u32 = 16;
/// Updates (~250 ms) after which an unchanging goal rounds to a level; long
/// enough that a slow drag keeps following the finger.
const REST_FRAMES: u32 = 30;

/// Which level each screen row of a background shows, moved toward a
/// (fractional) target level a bounded number of rows per frame.
#[derive(Debug, Clone)]
pub struct RowFade {
    levels: Vec<u8>,
    /// Rows in the order they change: even rows, then odd ones, each in a
    /// low-discrepancy order so any prefix is spread evenly over the
    /// screen.
    order: Vec<u16>,
    goal: u32,
    /// Updates the goal has stayed the same for.
    still: u32,
    settled: bool,
}

impl RowFade {
    pub fn new(rows: usize) -> Self {
        // Golden-ratio (Kronecker) sequence: every prefix is spread evenly
        // and, unlike a bit-reversed order, never forms a regular grating.
        let spread = |offset: f64| {
            move |index: usize| (index as f64 * 0.618_033_988_749_895 + offset).fract()
        };
        let mut order = Vec::with_capacity(rows);
        for (parity, offset) in [(0, 0.0), (1, 0.5)] {
            let key = spread(offset);
            let mut class: Vec<u16> = (parity..rows).step_by(2).map(|row| row as u16).collect();
            class.sort_by(|a, b| key(usize::from(*a) / 2).total_cmp(&key(usize::from(*b) / 2)));
            order.extend(class);
        }
        Self {
            levels: vec![0; rows],
            order,
            goal: 0,
            still: 0,
            settled: true,
        }
    }

    pub fn levels(&self) -> &[u8] {
        &self.levels
    }

    /// The lowest and highest level on screen.
    pub fn span(&self) -> std::ops::RangeInclusive<u8> {
        let low = self.levels.iter().copied().min().unwrap_or(0);
        let high = self.levels.iter().copied().max().unwrap_or(0);
        low..=high
    }

    /// The mean level on screen.
    pub fn mean(&self) -> f32 {
        let sum: u64 = self.levels.iter().map(|row| u64::from(*row)).sum();
        sum as f32 / self.levels.len().max(1) as f32
    }

    /// Moves at most `budget` rows toward `target` (in levels, 0..=max),
    /// keeping neighbouring rows within `gap` levels of each other (1 for a
    /// smooth fade; 2 halves the rows needed, to catch up).
    pub fn update(&mut self, target: f32, max: u8, budget: usize, gap: u8) {
        let gap = gap.max(1);
        let goal = goal_units(target, max);
        // A sixteenth of jitter (a resting finger is never perfectly still)
        // does not count as moving.
        if goal.abs_diff(self.goal) <= 1 && goal % GOAL_SCALE != 0 {
            self.still = self.still.saturating_add(1);
        } else {
            self.goal = goal;
            self.still = 0;
        }
        let rest = if self.goal % GOAL_SCALE == 0 {
            Some((self.goal / GOAL_SCALE) as u8)
        } else if self.still >= REST_FRAMES {
            Some(((self.goal + GOAL_SCALE / 2) / GOAL_SCALE) as u8)
        } else {
            None
        };
        self.settled = match rest {
            Some(level) => {
                let mut budget = budget;
                self.lower(level, u64::MAX, gap, &mut budget);
                self.raise(level, 0, gap, &mut budget);
                self.levels.iter().all(|row| *row == level)
            }
            None => {
                let rows = self.levels.len() as u64;
                let target = u64::from(self.goal) * rows / u64::from(GOAL_SCALE);
                // Half a sixteenth of a level either way is close enough.
                let slack = rows / u64::from(2 * GOAL_SCALE);
                let sum: u64 = self.levels.iter().map(|row| u64::from(*row)).sum();
                let mut budget = budget;
                if sum + slack < target {
                    self.raise(max, target, gap, &mut budget);
                } else if sum > target + slack {
                    self.lower(0, target, gap, &mut budget);
                }
                let sum: u64 = self.levels.iter().map(|row| u64::from(*row)).sum();
                sum.abs_diff(target) <= slack
            }
        };
    }

    /// Raises the lowest rows (up to `cap`) until the level sum reaches
    /// `target` or the budget runs out. A row may rise until it is `gap`
    /// levels above its lower neighbour, at most `2 * gap` at a time.
    fn raise(&mut self, cap: u8, target: u64, gap: u8, budget: &mut usize) {
        let mut sum: u64 = self.levels.iter().map(|row| u64::from(*row)).sum();
        let Some(low) = self.levels.iter().copied().min() else {
            return;
        };
        for level in low..cap {
            for index in 0..self.order.len() {
                if *budget == 0 || (target > 0 && sum >= target) {
                    return;
                }
                let row = usize::from(self.order[index]);
                if self.levels[row] != level {
                    continue;
                }
                let neighbours = self.neighbours(row).min();
                let up = (neighbours + gap)
                    .saturating_sub(level)
                    .min(2 * gap)
                    .min(cap - level);
                if up > 0 {
                    self.levels[row] += up;
                    sum += u64::from(up);
                    *budget -= 1;
                }
            }
        }
    }

    /// Lowers the highest rows (down to `floor`) until the level sum falls
    /// to `target` or the budget runs out; the mirror of `raise`.
    fn lower(&mut self, floor: u8, target: u64, gap: u8, budget: &mut usize) {
        let mut sum: u64 = self.levels.iter().map(|row| u64::from(*row)).sum();
        let Some(high) = self.levels.iter().copied().max() else {
            return;
        };
        for level in (floor + 1..=high).rev() {
            for index in 0..self.order.len() {
                if *budget == 0 || (target != u64::MAX && sum <= target) {
                    return;
                }
                let row = usize::from(self.order[index]);
                if self.levels[row] != level {
                    continue;
                }
                let neighbours = self.neighbours(row).max();
                let down = level
                    .saturating_sub(neighbours.saturating_sub(gap))
                    .min(2 * gap)
                    .min(level - floor);
                if down > 0 {
                    self.levels[row] -= down;
                    sum -= u64::from(down);
                    *budget -= 1;
                }
            }
        }
    }

    fn neighbours(&self, row: usize) -> Neighbours {
        let above = row.checked_sub(1).map(|r| self.levels[r]);
        let below = self.levels.get(row + 1).copied();
        Neighbours(above, below, self.levels[row])
    }

    /// True while rows are still moving (or a goal between levels has not
    /// yet held still long enough to settle on one).
    pub fn settling(&self) -> bool {
        !self.settled || (self.goal % GOAL_SCALE != 0 && self.still < REST_FRAMES)
    }

    /// Jumps straight to a level (entering a screen, not a transition).
    pub fn snap(&mut self, target: f32, max: u8) {
        self.goal = goal_units(target, max);
        self.still = 0;
        let level = ((self.goal + GOAL_SCALE / 2) / GOAL_SCALE) as u8;
        self.levels.fill(level);
        self.settled = true;
    }
}

/// The levels of a row's neighbours (its own level where it has none).
struct Neighbours(Option<u8>, Option<u8>, u8);

impl Neighbours {
    fn min(&self) -> u8 {
        self.0.unwrap_or(self.2).min(self.1.unwrap_or(self.2))
    }

    fn max(&self) -> u8 {
        self.0.unwrap_or(self.2).max(self.1.unwrap_or(self.2))
    }
}

fn goal_units(target: f32, max: u8) -> u32 {
    (target.clamp(0.0, f32::from(max)) * GOAL_SCALE as f32).round() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn max_neighbour_gap(fade: &RowFade) -> u8 {
        fade.levels
            .windows(2)
            .map(|pair| pair[0].abs_diff(pair[1]))
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn levels_are_close_and_every_stop_is_one() {
        let dims = level_dims(&[0.42, 0.55], LEVEL_STEP);
        let mut factor = 1.0;
        for dim in &dims {
            let next = 1.0 - dim;
            assert!(1.0 - next / factor <= LEVEL_STEP + 1e-4, "{dims:?}");
            factor = next;
        }
        assert!(dims.contains(&0.42) && *dims.last().unwrap() == 0.55);
        assert_eq!(level_of(&dims, 0.0), 0.0);
        let keypad = dims.iter().position(|d| *d == 0.42).unwrap() + 1;
        assert_eq!(level_of(&dims, 0.42), keypad as f32);
    }

    #[test]
    fn a_fade_keeps_neighbouring_rows_one_level_apart() {
        let mut fade = RowFade::new(2340);
        let mut frames = 0;
        let mut changed_rows = 0;
        loop {
            let before = fade.levels.clone();
            fade.update(14.0, 14, 300, 1);
            changed_rows += before
                .iter()
                .zip(&fade.levels)
                .filter(|(a, b)| a != b)
                .count();
            assert!(max_neighbour_gap(&fade) <= 1);
            frames += 1;
            if !fade.settling() {
                break;
            }
            assert!(frames < 1000);
        }
        assert!(fade.levels.iter().all(|row| *row == 14));
        // Two levels per changed row most of the way: about 7.5 screens
        // of rows, not 14.
        assert!(changed_rows <= 2340 * 8, "{changed_rows}");
        assert!(frames <= changed_rows.div_ceil(300) * 3 / 2 + 4, "{frames}");
    }

    #[test]
    fn a_fractional_target_tracks_the_mean_and_rests_on_a_level() {
        let mut fade = RowFade::new(2340);
        for _ in 0..20 {
            fade.update(3.4, 14, 560, 1);
            assert!(max_neighbour_gap(&fade) <= 1);
        }
        let mean = fade.levels.iter().map(|r| f32::from(*r)).sum::<f32>() / 2340.0;
        assert!((mean - 3.4).abs() < 0.05, "{mean}");
        // Held still, it settles on the nearest level.
        let mut frames = 0;
        while fade.settling() {
            fade.update(3.4, 14, 560, 1);
            frames += 1;
            assert!(frames < 200);
        }
        assert!(fade.levels.iter().all(|row| *row == 3));
        // And back down, to rest at 0.
        while {
            fade.update(0.0, 14, 560, 1);
            fade.settling()
        } {
            assert!(max_neighbour_gap(&fade) <= 1);
        }
        assert!(fade.levels.iter().all(|row| *row == 0));
    }

    #[test]
    fn catching_up_takes_fewer_rows_and_smooths_out_again() {
        let changes = |gap: u8| {
            let mut fade = RowFade::new(2340);
            let mut rows = 0;
            for _ in 0..1000 {
                let before = fade.levels.clone();
                fade.update(16.0, 24, 400, gap);
                rows += before
                    .iter()
                    .zip(&fade.levels)
                    .filter(|(a, b)| a != b)
                    .count();
                assert!(max_neighbour_gap(&fade) <= gap);
                if !fade.settling() {
                    break;
                }
            }
            assert!(fade.levels.iter().all(|row| *row == 16));
            rows
        };
        let (smooth, fast) = (changes(1), changes(2));
        assert!(fast * 10 <= smooth * 6, "{smooth} {fast}");
        // Gaps left by catching up close as the fade continues smoothly.
        let mut fade = RowFade::new(2340);
        for _ in 0..6 {
            fade.update(9.3, 24, 400, 2);
        }
        for _ in 0..200 {
            fade.update(12.0, 24, 400, 1);
        }
        assert!(fade.levels.iter().all(|row| *row == 12));
    }

    #[test]
    fn each_frame_changes_rows_spread_over_the_screen() {
        let mut fade = RowFade::new(2340);
        let before = fade.levels.clone();
        fade.update(5.0, 14, 146, 1);
        let changed: Vec<usize> = (0..2340)
            .filter(|r| before[*r] != fade.levels[*r])
            .collect();
        assert_eq!(changed.len(), 146);
        // No two within 8 rows of each other, and no gap above 40 rows.
        for pair in changed.windows(2) {
            assert!((8..=40).contains(&(pair[1] - pair[0])), "{pair:?}");
        }
    }
}
