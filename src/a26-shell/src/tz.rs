//! Time zones Moon offers: IANA ids with their POSIX TZ rules, taken from
//! tzdata 2026b (the chroot ships no zoneinfo, and a POSIX rule is all the
//! C library needs). The System app's picker carries the same ids with
//! city names (a26-system `src/tz.rs`).

pub const ZONES: [(&str, &str); 88] = [
    ("UTC", "UTC0"),
    ("Pacific/Honolulu", "HST10"),
    ("America/Anchorage", "AKST9AKDT,M3.2.0,M11.1.0"),
    ("America/Los_Angeles", "PST8PDT,M3.2.0,M11.1.0"),
    ("America/Vancouver", "MST7"),
    ("America/Tijuana", "PST8PDT,M3.2.0,M11.1.0"),
    ("America/Phoenix", "MST7"),
    ("America/Denver", "MST7MDT,M3.2.0,M11.1.0"),
    ("America/Edmonton", "MST7MDT,M3.2.0,M11.1.0"),
    ("America/Chicago", "CST6CDT,M3.2.0,M11.1.0"),
    ("America/Winnipeg", "CST6CDT,M3.2.0,M11.1.0"),
    ("America/Regina", "CST6"),
    ("America/Mexico_City", "CST6"),
    ("America/Guatemala", "CST6"),
    ("America/El_Salvador", "CST6"),
    ("America/Costa_Rica", "CST6"),
    ("America/New_York", "EST5EDT,M3.2.0,M11.1.0"),
    ("America/Toronto", "EST5EDT,M3.2.0,M11.1.0"),
    ("America/Havana", "CST5CDT,M3.2.0/0,M11.1.0/1"),
    ("America/Jamaica", "EST5"),
    ("America/Panama", "EST5"),
    ("America/Bogota", "<-05>5"),
    ("America/Lima", "<-05>5"),
    ("America/Guayaquil", "<-05>5"),
    ("America/Halifax", "AST4ADT,M3.2.0,M11.1.0"),
    ("America/Puerto_Rico", "AST4"),
    ("America/Santo_Domingo", "AST4"),
    ("America/Caracas", "<-04>4"),
    ("America/La_Paz", "<-04>4"),
    ("America/Santiago", "<-04>4<-03>,M9.1.6/24,M4.1.6/24"),
    ("America/St_Johns", "NST3:30NDT,M3.2.0,M11.1.0"),
    ("America/Sao_Paulo", "<-03>3"),
    ("America/Argentina/Buenos_Aires", "<-03>3"),
    ("America/Montevideo", "<-03>3"),
    ("Atlantic/Azores", "<-01>1<+00>,M3.5.0/0,M10.5.0/1"),
    ("Europe/London", "GMT0BST,M3.5.0/1,M10.5.0"),
    ("Europe/Dublin", "IST-1GMT0,M10.5.0,M3.5.0/1"),
    ("Europe/Lisbon", "WET0WEST,M3.5.0/1,M10.5.0"),
    ("Africa/Casablanca", "<+01>-1"),
    ("Africa/Lagos", "WAT-1"),
    ("Europe/Madrid", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Paris", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Brussels", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Amsterdam", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Berlin", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Zurich", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Rome", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Vienna", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Prague", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Warsaw", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Stockholm", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Oslo", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Copenhagen", "CET-1CEST,M3.5.0,M10.5.0/3"),
    ("Europe/Athens", "EET-2EEST,M3.5.0/3,M10.5.0/4"),
    ("Europe/Helsinki", "EET-2EEST,M3.5.0/3,M10.5.0/4"),
    ("Europe/Kyiv", "EET-2EEST,M3.5.0/3,M10.5.0/4"),
    ("Europe/Bucharest", "EET-2EEST,M3.5.0/3,M10.5.0/4"),
    ("Africa/Cairo", "EET-2EEST,M4.5.5/0,M10.5.4/24"),
    ("Africa/Johannesburg", "SAST-2"),
    ("Asia/Jerusalem", "IST-2IDT,M3.4.4/26,M10.5.0"),
    ("Europe/Istanbul", "<+03>-3"),
    ("Europe/Moscow", "MSK-3"),
    ("Asia/Riyadh", "<+03>-3"),
    ("Africa/Nairobi", "EAT-3"),
    ("Asia/Tehran", "<+0330>-3:30"),
    ("Asia/Dubai", "<+04>-4"),
    ("Asia/Karachi", "PKT-5"),
    ("Asia/Kolkata", "IST-5:30"),
    ("Asia/Kathmandu", "<+0545>-5:45"),
    ("Asia/Dhaka", "<+06>-6"),
    ("Asia/Bangkok", "<+07>-7"),
    ("Asia/Jakarta", "WIB-7"),
    ("Asia/Ho_Chi_Minh", "<+07>-7"),
    ("Asia/Singapore", "<+08>-8"),
    ("Asia/Kuala_Lumpur", "<+08>-8"),
    ("Asia/Hong_Kong", "HKT-8"),
    ("Asia/Shanghai", "CST-8"),
    ("Asia/Taipei", "CST-8"),
    ("Asia/Manila", "PST-8"),
    ("Australia/Perth", "AWST-8"),
    ("Asia/Seoul", "KST-9"),
    ("Asia/Tokyo", "JST-9"),
    ("Australia/Darwin", "ACST-9:30"),
    ("Australia/Adelaide", "ACST-9:30ACDT,M10.1.0,M4.1.0/3"),
    ("Australia/Brisbane", "AEST-10"),
    ("Australia/Sydney", "AEST-10AEDT,M10.1.0,M4.1.0/3"),
    ("Australia/Melbourne", "AEST-10AEDT,M10.1.0,M4.1.0/3"),
    ("Pacific/Auckland", "NZST-12NZDT,M9.5.0,M4.1.0/3"),
];

/// The POSIX rule for a zone id.
pub fn rule(id: &str) -> Option<&'static str> {
    ZONES
        .iter()
        .find(|(zone, _)| *zone == id)
        .map(|(_, rule)| *rule)
}

/// Makes `rule` the local time of Moon and of the apps it starts from now
/// on (running apps follow through Moon's state).
pub fn apply(rule: &str) {
    // SAFETY: Moon's event loop is the only thread that reads the
    // environment or the C library's time zone; nothing else runs while
    // this changes them.
    unsafe {
        std::env::set_var("TZ", rule);
        tzset();
    }
}

unsafe extern "C" {
    fn tzset();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zones_have_rules() {
        assert_eq!(rule("America/Toronto"), Some("EST5EDT,M3.2.0,M11.1.0"));
        assert_eq!(rule("Nowhere/Else"), None);
        let mut ids: Vec<_> = ZONES.iter().map(|(id, _)| *id).collect();
        ids.dedup();
        assert_eq!(ids.len(), ZONES.len());
    }
}
