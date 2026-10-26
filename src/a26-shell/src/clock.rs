//! Wall-clock time for the status bar, lock screen, and home widgets.
//!
//! Local time comes from the C library (`TZ` or `/etc/localtime` inside the
//! Alpine chroot; UTC when neither exists). Moon only redraws when the
//! displayed minute changes, so the clock adds one wakeup per minute while
//! the screen is on and none while it is off.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub hour: u8,
    pub minute: u8,
    /// 0 = Sunday.
    pub weekday: u8,
    pub day: u8,
    /// 1 = January.
    pub month: u8,
}

const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

impl LocalTime {
    pub fn now() -> Self {
        Self::at(unix_now())
    }

    /// The local time of a Unix timestamp (seconds).
    pub fn at(seconds: i64) -> Self {
        // time_t is 64-bit on every Moon target (aarch64 musl, x86-64).
        #[allow(deprecated)]
        let seconds = seconds as libc::time_t;
        // SAFETY: `tm` is plain data; localtime_r only writes through the
        // valid pointers given and is thread-safe.
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let ok = unsafe { !libc::localtime_r(&seconds, &mut tm).is_null() };
        if !ok {
            return Self {
                hour: 0,
                minute: 0,
                weekday: 4,
                day: 1,
                month: 1,
            };
        }
        Self {
            hour: tm.tm_hour.clamp(0, 23) as u8,
            minute: tm.tm_min.clamp(0, 59) as u8,
            weekday: tm.tm_wday.clamp(0, 6) as u8,
            day: tm.tm_mday.clamp(1, 31) as u8,
            month: (tm.tm_mon + 1).clamp(1, 12) as u8,
        }
    }

    /// "9:41" in 12-hour mode (no AM/PM, as on iPhone) or "21:41".
    pub fn time_text(self, twenty_four_hour: bool) -> String {
        if twenty_four_hour {
            format!("{:02}:{:02}", self.hour, self.minute)
        } else {
            let hour = match self.hour % 12 {
                0 => 12,
                hour => hour,
            };
            format!("{hour}:{:02}", self.minute)
        }
    }

    /// "Sunday, October 25".
    pub fn date_text(self) -> String {
        format!(
            "{}, {} {}",
            self.weekday_name(),
            MONTHS[usize::from(self.month - 1)],
            self.day
        )
    }

    pub fn weekday_name(self) -> &'static str {
        WEEKDAYS[usize::from(self.weekday)]
    }

    pub fn month_name(self) -> &'static str {
        MONTHS[usize::from(self.month - 1)]
    }
}

/// Wall-clock Unix seconds.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The monotonic instant at which the wall-clock minute next changes.
pub fn next_minute_boundary(now: Instant) -> Instant {
    let into_minute = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() % 60_000)
        .unwrap_or(0) as u64;
    // A few milliseconds late guarantees the new minute is visible.
    now + Duration::from_millis(60_000 - into_minute + 20)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(hour: u8, minute: u8) -> LocalTime {
        LocalTime {
            hour,
            minute,
            weekday: 0,
            day: 25,
            month: 10,
        }
    }

    #[test]
    fn twelve_and_twenty_four_hour_formats() {
        assert_eq!(at(0, 5).time_text(false), "12:05");
        assert_eq!(at(9, 41).time_text(false), "9:41");
        assert_eq!(at(13, 0).time_text(false), "1:00");
        assert_eq!(at(13, 0).time_text(true), "13:00");
        assert_eq!(at(0, 5).time_text(true), "00:05");
    }

    #[test]
    fn date_is_spelled_out() {
        assert_eq!(at(9, 41).date_text(), "Sunday, October 25");
    }

    #[test]
    fn minute_boundary_is_within_a_minute() {
        let now = Instant::now();
        let next = next_minute_boundary(now);
        assert!(next > now && next <= now + Duration::from_millis(60_020));
    }
}
