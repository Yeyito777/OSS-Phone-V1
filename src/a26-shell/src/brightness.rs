//! Brightness settings and automatic brightness (docs/display-brightness.md).
//!
//! Pure: lux readings and time come in, a backlight level comes out. Levels
//! are 0..=100 and map to the panel backlight on a perceptual (square) curve.
//! The level follows only when every reading for 1 s (brighter) or 4 s
//! (darker) lies outside a ±20 % band around the light it was last set for;
//! the new level comes from the log-lux average, and is ramped to.
//!
//! The sensor sits under the panel, so readings taken while it is lit are
//! compensated for its light (`compensation.rs`) and followed with an
//! up-only bias (docs/auto-brightness-compensation.md §5): brighter only
//! when even the low end of the estimate is (and stays so at the new
//! level's larger leak), at most once per 10 s; darker only when even the
//! high end is; not at all in a dark room with a large leak. The reading
//! taken on each wake while the panel is still dark is trusted as is.

use std::fs;
use std::io;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::compensation::{Calibration, Estimate, WakeWindow};
use crate::display::{DisplayRequest, DisplayView};
use crate::input::Backlight;
use crate::light::{self, LightReading, LightSensor};
use crate::model::ShellState;

/// (lux, level) anchor points, interpolated in log-lux.
const CURVE: [(f32, f32); 7] = [
    (0.0, 6.0),
    (10.0, 18.0),
    (50.0, 30.0),
    (200.0, 45.0),
    (1000.0, 65.0),
    (5000.0, 85.0),
    (20000.0, 100.0),
];
const BAND: f32 = 0.2;
const BRIGHTER_AFTER: Duration = Duration::from_secs(1);
const DARKER_AFTER: Duration = Duration::from_secs(4);
/// Exponential averaging of log-lux per reading (readings arrive ~5 Hz); it
/// sets the new level, while the band test uses each raw reading so that a
/// short shadow is forgotten as soon as the light is back.
const SMOOTHING: f32 = 0.4;
/// Ramp speed in levels per second; a full swing takes ~1.5 s, small ones less.
const RAMP_LEVELS_PER_S: f32 = 60.0;
pub const RAMP_STEP: Duration = Duration::from_millis(33);
/// Lit readings raise the level at most this often (no runaway).
const LIT_RISE_EVERY: Duration = Duration::from_secs(10);
/// On a wake with auto on, the backlight stays dark until the first reading
/// (normally ~0.1 s), at most this long.
pub const WAKE_HOLD: Duration = Duration::from_millis(300);
/// At screen-off, how long to wait for the dark-panel reading.
const CLOSE_WAIT: Duration = Duration::from_millis(500);
/// The sensor integrates for up to ~0.2 s: a lit reading counts only if the
/// backlight was steady that long before it.
const SETTLE_NS: u64 = 300_000_000;
/// No reading for this long while on: the sensor is reported unavailable.
const SILENT_LIMIT: Duration = Duration::from_secs(20);
/// The CSV log of readings rotates past this size.
const LOG_LIMIT: u64 = 1 << 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub brightness: u8,
    pub auto: bool,
    pub auto_offset: i8,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            brightness: 40,
            auto: false,
            auto_offset: 0,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Option<Self> {
        let settings: Self = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        Some(Self {
            brightness: settings.brightness.min(100),
            auto: settings.auto,
            auto_offset: settings.auto_offset.clamp(-50, 50),
        })
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SensorState {
    Ok,
    Unavailable,
    Off,
}

/// Level 0..=100 -> backlight 1..=max, perceptual (square) curve.
pub fn backlight_for(level: f32, max: u16) -> u16 {
    let fraction = (level / 100.0).clamp(0.0, 1.0);
    let max = f32::from(max.max(1));
    (1.0 + (max - 1.0) * fraction * fraction).round() as u16
}

/// Inverse of `backlight_for` (for adopting a level the panel already has).
pub fn level_for(backlight: u16, max: u16) -> u8 {
    let max = f32::from(max.max(2));
    let fraction = ((f32::from(backlight) - 1.0) / (max - 1.0)).clamp(0.0, 1.0);
    (fraction.sqrt() * 100.0).round() as u8
}

/// The automatic level for `lux`, before the user's offset.
pub fn curve(lux: f32) -> f32 {
    let x = (lux.max(0.0) + 1.0).log10();
    let points = CURVE.map(|(l, v)| ((l + 1.0).log10(), v));
    if x <= points[0].0 {
        return points[0].1;
    }
    for pair in points.windows(2) {
        let ((x0, y0), (x1, y1)) = (pair[0], pair[1]);
        if x <= x1 {
            return y0 + (y1 - y0) * (x - x0) / (x1 - x0);
        }
    }
    points[points.len() - 1].1
}

pub struct AutoBrightness {
    pub settings: Settings,
    pub sensor: SensorState,
    /// Averaged log10(lux + 1).
    smoothed: Option<f32>,
    /// The light the automatic level was last set for.
    anchor: Option<f32>,
    /// Since when the light has been outside the band (and which way).
    outside_since: Option<(Instant, bool)>,
    last_lux: Option<u32>,
    target: f32,
    current: f32,
    last_step: Option<Instant>,
    last_lit_rise: Option<Instant>,
}

impl AutoBrightness {
    pub fn new(settings: Settings) -> Self {
        let level = f32::from(settings.brightness);
        Self {
            settings,
            sensor: SensorState::Off,
            smoothed: None,
            anchor: None,
            outside_since: None,
            last_lux: None,
            target: level,
            current: level,
            last_step: None,
            last_lit_rise: None,
        }
    }

    pub fn lux(&self) -> Option<u32> {
        (self.sensor == SensorState::Ok)
            .then_some(self.last_lux)
            .flatten()
    }

    /// The level now on the panel (0..=100).
    pub fn level(&self) -> u8 {
        self.current.round().clamp(0.0, 100.0) as u8
    }

    fn auto_level(&self, log_lux: f32) -> f32 {
        let lux = 10f32.powf(log_lux) - 1.0;
        (curve(lux) + f32::from(self.settings.auto_offset)).clamp(0.0, 100.0)
    }

    /// Screen on: the sensor (re)starts; the first reading applies at once.
    pub fn screen_on(&mut self, sensor_available: bool) {
        self.sensor = if sensor_available {
            SensorState::Ok
        } else {
            SensorState::Unavailable
        };
        self.smoothed = None;
        self.anchor = None;
        self.outside_since = None;
        self.last_lux = None;
        self.last_lit_rise = None;
    }

    pub fn screen_off(&mut self) {
        self.sensor = SensorState::Off;
        self.outside_since = None;
        self.last_lux = None;
        self.current = self.target;
        self.last_step = None;
    }

    /// One reading. Returns true when the target level changed.
    pub fn reading(&mut self, lux: u32, now: Instant) -> bool {
        self.last_lux = Some(lux);
        let sample = (lux as f32 + 1.0).log10();
        let smoothed = match self.smoothed {
            None => sample,
            Some(previous) => previous + SMOOTHING * (sample - previous),
        };
        self.smoothed = Some(smoothed);
        if !self.settings.auto {
            return false;
        }
        let Some(anchor) = self.anchor else {
            // First reading after screen-on: apply at once.
            self.anchor = Some(smoothed);
            return self.retarget(self.auto_level(smoothed), now, true);
        };
        let band = (1.0 + BAND).log10();
        let brighter = sample > anchor + band;
        let darker = sample < anchor - band;
        if !brighter && !darker {
            self.outside_since = None;
            return false;
        }
        let since = match self.outside_since {
            Some((since, direction)) if direction == brighter => since,
            _ => {
                self.outside_since = Some((now, brighter));
                now
            }
        };
        let wait = if brighter {
            BRIGHTER_AFTER
        } else {
            DARKER_AFTER
        };
        if now.duration_since(since) < wait {
            return false;
        }
        self.anchor = Some(smoothed);
        self.outside_since = None;
        self.retarget(self.auto_level(smoothed), now, false)
    }

    /// A reading taken with the panel lit, compensated for its light.
    /// `at_level` re-estimates the same reading as if the panel were at a
    /// given level (for the no-runaway check). Returns true when the target
    /// level changed.
    pub fn lit_reading(
        &mut self,
        estimate: Estimate,
        now: Instant,
        at_level: impl Fn(f32) -> Estimate,
    ) -> bool {
        self.last_lux = Some(estimate.ambient.round() as u32);
        if estimate.untrusted() {
            self.outside_since = None;
            return false;
        }
        let ambient = (estimate.ambient + 1.0).log10();
        self.smoothed = Some(match self.smoothed {
            None => ambient,
            Some(previous) => previous + SMOOTHING * (ambient - previous),
        });
        if !self.settings.auto {
            return false;
        }
        let low = (estimate.low() + 1.0).log10();
        let high = (estimate.high() + 1.0).log10();
        let Some(anchor) = self.anchor else {
            // No dark-panel reading this wake: start from the low end.
            self.anchor = Some(low);
            return self.retarget(self.auto_level(low), now, false);
        };
        let band = (1.0 + BAND).log10();
        let brighter = low > anchor + band;
        let darker = high < anchor - band;
        if !brighter && !darker {
            self.outside_since = None;
            return false;
        }
        let since = match self.outside_since {
            Some((since, direction)) if direction == brighter => since,
            _ => {
                self.outside_since = Some((now, brighter));
                now
            }
        };
        let wait = if brighter {
            BRIGHTER_AFTER
        } else {
            DARKER_AFTER
        };
        if now.duration_since(since) < wait {
            return false;
        }
        let new_anchor = if brighter {
            if self
                .last_lit_rise
                .is_some_and(|at| now.duration_since(at) < LIT_RISE_EVERY)
            {
                return false;
            }
            // The estimate must survive the larger leak the rise would cause.
            let worst = (at_level(self.auto_level(low)).low() + 1.0).log10();
            if worst <= anchor + band {
                return false;
            }
            self.last_lit_rise = Some(now);
            worst.min(low)
        } else {
            high
        };
        self.anchor = Some(new_anchor);
        self.outside_since = None;
        self.retarget(self.auto_level(new_anchor), now, false)
    }

    /// The user's brightness (`brightness set N`). With auto on, re-anchors
    /// the curve so the current light gives `N`.
    pub fn set_user_level(&mut self, level: u8, now: Instant) {
        let level = level.min(100);
        if self.settings.auto
            && let Some(smoothed) = self.smoothed
        {
            let base = curve(10f32.powf(smoothed) - 1.0);
            self.settings.auto_offset = (f32::from(level) - base).round().clamp(-50.0, 50.0) as i8;
            self.anchor = Some(smoothed);
        } else if self.settings.auto {
            // No reading yet: adjust the offset by the change.
            let delta = i16::from(level) - i16::from(self.level());
            self.settings.auto_offset =
                (i16::from(self.settings.auto_offset) + delta).clamp(-50, 50) as i8;
        }
        self.settings.brightness = level;
        self.retarget(f32::from(level), now, false);
    }

    pub fn set_auto(&mut self, auto: bool, now: Instant) {
        if self.settings.auto == auto {
            return;
        }
        self.settings.auto = auto;
        if auto {
            self.anchor = None;
            if let Some(smoothed) = self.smoothed {
                self.anchor = Some(smoothed);
                let level = self.auto_level(smoothed);
                self.retarget(level, now, false);
            }
        } else {
            self.settings.brightness = self.level();
            self.target = self.current;
        }
    }

    fn retarget(&mut self, level: f32, now: Instant, immediate: bool) -> bool {
        let changed = (level - self.target).abs() >= 0.5;
        self.target = level;
        if self.settings.auto {
            self.settings.brightness = level.round() as u8;
        }
        if immediate {
            self.current = level;
            self.last_step = None;
        } else if changed {
            self.last_step = Some(now);
        }
        changed
    }

    /// Advances the ramp; returns the level to write when it moved.
    pub fn step(&mut self, now: Instant) -> Option<u8> {
        let last = self.last_step?;
        let before = self.level();
        let dt = now.duration_since(last).as_secs_f32();
        let max_move = RAMP_LEVELS_PER_S * dt;
        let delta = self.target - self.current;
        if delta.abs() <= max_move {
            self.current = self.target;
            self.last_step = None;
        } else {
            self.current += max_move.copysign(delta);
            self.last_step = Some(now);
        }
        (self.level() != before).then(|| self.level())
    }

    /// Wake for the next ramp step, or for a pending hysteresis decision.
    pub fn next_deadline(&self) -> Option<Instant> {
        let ramp = self.last_step.map(|last| last + RAMP_STEP);
        let pending = self.outside_since.map(|(since, brighter)| {
            since
                + if brighter {
                    BRIGHTER_AFTER
                } else {
                    DARKER_AFTER
                }
        });
        ramp.into_iter().chain(pending).min()
    }

    pub fn ramping(&self) -> bool {
        self.last_step.is_some()
    }
}

/// `state.display` and the reply to the `brightness` commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PublicDisplay {
    pub brightness: u8,
    pub auto: bool,
    pub auto_offset: i8,
    pub lux: Option<u32>,
    pub sensor: SensorState,
}

