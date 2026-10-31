//! The phone's tilt, for wallpapers with depth (wallpaper.rs `build`).
//!
//! The accelerometer is on Samsung's sensor hub, like the light sensor
//! (light.rs): switched with /sys/class/sensors/ssp_sensor/enable (type 1,
//! the hub counts references) and read from the driver's last event in
//! /sys/class/sensors/accelerometer_sensor/raw_data (`x,y,z`, 4096 = 1 g).
//! It runs only while a dynamic wallpaper is on screen with the screen on.
//!
//! The target offset is the tilt away from how the phone has been held
//! lately (a slowly re-centring baseline), so any comfortable pose looks
//! centred and only moving the phone moves the layers. What is shown
//! follows the target on a critically damped spring evaluated at each
//! frame's display time, so the motion is smooth at the panel's 120 Hz
//! whatever the sensor's rate; frames are drawn only while it moves. Sensor
//! noise below `DEADBAND` never moves the target: a still phone draws
//! nothing.
//!
//! `MOON_TILT=X,Y` holds the target there instead, and `MOON_TILT=demo`
//! sweeps it (for sandboxes and screenshots, which have no sensor).

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::motion::Spring;

const ACCEL_TYPE: u32 = 1;
const ENABLE: &str = "/sys/class/sensors/ssp_sensor/enable";
const RAW: &str = "/sys/class/sensors/accelerometer_sensor/raw_data";
const BATCH: &str = "/dev/batch_io";
const ONE_G: f32 = 4096.0;
const PERIOD: Duration = Duration::from_millis(20);
/// Smoothing of the readings, and how fast the neutral pose follows.
const SMOOTH_S: f32 = 0.06;
const RECENTRE_S: f32 = 3.0;
/// Unit offset (+/- 1) per this much tilt, in g.
const RANGE_G: f32 = 0.30;
/// Target changes smaller than this (in units: about a third of a pixel at
/// the nearest layer's travel) are noise.
const DEADBAND: f32 = 0.004;
/// The spring: its undamped period (s) and damping fraction (critical).
const RESPONSE_S: f32 = 0.42;
const DAMPING: f32 = 1.0;
/// Settled within this (units).
const SETTLED: f32 = 0.0015;

/// What is shown: a spring per axis toward the target, -1..1; x positive
/// when the right edge dips, y positive when the top edge dips.
static SHOWN: Mutex<Option<[Spring; 2]>> = Mutex::new(None);

/// The offset to draw for a frame shown at `at`.
pub fn offset_at(at: Instant) -> (f32, f32) {
    match *SHOWN.lock().unwrap_or_else(|e| e.into_inner()) {
        Some([x, y]) => (x.value(at), y.value(at)),
        None => (0.0, 0.0),
    }
}

/// Whether the offset is still moving at `now` (frames are wanted).
pub fn moving(now: Instant) -> bool {
    SHOWN
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_some_and(|[x, y]| !x.settled(now, SETTLED) || !y.settled(now, SETTLED))
}

/// Sends the shown offset toward `target` from wherever it is at `now`,
/// keeping its velocity.
fn retarget(target: (f32, f32), now: Instant) {
    let mut shown = SHOWN.lock().unwrap_or_else(|e| e.into_inner());
    let spring = |from: Option<Spring>, to: f32| {
        let (value, velocity) = from.map_or((to, 0.0), |s| (s.value(now), s.velocity_at(now)));
        Spring::new(value, to, velocity, now, RESPONSE_S, DAMPING)
    };
    let [x, y] = shown.map_or([None, None], |[x, y]| [Some(x), Some(y)]);
    *shown = Some([spring(x, target.0), spring(y, target.1)]);
}

fn reset() {
    *SHOWN.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

#[derive(Default)]
pub struct Tilt {
    on: bool,
    normalized: bool,
    last: Option<Instant>,
    smooth: (f32, f32),
    neutral: (f32, f32),
    target: (f32, f32),
}

impl Tilt {
    /// Follows the sensor while `active`.
    pub fn update(&mut self, active: bool, now: Instant) {
        if !active {
            if self.on {
                write_enable(false);
                self.on = false;
            }
            if self.last.take().is_some() {
                // Centred for next time: the next pose is the new neutral.
                self.target = (0.0, 0.0);
                reset();
            }
            return;
        }
        if self.last.is_some_and(|last| now < last + PERIOD) {
            return;
        }
        let Some(reading) = self.read(now) else {
            return;
        };
        let Some(last) = self.last.replace(now) else {
            self.smooth = reading;
            self.neutral = reading;
            self.target = (0.0, 0.0);
            retarget(self.target, now);
            return;
        };
        let dt = now.saturating_duration_since(last).as_secs_f32().min(0.5);
        let a = 1.0 - (-dt / SMOOTH_S).exp();
        self.smooth.0 += (reading.0 - self.smooth.0) * a;
        self.smooth.1 += (reading.1 - self.smooth.1) * a;
        let b = 1.0 - (-dt / RECENTRE_S).exp();
        self.neutral.0 += (self.smooth.0 - self.neutral.0) * b;
        self.neutral.1 += (self.smooth.1 - self.neutral.1) * b;
        let target = (
            soft_clamp((self.smooth.0 - self.neutral.0) / RANGE_G),
            soft_clamp((self.smooth.1 - self.neutral.1) / RANGE_G),
        );
        if (target.0 - self.target.0).abs() < DEADBAND
            && (target.1 - self.target.1).abs() < DEADBAND
        {
            return;
        }
        self.target = target;
        retarget(target, now);
    }

    /// The sensor's reading in g, (x, y) in the screen's frame; or the
    /// stand-in from `MOON_TILT`.
    fn read(&mut self, now: Instant) -> Option<(f32, f32)> {
        match stand_in() {
            // Held there: a neutral that never re-centres it.
            Some(StandIn::Fixed(x, y)) => {
                self.neutral = (0.0, 0.0);
                return Some((x * RANGE_G, y * RANGE_G));
            }
            Some(StandIn::Demo(started)) => {
                let t = now.saturating_duration_since(started).as_secs_f32();
                let phase = std::f32::consts::TAU * t;
                return Some((
                    RANGE_G * (phase / 3.1).sin(),
                    0.7 * RANGE_G * (phase / 4.3).sin(),
                ));
            }
            None => {}
        }
        if !self.on {
            if !self.normalized {
                // A Moon that died with the sensor on left a reference
                // behind (light.rs does the same).
                for _ in 0..4 {
                    if !accel_enabled() {
                        break;
                    }
                    write_enable(false);
                }
                self.normalized = true;
            }
            write_enable(true);
            set_period(PERIOD);
            self.on = true;
        }
        read()
    }

    /// When to sample next.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.last.map(|last| (last + PERIOD).max(now))
    }
}

