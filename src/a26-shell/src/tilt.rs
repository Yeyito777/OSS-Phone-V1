//! The phone's tilt, for wallpapers with depth (wallpaper.rs "orbit").
//!
//! The accelerometer is on Samsung's sensor hub, like the light sensor
//! (light.rs): switched with /sys/class/sensors/ssp_sensor/enable (type 1,
//! the hub counts references) and read from the driver's last event in
//! /sys/class/sensors/accelerometer_sensor/raw_data (`x,y,z`, 4096 = 1 g).
//! It runs only while a dynamic wallpaper is on screen with the screen on.
//!
//! The offset is the tilt away from how the phone has been held lately (a
//! slowly re-centring baseline), so any comfortable pose looks centred and
//! only moving the phone moves the layers. A still phone redraws nothing.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const ACCEL_TYPE: u32 = 1;
const ENABLE: &str = "/sys/class/sensors/ssp_sensor/enable";
const RAW: &str = "/sys/class/sensors/accelerometer_sensor/raw_data";
const BATCH: &str = "/dev/batch_io";
const ONE_G: f32 = 4096.0;
const PERIOD: Duration = Duration::from_millis(20);
/// Smoothing of the readings, and how fast the neutral pose follows.
const SMOOTH_S: f32 = 0.07;
const RECENTRE_S: f32 = 2.5;
/// Unit offset (+/- 1) per this much tilt, in g.
const RANGE_G: f32 = 0.30;
/// Offset change (in units) worth a frame: about a third of a pixel at
/// the nearest layer's travel.
const STEP: f32 = 0.006;

/// The offset shown, -1..1 on each axis (f32 bits): x positive when the
/// right edge dips, y positive when the top edge dips.
static OFFSET: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

pub fn offset() -> (f32, f32) {
    (
        f32::from_bits(OFFSET[0].load(Ordering::Relaxed)),
        f32::from_bits(OFFSET[1].load(Ordering::Relaxed)),
    )
}

fn set_offset(x: f32, y: f32) {
    OFFSET[0].store(x.to_bits(), Ordering::Relaxed);
    OFFSET[1].store(y.to_bits(), Ordering::Relaxed);
}

#[derive(Default)]
pub struct Tilt {
    on: bool,
    normalized: bool,
    last: Option<Instant>,
    smooth: (f32, f32),
    neutral: (f32, f32),
    shown: (f32, f32),
}

impl Tilt {
    /// Follows the sensor while `active`; true when the offset moved enough
    /// to be worth a frame.
    pub fn update(&mut self, active: bool, now: Instant) -> bool {
        if !active {
            if self.on {
                write_enable(false);
                self.on = false;
                self.last = None;
                // Back to centre for next time (the next frame is not
                // drawn with a dynamic wallpaper on screen anyway).
                self.shown = (0.0, 0.0);
                set_offset(0.0, 0.0);
            }
            return false;
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
        if self.last.is_some_and(|last| now < last + PERIOD) {
            return false;
        }
        let Some((x, y)) = read() else {
            return false;
        };
        let Some(last) = self.last.replace(now) else {
            self.smooth = (x, y);
            self.neutral = (x, y);
            return false;
        };
        let dt = now.saturating_duration_since(last).as_secs_f32().min(0.5);
        let a = 1.0 - (-dt / SMOOTH_S).exp();
        self.smooth.0 += (x - self.smooth.0) * a;
        self.smooth.1 += (y - self.smooth.1) * a;
        let b = 1.0 - (-dt / RECENTRE_S).exp();
        self.neutral.0 += (self.smooth.0 - self.neutral.0) * b;
        self.neutral.1 += (self.smooth.1 - self.neutral.1) * b;
        let target = (
            soft_clamp((self.smooth.0 - self.neutral.0) / RANGE_G),
            soft_clamp((self.smooth.1 - self.neutral.1) / RANGE_G),
        );
        if (target.0 - self.shown.0).abs() < STEP && (target.1 - self.shown.1).abs() < STEP {
            return false;
        }
        self.shown = target;
        set_offset(target.0, target.1);
        true
    }

    /// When to sample next.
    pub fn next_deadline(&self, now: Instant) -> Option<Instant> {
        self.on
            .then(|| self.last.map_or(now, |last| (last + PERIOD).max(now)))
    }
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
}