/// Where the sensor is in its screen-on/off cycle.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    Off,
    /// Woken with auto on: the backlight stays dark until the first reading.
    Holding {
        until: Instant,
    },
    On,
    /// Screen off, panel dark: waiting for the reading that closes the wake.
    Closing {
        until: Instant,
    },
}

/// Moon's brightness backend: the light sensor while the screen is on, the
/// automatic level, the backlight and the persisted settings.
pub struct BrightnessControl {
    pub auto: AutoBrightness,
    sensor: Option<LightSensor>,
    sensor_ok: bool,
    phase: Phase,
    awake: bool,
    path: PathBuf,
    persisted: Settings,
    written: Option<u16>,
    calibration: Calibration,
    calibration_path: PathBuf,
    log_path: PathBuf,
    window: Option<WakeWindow>,
    last_reading: Option<Instant>,
    started: Option<Instant>,
}

impl BrightnessControl {
    /// `current` is the backlight Moon found, adopted as the level when
    /// there are no saved settings yet. The calibration and the reading log
    /// live next to `path`.
    pub fn new(path: PathBuf, current: Option<(u16, u16)>) -> Self {
        let settings = Settings::load(&path).unwrap_or_else(|| Settings {
            brightness: current.map_or(40, |(value, max)| level_for(value, max)),
            ..Settings::default()
        });
        let sensor = LightSensor::find();
        if sensor.is_none() {
            eprintln!("ambient light sensor not found; automatic brightness unavailable");
        }
        let calibration_path = path.with_file_name("light-calibration.json");
        let log_path = path.with_file_name("light-log.csv");
        Self {
            auto: AutoBrightness::new(settings),
            sensor,
            sensor_ok: true,
            phase: Phase::Off,
            awake: false,
            persisted: settings,
            written: None,
            calibration: Calibration::load(&calibration_path).unwrap_or_default(),
            calibration_path,
            log_path,
            path,
            window: None,
            last_reading: None,
            started: None,
        }
    }

