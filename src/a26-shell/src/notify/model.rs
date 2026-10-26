//! moon-notifyd's persistent objects and their validation
//! (docs/notifications-protocol.md §2).

use serde::{Deserialize, Serialize};

pub const MAX_NOTIFICATIONS_PER_APP: usize = 50;
pub const MAX_NOTIFICATIONS: usize = 200;
pub const MAX_ALARMS: usize = 32;
pub const MAX_TIMERS: usize = 16;
pub const MAX_TIMER_SECONDS: i64 = 24 * 3600;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Urgency {
    Low,
    Normal,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sound {
    Radar,
    Chimes,
    Beacon,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Open {
    pub app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Notification {
    pub id: String,
    pub app: String,
    pub tag: Option<String>,
    pub title: String,
    pub body: Option<String>,
    pub urgency: Urgency,
    pub silent: bool,
    pub posted_at: i64,
    pub expires_at: Option<i64>,
    pub seen: bool,
    pub open: Option<Open>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Alarm {
    pub id: String,
    pub hour: u8,
    pub minute: u8,
    pub days: Vec<u8>,
    pub label: String,
    pub enabled: bool,
    pub sound: Sound,
    pub vibrate: bool,
    pub snooze_minutes: u8,
    /// Computed; null when disabled.
    pub next_fire_at: Option<i64>,
    pub snoozed_until: Option<i64>,
    pub ringing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimerState {
    Running,
    Paused,
    Ringing,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Timer {
    pub id: String,
    pub label: String,
    pub duration_s: i64,
    pub state: TimerState,
    pub ends_at: Option<i64>,
    pub remaining_s: i64,
    pub sound: Sound,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub alert_volume: u8,
    pub vibrate: bool,
    pub notification_sounds: bool,
    pub do_not_disturb: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            alert_volume: 70,
            vibrate: true,
            notification_sounds: true,
            do_not_disturb: false,
        }
    }
}

/// Everything persisted in /var/lib/moon-notify/state.json.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub version: u32,
    pub next_id: u64,
    pub notifications: Vec<Notification>,
    pub alarms: Vec<Alarm>,
    pub timers: Vec<Timer>,
    pub settings: Settings,
}

impl State {
    pub fn new_id(&mut self, prefix: char) -> String {
        self.next_id += 1;
        format!("{prefix}{}", self.next_id)
    }
}

pub fn valid_app(app: &str) -> bool {
    (1..=32).contains(&app.len())
        && app
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn valid_tag(tag: &str) -> bool {
    (1..=64).contains(&tag.len())
        && tag
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Trims and bounds user-visible text; `None` when it is empty after trimming
/// or longer than `max` characters.
pub fn clean_text(text: &str, max: usize) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty() && trimmed.chars().count() <= max).then(|| trimmed.to_owned())
}

/// Sorted, unique weekdays 0..=6.
pub fn clean_days(days: &[u8]) -> Option<Vec<u8>> {
    if days.iter().any(|day| *day > 6) {
        return None;
    }
    let mut days = days.to_vec();
    days.sort_unstable();
    days.dedup();
    Some(days)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_and_text_are_bounded() {
        assert!(valid_app("system"));
        assert!(!valid_app("System"));
        assert!(!valid_app(""));
        assert!(valid_tag("wifi-state.v2"));
        assert!(!valid_tag("has space"));
        assert_eq!(clean_text("  hi ", 10).as_deref(), Some("hi"));
        assert_eq!(clean_text("   ", 10), None);
        assert_eq!(clean_text("abcdef", 5), None);
        assert_eq!(clean_days(&[5, 1, 1, 3]), Some(vec![1, 3, 5]));
        assert_eq!(clean_days(&[7]), None);
    }

    #[test]
    fn state_round_trips_and_tolerates_missing_fields() {
        let mut state = State::default();
        state.alarms.push(Alarm {
            id: "a1".into(),
            hour: 7,
            minute: 30,
            days: vec![1, 2],
            label: "x".into(),
            enabled: true,
            sound: Sound::Radar,
            vibrate: true,
            snooze_minutes: 9,
            next_fire_at: Some(10),
            snoozed_until: None,
            ringing: false,
        });
        let text = serde_json::to_string(&state).unwrap();
        assert_eq!(serde_json::from_str::<State>(&text).unwrap(), state);
        let empty: State = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.settings.alert_volume, 70);
    }
}