#[derive(Clone, Copy)]
enum StandIn {
    Fixed(f32, f32),
    Demo(Instant),
}

/// `MOON_TILT`: a fixed tilt (X,Y in -1..1), or `demo`, a slow sweep.
fn stand_in() -> Option<StandIn> {
    static STAND_IN: std::sync::OnceLock<Option<StandIn>> = std::sync::OnceLock::new();
    *STAND_IN.get_or_init(|| {
        let value = std::env::var("MOON_TILT").ok()?;
        if value == "demo" {
            return Some(StandIn::Demo(Instant::now()));
        }
        let (x, y) = value.split_once(',')?;
        Some(StandIn::Fixed(
            x.trim().parse::<f32>().ok()?.clamp(-1.0, 1.0),
            y.trim().parse::<f32>().ok()?.clamp(-1.0, 1.0),
        ))
    })
}

/// Smoothly saturating at +/- 1.
fn soft_clamp(v: f32) -> f32 {
    v / (1.0 + v * v).sqrt()
}

/// The last event, in g: (x, y) in the screen's frame.
fn read() -> Option<(f32, f32)> {
    let text = fs::read_to_string(RAW).ok()?;
    let mut parts = text.trim().split(',').map(|v| v.trim().parse::<f32>());
    let x = parts.next()?.ok()?;
    let y = parts.next()?.ok()?;
    let z = parts.next()?.ok()?;
    // All zeros: no event yet since switching on.
    if x == 0.0 && y == 0.0 && z == 0.0 {
        return None;
    }
    Some((x / ONE_G, y / ONE_G))
}

fn accel_enabled() -> bool {
    fs::read_to_string(ENABLE)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .is_some_and(|mask| mask & (1 << ACCEL_TYPE) != 0)
}

fn write_enable(on: bool) {
    // The hub driver switches the sensor but reports EINVAL: ignore it.
    let command = format!("{}\n", ACCEL_TYPE * 10 + u32::from(on));
    if let Ok(mut file) = OpenOptions::new().write(true).open(ENABLE) {
        let _ = file.write(command.as_bytes());
    }
}

/// The hub's batch ioctl (light.rs `set_period`), for the accelerometer.
fn set_period(period: Duration) {
    #[repr(C)]
    struct BatchConfig {
        timeout: i64,
        delay: i64,
        flag: libc::c_int,
    }
    let Ok(file) = OpenOptions::new().read(true).open(BATCH) else {
        return;
    };
    let config = BatchConfig {
        timeout: 0,
        delay: period.as_nanos() as i64,
        flag: 0,
    };
    let request = (0xFC << 8) | ACCEL_TYPE;
    // SAFETY: the driver copies sizeof(struct batch_config) bytes from this
    // live, fully initialized value; the fd is valid for the call.
    unsafe {
        libc::ioctl(
            file.as_raw_fd(),
            request as _,
            &config as *const BatchConfig,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_clamp_saturates() {
        assert_eq!(soft_clamp(0.0), 0.0);
        assert!(soft_clamp(100.0) < 1.0 && soft_clamp(100.0) > 0.99);
        assert!((soft_clamp(-0.1) + 0.0995).abs() < 1e-3);
    }

    #[test]
    fn the_shown_offset_eases_toward_the_target() {
        let start = Instant::now();
        retarget((0.0, 0.0), start);
        retarget((1.0, -0.5), start);
        let early = offset_at(start + Duration::from_millis(30));
        let later = offset_at(start + Duration::from_millis(200));
        assert!(early.0 > 0.0 && early.0 < later.0 && later.0 < 1.0);
        assert!(moving(start + Duration::from_millis(30)));
        let settled = offset_at(start + Duration::from_secs(3));
        assert!((settled.0 - 1.0).abs() < 1e-3 && (settled.1 + 0.5).abs() < 1e-3);
        assert!(!moving(start + Duration::from_secs(3)));
        reset();
        assert_eq!(offset_at(start), (0.0, 0.0));
    }
}