    pub fn sensor_fd(&self) -> Option<RawFd> {
        self.sensor.as_ref().and_then(LightSensor::raw_fd)
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        let phase = match self.phase {
            Phase::Holding { until } | Phase::Closing { until } => Some(until),
            _ => None,
        };
        let silent = (self.phase == Phase::On && self.sensor_ok)
            .then(|| {
                self.last_reading
                    .or(self.started)
                    .map(|at| at + SILENT_LIMIT)
            })
            .flatten();
        [self.auto.next_deadline(), phase, silent]
            .into_iter()
            .flatten()
            .min()
    }

    pub fn public(&self) -> PublicDisplay {
        PublicDisplay {
            brightness: self.auto.level(),
            auto: self.auto.settings.auto,
            auto_offset: self.auto.settings.auto_offset,
            lux: self.auto.lux(),
            sensor: if self.sensor.is_none() || !self.sensor_ok {
                SensorState::Unavailable
            } else {
                self.auto.sensor
            },
        }
    }

    pub fn request(&mut self, request: DisplayRequest, now: Instant) {
        match request {
            DisplayRequest::Set(level) => self.auto.set_user_level(level, now),
            DisplayRequest::Auto(on) => self.auto.set_auto(on, now),
        }
    }

    /// Once per loop: follows the screen state, drains in-shell requests,
    /// reads the sensor, ramps, writes the backlight when Moon may (screen
    /// on, no power fade, no Auto-Lock dim), persists, refreshes the view.
    /// On a wake with auto on it holds the fade-in dark until the first
    /// reading (`state.fade` starts later).
    pub fn service(
        &mut self,
        state: &mut ShellState,
        mut backlight: Option<&mut Backlight>,
        fading: bool,
        now: Instant,
    ) {
        if state.screen_awake && !self.awake {
            self.wake(state, now);
        } else if !state.screen_awake && self.awake {
            self.sleep(now);
        }
        if let Phase::Holding { until } = self.phase {
            // Idempotent: also covers a fade-in started after the wake.
            hold_fade(state, until);
        }
        for request in std::mem::take(&mut state.display_requests) {
            self.request(request, now);
        }
        let readings = self
            .sensor
            .as_mut()
            .map(LightSensor::read)
            .unwrap_or_default();
        let max = backlight.as_deref().map_or(255, Backlight::max);
        for reading in readings {
            self.last_reading = Some(now);
            self.sensor_ok = true;
            self.handle(reading, backlight.as_deref(), max, state, now);
        }
        self.timeouts(state, now);
        self.auto.step(now);
        if let Some(backlight) = backlight.as_mut() {
            let value = backlight_for(f32::from(self.auto.level()), backlight.max());
            backlight.set_saved(value);
            if state.screen_awake && !fading && !state.idle_dimmed {
                if self.written != Some(value) {
                    let _ = backlight.on();
                    self.written = Some(value);
                }
            } else {
                // Restore our level once the fade, dim or screen-off ends.
                self.written = None;
            }
        }
        if self.auto.settings != self.persisted && !self.auto.ramping() {
            match self.auto.settings.save(&self.path) {
                Ok(()) => self.persisted = self.auto.settings,
                Err(error) => eprintln!("brightness settings not saved: {error}"),
            }
        }
        let public = self.public();
        state.display_view = Some(DisplayView {
            brightness: public.brightness,
            auto: public.auto,
            sensor_available: public.sensor != SensorState::Unavailable,
        });
    }

