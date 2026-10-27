//! The ambient light sensor through Samsung's sensor hub (`shub`), without
//! IIO (docs/auto-brightness-compensation.md §1).
//!
//! Android's sensors HAL keeps every hub IIO device open even in Moon mode,
//! and an IIO device opens only once, so Moon reads the hub's lux from the
//! kernel log instead:
//!
//! - the driver logs the first three light events after the sensor is
//!   switched on (`[SHUB] Light Sensor : lux=N …`), within ~0.1 s: the wake
//!   reading;
//! - while any hub sensor runs, the hub's debug work logs the last light
//!   event about every 6 s (`[SHUB] light_sensor(5) : N(TIMESTAMP_NS) …`).
//!
//! The sensor is switched with /sys/class/sensors/ssp_sensor/enable
//! (`type * 10 + on`, light is type 5; the hub counts references, so this
//! coexists with the HAL) and runs only while the screen is on. The lux is
//! the hub's own, which includes the panel's light over the sensor; the
//! brightness backend compensates for it (`compensation.rs`).

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const LIGHT_TYPE: u32 = 5;
const ENABLE: &str = "/sys/class/sensors/ssp_sensor/enable";
const KMSG: &str = "/dev/kmsg";
const BATCH: &str = "/dev/batch_io";
/// The hub's sampling period while on (it reports on change).
const PERIOD_NS: i64 = 200_000_000;

/// One lux value from the hub.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LightReading {
    pub lux: u32,
    /// When the hub measured it (CLOCK_BOOTTIME ns): the event's timestamp
    /// for the periodic line, the log record's time for the first events.
    pub at_ns: u64,
    /// One of the events logged right after the sensor was switched on.
    pub first: bool,
}

pub struct LightSensor {
    kmsg: Option<File>,
    normalized: bool,
}

impl LightSensor {
    /// The hub's light sensor; None when this kernel has none.
    pub fn find() -> Option<Self> {
        (Path::new(ENABLE).exists() && Path::new("/sys/class/sensors/light_sensor").exists())
            .then_some(Self {
                kmsg: None,
                normalized: false,
            })
    }

    pub fn raw_fd(&self) -> Option<RawFd> {
        self.kmsg.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// Switches the sensor on and follows the kernel log from now.
    pub fn start(&mut self) -> io::Result<()> {
        if self.kmsg.is_some() {
            return Ok(());
        }
        if !self.normalized {
            // A Moon that died with the sensor on left a reference behind:
            // without dropping it, switching on would not be the first
            // reference and the hub would not log the first events.
            for _ in 0..4 {
                if !light_enabled() {
                    break;
                }
                let _ = write_enable(false);
            }
            self.normalized = true;
        }
        let mut kmsg = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(KMSG)?;
        kmsg.seek(SeekFrom::End(0))?;
        set_period(PERIOD_NS);
        write_enable(true)?;
        self.kmsg = Some(kmsg);
        Ok(())
    }

    /// Switches the sensor off and on again, so the hub logs fresh first
    /// events (about 0.1 s later): a reading on demand.
    pub fn retrigger(&mut self) -> io::Result<()> {
        if self.kmsg.is_none() {
            return Err(io::Error::from(io::ErrorKind::NotConnected));
        }
        write_enable(false)?;
        write_enable(true)
    }

    pub fn stop(&mut self) {
        if self.kmsg.take().is_some() {
            let _ = write_enable(false);
        }
    }

    /// The readings logged since the last call.
    pub fn read(&mut self) -> Vec<LightReading> {
        let mut readings = Vec::new();
        let Some(kmsg) = self.kmsg.as_mut() else {
            return readings;
        };
        // One record per read(2); EAGAIN when drained, EPIPE when records
        // were overwritten before we read them (then the next one follows).
        let mut buffer = [0u8; 2048];
        for _ in 0..512 {
            match kmsg.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if let Some(reading) = std::str::from_utf8(&buffer[..count])
                        .ok()
                        .and_then(parse_record)
                    {
                        readings.push(reading);
                    }
                }
                Err(error) if error.raw_os_error() == Some(libc::EPIPE) => continue,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        readings
    }
}

impl Drop for LightSensor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Is the light sensor on (bit 5 of the hub's enabled-sensor mask)?
fn light_enabled() -> bool {
    fs::read_to_string(ENABLE)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .is_some_and(|mask| mask & (1 << LIGHT_TYPE) != 0)
}

/// Switches the light sensor on or off. One write(2): the node returns more
/// than was written (64 for "51\n"), which `write_all` (`fs::write`) would
/// treat as a slice overrun and panic on.
fn write_enable(on: bool) -> io::Result<()> {
    let command = format!("{}\n", LIGHT_TYPE * 10 + u32::from(on));
    OpenOptions::new()
        .write(true)
        .open(ENABLE)?
        .write(command.as_bytes())
        .map(drop)
}

/// A /dev/kmsg record (`PRI,SEQ,TS_US,FLAGS;MESSAGE\n…`) -> a light reading.
fn parse_record(record: &str) -> Option<LightReading> {
    let (header, message) = record.split_once(';')?;
    let message = message.lines().next()?;
    let record_us: u64 = header.split(',').nth(2)?.parse().ok()?;
    if let Some(rest) = message.split("[SHUB] Light Sensor : lux=").nth(1) {
        let lux = rest.split_whitespace().next()?.parse().ok()?;
        return Some(LightReading {
            lux,
            at_ns: record_us * 1000,
            first: true,
        });
    }
    let rest = message.split("[SHUB] light_sensor(5) : ").nth(1)?;
    let (lux, rest) = rest.split_once('(')?;
    let at_ns = rest.split(')').next()?.trim().parse().ok()?;
    Some(LightReading {
        lux: lux.trim().parse().ok()?,
        at_ns,
        first: false,
    })
}

/// The hub's batch ioctl: (0xFC << 8) | sensor type, with
/// struct batch_config { s64 timeout; s64 delay; int flag; }.
fn set_period(period_ns: i64) {
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
        delay: period_ns,
        flag: 0,
    };
    let request = (0xFC << 8) | LIGHT_TYPE;
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

/// CLOCK_BOOTTIME in ns: hub event timestamps, and kernel log record times
/// while the phone has not suspended (Moon mode does not, short of the
/// opt-in deep suspend, which ends in a reboot).
pub fn boottime_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: clock_gettime writes one timespec into a valid pointer.
    unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_log_light_lines() {
        assert_eq!(
            parse_record(
                "6,4012,198930292,-;[SHUB] Light Sensor : lux=28 brightness=0 r=563 g=116 b=0 c=0 atime=0 again=0\n"
            ),
            Some(LightReading {
                lux: 28,
                at_ns: 198_930_292_000,
                first: true
            })
        );
        assert_eq!(
            parse_record(
                "6,5000,1132009327,-;[SHUB] light_sensor(5) : 19(1121814710029) (0ms, 0ms)\n SUBSYSTEM=x\n"
            ),
            Some(LightReading {
                lux: 19,
                at_ns: 1_121_814_710_029,
                first: false
            })
        );
        assert_eq!(
            parse_record("6,1,2,-;[SHUB] accelerometer_sensor(1) : 3(4) (0ms, 0ms)\n"),
            None
        );
        assert_eq!(parse_record("garbage"), None);
    }
}
