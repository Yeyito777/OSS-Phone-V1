//! Local wall-clock alarm times (docs/notifications-protocol.md §2, Alarm).
//!
//! Uses the C library's `TZ`-aware `mktime`/`localtime_r`; the daemon sets
//! `TZ` from Moon's config before anything here runs.

/// The first moment strictly after `after` (Unix seconds) at which an alarm
/// set to `hour:minute` local time on `days` (0 = Sunday; empty = any day)
/// fires. A time inside a spring-forward gap fires at the first valid minute
/// after it; an ambiguous fall-back time fires on its first occurrence.
pub fn next_fire(hour: u8, minute: u8, days: &[u8], after: i64) -> Option<i64> {
    let today = local(after)?;
    for offset in 0..=8 {
        let weekday = local_weekday_of_date(&today, offset)?;
        if !days.is_empty() && !days.contains(&weekday) {
            continue;
        }
        let mut tm = blank_tm();
        tm.tm_year = today.tm_year;
        tm.tm_mon = today.tm_mon;
        tm.tm_mday = today.tm_mday + offset;
        tm.tm_hour = i32::from(hour);
        tm.tm_min = i32::from(minute);
        tm.tm_isdst = -1;
        // SAFETY: tm is a complete, initialized struct tm owned here.
        let candidate = unsafe { libc::mktime(&mut tm) };
        if candidate == -1 {
            continue;
        }
        let candidate = first_occurrence(out_of_gap(i64::from(candidate), hour, minute));
        if candidate > after {
            return Some(candidate);
        }
    }
    None
}

/// A wall time inside a spring-forward gap does not exist; libcs disagree on
/// where mktime puts it (glibc after the gap, musl before it). It fires
/// shifted forward by the jump, e.g. 02:30 -> 03:30.
fn out_of_gap(candidate: i64, hour: u8, minute: u8) -> i64 {
    let requested = i32::from(hour) * 60 + i32::from(minute);
    let minutes = |t: i64| local(t).map(|tm| tm.tm_hour * 60 + tm.tm_min);
    if minutes(candidate) == Some(requested) {
        return candidate;
    }
    let shifted = (requested + 60) % 1440;
    [candidate, candidate + 3600, candidate - 3600]
        .into_iter()
        .find(|t| minutes(*t) == Some(shifted))
        .unwrap_or(candidate)
}

/// For an ambiguous local time (fall-back hour), mktime may pick either
/// occurrence; the alarm fires on the first.
fn first_occurrence(candidate: i64) -> i64 {
    let earlier = candidate - 3600;
    match (local(candidate), local(earlier)) {
        (Some(a), Some(b))
            if a.tm_hour == b.tm_hour && a.tm_min == b.tm_min && a.tm_mday == b.tm_mday =>
        {
            earlier
        }
        _ => candidate,
    }
}

fn local_weekday_of_date(today: &libc::tm, offset: i32) -> Option<u8> {
    let mut noon = blank_tm();
    noon.tm_year = today.tm_year;
    noon.tm_mon = today.tm_mon;
    noon.tm_mday = today.tm_mday + offset;
    noon.tm_hour = 12;
    noon.tm_isdst = -1;
    // SAFETY: as above.
    if unsafe { libc::mktime(&mut noon) } == -1 {
        return None;
    }
    u8::try_from(noon.tm_wday).ok()
}

pub fn local(seconds: i64) -> Option<libc::tm> {
    let time = seconds as libc::time_t;
    let mut tm = blank_tm();
    // SAFETY: localtime_r writes only into the provided tm.
    let ok = unsafe { !libc::localtime_r(&time, &mut tm).is_null() };
    ok.then_some(tm)
}

fn blank_tm() -> libc::tm {
    // SAFETY: struct tm is plain data; all-zero is a valid value.
    unsafe { std::mem::zeroed() }
}

/// "07:30" in local time, for notification text.
pub fn format_hm(seconds: i64) -> String {
    local(seconds).map_or_else(
        || "--:--".to_owned(),
        |tm| format!("{}:{:02}", tm.tm_hour, tm.tm_min),
    )
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use std::sync::Mutex;

    /// TZ is process-global; tests that set it take this lock.
    pub static TZ_LOCK: Mutex<()> = Mutex::new(());

    pub fn with_tz<T>(zone: &str, body: impl FnOnce() -> T) -> T {
        let _guard = TZ_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialized by TZ_LOCK; tests touch TZ only under it.
        unsafe {
            std::env::set_var("TZ", zone);
            tzset();
        }
        body()
    }

    unsafe extern "C" {
        fn tzset();
    }

    const TORONTO: &str = "EST5EDT,M3.2.0,M11.1.0";

    fn at(y: i32, mo: i32, d: i32, h: i32, mi: i32) -> i64 {
        let mut tm = blank_tm();
        tm.tm_year = y - 1900;
        tm.tm_mon = mo - 1;
        tm.tm_mday = d;
        tm.tm_hour = h;
        tm.tm_min = mi;
        tm.tm_isdst = -1;
        i64::from(unsafe { libc::mktime(&mut tm) })
    }

    #[test]
    fn daily_weekly_and_dst_cases() {
        with_tz(TORONTO, || {
            // Sunday 2026-10-25 08:00 local; one-shot 07:30 is tomorrow.
            let now = at(2026, 10, 25, 8, 0);
            assert_eq!(next_fire(7, 30, &[], now), Some(at(2026, 10, 26, 7, 30)));
            // Later today.
            assert_eq!(next_fire(9, 0, &[], now), Some(at(2026, 10, 25, 9, 0)));
            // Exactly now is not "after".
            assert_eq!(next_fire(8, 0, &[], now), Some(at(2026, 10, 26, 8, 0)));
            // Weekdays only from Saturday 2026-10-31: Monday 2026-11-02.
            let saturday = at(2026, 10, 31, 12, 0);
            assert_eq!(
                next_fire(7, 30, &[1, 2, 3, 4, 5], saturday),
                Some(at(2026, 11, 2, 7, 30))
            );
            // Fall back 2026-11-01 01:30 happens twice: the first (EDT) one.
            let before = at(2026, 10, 31, 23, 0);
            let first = next_fire(1, 30, &[], before).unwrap();
            assert_eq!(format_hm(first), "1:30");
            assert_eq!(format_hm(first + 3600), "1:30");
            // Spring forward 2027-03-14 02:30 does not exist: fires at 03:30.
            let before = at(2027, 3, 13, 23, 0);
            let shifted = next_fire(2, 30, &[], before).unwrap();
            assert_eq!(format_hm(shifted), "3:30");
            assert!(shifted - before < 6 * 3600);
        });
    }
}