    fn wake(&mut self, state: &mut ShellState, now: Instant) {
        self.awake = true;
        self.window = None;
        let started = match self.sensor.as_mut() {
            Some(sensor) => match sensor.start() {
                Ok(()) => true,
                Err(error) => {
                    eprintln!("ambient light sensor start failed: {error}");
                    false
                }
            },
            None => false,
        };
        self.auto.screen_on(started);
        self.started = started.then_some(now);
        self.last_reading = None;
        self.phase = if started && self.auto.settings.auto && self.sensor_ok {
            hold_fade(state, now + WAKE_HOLD);
            Phase::Holding {
                until: now + WAKE_HOLD,
            }
        } else {
            Phase::On
        };
    }

    fn sleep(&mut self, now: Instant) {
        self.awake = false;
        self.auto.screen_off();
        let closable = self
            .window
            .as_ref()
            .is_some_and(|window| window.closable(light::boottime_ns()));
        let retriggered = closable
            && self
                .sensor
                .as_mut()
                .is_some_and(|sensor| sensor.retrigger().is_ok());
        if retriggered {
            self.phase = Phase::Closing {
                until: now + CLOSE_WAIT,
            };
        } else {
            self.stop_sensor();
        }
    }

    fn stop_sensor(&mut self) {
        if let Some(sensor) = self.sensor.as_mut() {
            sensor.stop();
        }
        self.window = None;
        self.phase = Phase::Off;
    }

