//! The ambient light sensor through Samsung's sensor hub (`shub`).
//!
//! The hub exposes each sensor as an IIO device (`light_sensor`); a sensor is
//! switched on and off by writing `type * 10 + enable` to
//! /sys/class/sensors/ssp_sensor/enable (light is type 5) and its rate is set
//! with the hub's batch ioctl on /dev/batch_io. Every IIO record starts with
//! the calibrated lux (u32, little endian), followed by colour channels and a
//! 64-bit timestamp. The sensor runs only while the screen is on.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

const LIGHT_TYPE: u32 = 5;
const ENABLE: &str = "/sys/class/sensors/ssp_sensor/enable";
const BATCH: &str = "/dev/batch_io";
const IIO_SYSFS: &str = "/sys/bus/iio/devices";
const IIO_NAME: &str = "light_sensor";
/// 200 ms between readings: responsive, and ~5 wakeups/s only while the
/// screen is on.
const PERIOD_NS: i64 = 200_000_000;

pub struct LightSensor {
    device: PathBuf,
    sysfs: PathBuf,
    record: usize,
    file: Option<File>,
    pending: Vec<u8>,
}

impl LightSensor {
    /// Finds the hub's light sensor; None when this kernel has none.
    pub fn find() -> Option<Self> {
        let entries = fs::read_dir(IIO_SYSFS).ok()?;
        for entry in entries.flatten() {
            let sysfs = entry.path();
            let name = fs::read_to_string(sysfs.join("name")).unwrap_or_default();
            if name.trim() != IIO_NAME {
                continue;
            }
            let node = entry.file_name();
            let record = record_size(&sysfs).unwrap_or(48);
            return Some(Self {
                device: PathBuf::from("/dev").join(node),
                sysfs,
                record,
                file: None,
                pending: Vec::new(),
            });
        }
        None
    }

    pub fn raw_fd(&self) -> Option<RawFd> {
        self.file.as_ref().map(AsRawFd::as_raw_fd)
    }

    /// Starts the sensor and opens its data. Errors leave it stopped.
    pub fn start(&mut self) -> io::Result<()> {
        if self.file.is_some() {
            return Ok(());
        }
        let buffer = self.sysfs.join("buffer/enable");
        if fs::read_to_string(&buffer).is_ok_and(|v| v.trim() == "0") {
            // Enable every scan element, then the buffer (the hub driver
            // normally leaves it enabled).
            if let Ok(elements) = fs::read_dir(self.sysfs.join("scan_elements")) {
                for element in elements.flatten() {
                    let path = element.path();
                    if path.to_string_lossy().ends_with("_en") {
                        let _ = fs::write(&path, "1");
                    }
                }
            }
            let _ = fs::write(&buffer, "1");
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(&self.device)?;
        set_period(PERIOD_NS);
        fs::write(ENABLE, format!("{}\n", LIGHT_TYPE * 10 + 1))?;
        self.pending.clear();
        self.file = Some(file);
        Ok(())
    }

    pub fn stop(&mut self) {
        if self.file.take().is_some() {
            let _ = fs::write(ENABLE, format!("{}\n", LIGHT_TYPE * 10));
        }
        self.pending.clear();
    }

    /// Drains queued records; the newest lux, if any arrived.
    pub fn read(&mut self) -> Option<u32> {
        let file = self.file.as_mut()?;
        let mut buffer = [0u8; 1024];
        let mut newest = None;
        loop {
            match file.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    self.pending.extend_from_slice(&buffer[..count]);
                    while self.pending.len() >= self.record {
                        let record: Vec<u8> = self.pending.drain(..self.record).collect();
                        newest = Some(u32::from_le_bytes([
                            record[0], record[1], record[2], record[3],
                        ]));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        newest
    }
}

impl Drop for LightSensor {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Bytes per IIO record: the data channel (storagebits x repeat) plus the
/// 8-byte timestamp, aligned to 8 as IIO lays them out.
fn record_size(sysfs: &std::path::Path) -> Option<usize> {
    let elements = fs::read_dir(sysfs.join("scan_elements")).ok()?;
    let mut data = 0;
    let mut timestamp = false;
    for element in elements.flatten() {
        let name = element.file_name().to_string_lossy().into_owned();
        if !name.ends_with("_type") {
            continue;
        }
        if name.contains("timestamp") {
            timestamp = true;
            continue;
        }
        data += parse_type_bytes(&fs::read_to_string(element.path()).ok()?)?;
    }
    if data == 0 {
        return None;
    }
    Some(if timestamp {
        data.div_ceil(8) * 8 + 8
    } else {
        data
    })
}

/// `le:s<real>/<storage>[X<repeat>]>><shift>` -> storage/8 * repeat bytes.
fn parse_type_bytes(text: &str) -> Option<usize> {
    let text = text.trim();
    let after_slash = text.split_once('/')?.1;
    let (storage, rest) = match after_slash.split_once('X') {
        Some((storage, rest)) => (storage, rest.split(">>").next()?),
        None => (after_slash.split(">>").next()?, "1"),
    };
    let storage: usize = storage.parse().ok()?;
    let repeat: usize = rest.parse().ok()?;
    Some(storage / 8 * repeat)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iio_type_strings() {
        assert_eq!(parse_type_bytes("le:s8/8X44>>0\n"), Some(44));
        assert_eq!(parse_type_bytes("le:s32/32>>0"), Some(4));
        assert_eq!(parse_type_bytes("be:u16/16X3>>0"), Some(6));
        assert_eq!(parse_type_bytes("garbage"), None);
    }
}