    fn handle(
        &mut self,
        reading: LightReading,
        backlight: Option<&Backlight>,
        max: u16,
        state: &mut ShellState,
        now: Instant,
    ) {
        match self.phase {
            Phase::Off => {}
            Phase::Closing { .. } => {
                let dark = backlight
                    .and_then(|device| device.value_at(reading.at_ns))
                    .is_none_or(|value| value == 0);
                if reading.first && dark {
                    let accepted = self
                        .window
                        .take()
                        .is_some_and(|window| window.close(reading.lux, &mut self.calibration));
                    self.log("a1", reading, 0, None);
                    if accepted && let Err(error) = self.calibration.save(&self.calibration_path) {
                        eprintln!("light calibration not saved: {error}");
                    }
                    self.stop_sensor();
                }
            }
            Phase::Holding { .. } | Phase::On => {
                // The dark-panel reading of this wake: trusted as ambient.
                let dark = backlight
                    .and_then(|device| device.value_at(reading.at_ns))
                    .is_some_and(|value| value <= max / 30);
                if reading.first && self.window.is_none() && dark {
                    let bl = backlight
                        .and_then(|d| d.value_at(reading.at_ns))
                        .unwrap_or(0);
                    let ambient = (reading.lux as f32 - self.calibration.leak(bl, max))
                        .max(0.0)
                        .round() as u32;
                    self.window = Some(WakeWindow::new(ambient, reading.at_ns));
                    self.auto.reading(ambient, now);
                    self.log("a0", reading, bl, None);
                    release_fade(state, now);
                    self.phase = Phase::On;
                    return;
                }
                let Some(bl) =
                    backlight.and_then(|device| device.steady_at(reading.at_ns, SETTLE_NS))
                else {
                    return;
                };
                if bl == 0 {
                    return;
                }
                if let Some(window) = self.window.as_mut() {
                    window.sample(reading.lux, reading.at_ns, bl, max);
                }
                let calibration = self.calibration;
                let estimate = calibration.estimate(reading.lux, bl, max);
                self.auto.lit_reading(estimate, now, |level| {
                    calibration.estimate(reading.lux, backlight_for(level, max).max(bl), max)
                });
                self.log("lit", reading, bl, Some(estimate));
            }
        }
    }

    fn timeouts(&mut self, state: &mut ShellState, now: Instant) {
        match self.phase {
            Phase::Holding { until } if now >= until => {
                // No reading in time: light up anyway, without calibration.
                release_fade(state, now);
                self.phase = Phase::On;
            }
            Phase::Closing { until } if now >= until => self.stop_sensor(),
            Phase::On => {
                let silent_since = self.last_reading.or(self.started);
                if self.sensor_ok
                    && silent_since.is_some_and(|at| now.duration_since(at) >= SILENT_LIMIT)
                {
                    eprintln!("ambient light sensor: no reading for 20 s; unavailable");
                    self.sensor_ok = false;
                }
            }
            _ => {}
        }
    }

    /// Appends a reading to the CSV log (for checking the fit offline).
    fn log(&self, kind: &str, reading: LightReading, bl: u16, estimate: Option<Estimate>) {
        use std::io::Write;
        if fs::metadata(&self.log_path).is_ok_and(|meta| meta.len() > LOG_LIMIT) {
            let _ = fs::rename(&self.log_path, self.log_path.with_extension("csv.1"));
        }
        let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        else {
            return;
        };
        let (leak, ambient, margin) = estimate.map_or((0.0, reading.lux as f32, 0.0), |e| {
            (e.leak, e.ambient, e.margin)
        });
        let _ = writeln!(
            file,
            "{},{kind},{},{bl},{leak:.1},{ambient:.1},{margin:.1},{},{:.1}",
            reading.at_ns,
            reading.lux,
            self.auto.level(),
            self.calibration.k
        );
    }
}

/// Keeps a starting fade-in dark until `until`.
fn hold_fade(state: &mut ShellState, until: Instant) {
    if let Some(fade) = state.fade.as_mut()
        && !fade.out
    {
        fade.started = fade.started.max(until);
    }
}

/// Starts a held fade-in now.
fn release_fade(state: &mut ShellState, now: Instant) {
    if let Some(fade) = state.fade.as_mut()
        && !fade.out
        && fade.started > now
    {
        fade.started = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auto() -> AutoBrightness {
        let mut a = AutoBrightness::new(Settings {
            brightness: 40,
            auto: true,
            auto_offset: 0,
        });
        a.screen_on(true);
        a
    }

    fn run_ramp(a: &mut AutoBrightness, mut now: Instant) -> Instant {
        while a.ramping() {
            now += RAMP_STEP;
            a.step(now);
        }
        now
    }

    #[test]
    fn curve_and_backlight_mapping() {
        assert_eq!(curve(0.0).round(), 6.0);
        assert_eq!(curve(200.0).round(), 45.0);
        assert_eq!(curve(100_000.0), 100.0);
        assert!(curve(100.0) > curve(50.0) && curve(100.0) < curve(200.0));
        assert_eq!(backlight_for(0.0, 665), 1);
        assert_eq!(backlight_for(100.0, 665), 665);
        assert_eq!(backlight_for(50.0, 665), 167);
        for level in [0u8, 10, 37, 50, 99, 100] {
            assert_eq!(level_for(backlight_for(f32::from(level), 665), 665), level);
        }
    }

    #[test]
    fn first_reading_applies_at_once_then_hysteresis() {
        let mut a = auto();
        let t = Instant::now();
        assert!(a.reading(200, t));
        assert_eq!(a.level(), 45);
        assert!(!a.ramping());
        // Inside the ±20 % band: nothing happens.
        assert!(!a.reading(230, t + Duration::from_millis(200)));
        // A dark moment shorter than 4 s does not dim.
        for i in 1..15 {
            assert!(!a.reading(5, t + Duration::from_millis(200 * i)), "{i}");
        }
        // Back to normal light: the pending darkening is dropped.
        for i in 15..30 {
            a.reading(200, t + Duration::from_millis(200 * i));
        }
        assert_eq!(a.level(), 45);
        assert_eq!(a.next_deadline(), None);
    }

    #[test]
    fn brighter_follows_after_one_second_and_ramps() {
        let mut a = auto();
        let t = Instant::now();
        a.reading(200, t);
        let mut now = t;
        let mut changed = false;
        for i in 1..=10 {
            now = t + Duration::from_millis(200 * i);
            changed |= a.reading(5000, now);
        }
        assert!(changed);
        assert!(a.ramping());
        run_ramp(&mut a, now);
        assert!(a.level() >= 80, "level {}", a.level());
    }

    #[test]
    fn user_adjustment_reanchors_the_curve() {
        let mut a = auto();
        let t = Instant::now();
        a.reading(200, t);
        a.set_user_level(55, t);
        assert_eq!(a.settings.auto_offset, 10);
        let now = run_ramp(&mut a, t);
        assert_eq!(a.level(), 55);
        // Much brighter light: curve + 10.
        let mut now2 = now;
        for i in 1..=20 {
            now2 = now + Duration::from_millis(200 * i);
            a.reading(20000, now2);
        }
        run_ramp(&mut a, now2);
        assert!(a.level() >= 95);
    }

    #[test]
    fn manual_mode_ignores_light_and_keeps_level() {
        let mut a = AutoBrightness::new(Settings::default());
        a.screen_on(true);
        let t = Instant::now();
        assert!(!a.reading(20000, t));
        assert_eq!(a.level(), 40);
        a.set_user_level(70, t);
        run_ramp(&mut a, t);
        assert_eq!(a.level(), 70);
        assert_eq!(a.settings.brightness, 70);
        // Switching auto on uses the current light.
        a.set_auto(true, t);
        run_ramp(&mut a, t);
        assert_eq!(a.level(), 100);
        a.set_auto(false, t);
        assert_eq!(a.settings.brightness, 100);
    }

    /// A lit reading at `bl` under the default calibration (k = 110).
    fn lit(a: &mut AutoBrightness, lux: u32, bl: u16, now: Instant) -> bool {
        let c = Calibration::default();
        a.lit_reading(c.estimate(lux, bl, 665), now, |level| {
            c.estimate(lux, backlight_for(level, 665).max(bl), 665)
        })
    }

    #[test]
    fn lit_readings_rise_only_beyond_the_leak_and_never_run_away() {
        let mut a = auto();
        let t = Instant::now();
        // Wake in a 50 lx room (dark panel): level 30.
        a.reading(50, t);
        assert_eq!(a.level(), 30);
        // Lit at full backlight: 160 lx measured is ~50 lx ambient + leak.
        for i in 1..=6 {
            assert!(!lit(&mut a, 160, 665, t + Duration::from_secs(6 * i)));
        }
        // The lights go on: 900 lx measured at backlight 167 (leak ~14).
        let s = t + Duration::from_secs(60);
        assert!(!lit(&mut a, 900, 167, s));
        assert!(lit(&mut a, 900, 167, s + Duration::from_secs(6)));
        let level = a.target;
        assert!(level > 55.0, "level {level}");
        // Another rise within 10 s is refused.
        a.anchor = Some((300f32).log10());
        assert!(!lit(&mut a, 5000, 167, s + Duration::from_secs(8)));
        assert!(!lit(&mut a, 5000, 167, s + Duration::from_secs(12)));
        assert!(lit(&mut a, 5000, 167, s + Duration::from_secs(18)));
    }

    #[test]
    fn dark_rooms_and_uncertain_leaks_hold_the_level() {
        let mut a = auto();
        let t = Instant::now();
        a.reading(5, t);
        let level = a.level();
        // A dark room at full backlight: the reading is mostly leak.
        for i in 1..=6 {
            assert!(!lit(&mut a, 120, 665, t + Duration::from_secs(6 * i)));
        }
        assert_eq!(a.level(), level);
        // Darker only when even the high end is below the band.
        let mut a = auto();
        a.reading(200, t);
        assert!(!lit(&mut a, 230, 400, t + Duration::from_secs(6)));
        assert!(!lit(&mut a, 230, 400, t + Duration::from_secs(12)));
        assert!(!lit(&mut a, 20, 30, t + Duration::from_secs(18)));
        assert!(lit(&mut a, 20, 30, t + Duration::from_secs(24)));
        assert!(a.target < 45.0);
    }

    #[test]
    fn settings_round_trip_and_clamp() {
        let dir = std::env::temp_dir().join(format!("moon-display-{}", std::process::id()));
        let path = dir.join("display.json");
        Settings {
            brightness: 42,
            auto: true,
            auto_offset: -6,
        }
        .save(&path)
        .unwrap();
        assert_eq!(Settings::load(&path).unwrap().auto_offset, -6);
        fs::write(&path, r#"{"brightness": 250, "auto_offset": 90}"#).unwrap();
        let clamped = Settings::load(&path).unwrap();
        assert_eq!(
            (clamped.brightness, clamped.auto_offset, clamped.auto),
            (100, 50, false)
        );
        let _ = fs::remove_dir_all(dir);
    }
}
