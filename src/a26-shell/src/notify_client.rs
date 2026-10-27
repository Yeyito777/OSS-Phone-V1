//! Moon's client for `moon-notifyd` (docs/notifications-protocol.md, v1),
//! over the JSON-lines transport in line_client.rs. After every (re)connect
//! Moon subscribes, and the daemon's `snapshot` replaces all of
//! `NotifyStore`.

use std::os::fd::RawFd;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::line_client::{Line, LineClient};

pub const DEFAULT_SOCKET: &str = "/run/moon-notify/notify.sock";
/// A request the daemon has not answered in this long is considered lost
/// (protocol §1 suggests 1 s); the connection is dropped and rebuilt.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Notification {
    pub id: String,
    pub app: String,
    #[serde(default)]
    pub tag: Option<String>,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default = "normal")]
    pub urgency: String,
    #[serde(default)]
    pub posted_at: i64,
    #[serde(default)]
    pub seen: bool,
    #[serde(default)]
    pub open: Option<OpenAction>,
    /// An incoming call ringing (revision 3): Moon's call screen shows it,
    /// not a banner.
    #[serde(default)]
    pub ringing: bool,
}

fn normal() -> String {
    "normal".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct OpenAction {
    pub app: String,
    #[serde(default)]
    pub target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Alarm {
    pub id: String,
    pub hour: u8,
    pub minute: u8,
    #[serde(default)]
    pub days: Vec<u8>,
    #[serde(default)]
    pub label: String,
    pub enabled: bool,
    #[serde(default)]
    pub next_fire_at: Option<i64>,
    #[serde(default)]
    pub snoozed_until: Option<i64>,
    #[serde(default)]
    pub snooze_minutes: u8,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Timer {
    pub id: String,
    #[serde(default)]
    pub label: String,
    pub duration_s: u32,
    pub state: String,
    #[serde(default)]
    pub ends_at: Option<i64>,
    #[serde(default)]
    pub remaining_s: u32,
}

/// The daemon's settings Moon shows (Do Not Disturb, in Control Centre).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct NotifySettings {
    #[serde(default)]
    pub do_not_disturb: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RingKind {
    Alarm,
    Timer,
}

/// What the ringing screen's buttons do: stop, or snooze an alarm / repeat
/// a timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RingAction {
    Stop,
    Again,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Ringing {
    pub kind: RingKind,
    pub id: String,
}

/// A daemon event (§5). Unknown events are ignored, as the protocol asks.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Snapshot {
        #[serde(default)]
        notifications: Vec<Notification>,
        #[serde(default)]
        alarms: Vec<Alarm>,
        #[serde(default)]
        timers: Vec<Timer>,
        #[serde(default)]
        ringing: Vec<Ringing>,
        #[serde(default)]
        settings: Option<NotifySettings>,
    },
    SettingsChanged {
        settings: NotifySettings,
    },
    NotificationPosted {
        notification: Notification,
        #[serde(default)]
        wake: bool,
    },
    NotificationRemoved {
        id: String,
    },
    NotificationsSeen {
        ids: Vec<String>,
    },
    AlarmChanged {
        alarm: Alarm,
    },
    AlarmDeleted {
        id: String,
    },
    AlarmRinging {
        alarm: Alarm,
    },
    AlarmEnded {
        id: String,
    },
    TimerChanged {
        timer: Timer,
    },
    TimerDeleted {
        id: String,
    },
    TimerRinging {
        timer: Timer,
    },
    TimerEnded {
        id: String,
    },
    #[serde(other)]
    Unknown,
}

/// What Moon knows of the daemon's state, kept current from events.
#[derive(Debug, Default)]
pub struct NotifyStore {
    /// Newest first.
    pub notifications: Vec<Notification>,
    pub alarms: Vec<Alarm>,
    pub timers: Vec<Timer>,
    pub ringing: Vec<Ringing>,
    pub connected: bool,
    pub do_not_disturb: bool,
}

impl NotifyStore {
    pub fn apply(&mut self, event: &Event) {
        match event.clone() {
            Event::Snapshot {
                notifications,
                alarms,
                timers,
                ringing,
                settings,
            } => {
                if let Some(settings) = settings {
                    self.do_not_disturb = settings.do_not_disturb;
                }
                self.notifications = notifications;
                self.notifications
                    .sort_by(|a, b| b.posted_at.cmp(&a.posted_at));
                self.alarms = alarms;
                self.timers = timers;
                self.ringing = ringing;
            }
            Event::NotificationPosted { notification, .. } => {
                self.notifications.retain(|n| n.id != notification.id);
                self.notifications.insert(0, notification);
            }
            Event::NotificationRemoved { id } => self.notifications.retain(|n| n.id != id),
            Event::NotificationsSeen { ids } => {
                for notification in &mut self.notifications {
                    if ids.contains(&notification.id) {
                        notification.seen = true;
                    }
                }
            }
            Event::AlarmChanged { alarm } => upsert(&mut self.alarms, alarm, |a| &a.id),
            Event::AlarmDeleted { id } => {
                self.alarms.retain(|a| a.id != id);
                self.ringing.retain(|r| r.id != id);
            }
            Event::AlarmRinging { alarm } => {
                let ring = Ringing {
                    kind: RingKind::Alarm,
                    id: alarm.id.clone(),
                };
                upsert(&mut self.alarms, alarm, |a| &a.id);
                if !self.ringing.contains(&ring) {
                    self.ringing.push(ring);
                }
            }
            Event::TimerChanged { timer } => upsert(&mut self.timers, timer, |t| &t.id),
            Event::TimerDeleted { id } => {
                self.timers.retain(|t| t.id != id);
                self.ringing.retain(|r| r.id != id);
            }
            Event::TimerRinging { timer } => {
                let ring = Ringing {
                    kind: RingKind::Timer,
                    id: timer.id.clone(),
                };
                upsert(&mut self.timers, timer, |t| &t.id);
                if !self.ringing.contains(&ring) {
                    self.ringing.push(ring);
                }
            }
            Event::AlarmEnded { id } | Event::TimerEnded { id } => {
                self.ringing.retain(|r| r.id != id);
            }
            Event::SettingsChanged { settings } => self.do_not_disturb = settings.do_not_disturb,
            Event::Unknown => {}
        }
    }

    pub fn notification(&self, id: &str) -> Option<&Notification> {
        self.notifications.iter().find(|n| n.id == id)
    }

    /// An enabled alarm (or a snoozed one) will ring.
    pub fn alarm_set(&self) -> bool {
        self.alarms
            .iter()
            .any(|a| a.next_fire_at.is_some() || a.snoozed_until.is_some())
    }
}

fn upsert<T>(list: &mut Vec<T>, item: T, id: impl Fn(&T) -> &String) {
    match list.iter_mut().find(|existing| id(existing) == id(&item)) {
        Some(existing) => *existing = item,
        None => list.push(item),
    }
}

/// A line from the daemon: an event, or the failure of one of Moon's
/// requests (only failures matter to Moon; they are logged).
#[derive(Debug)]
pub enum Incoming {
    Event(Event),
    Failed { cmd: String, code: String },
}

pub struct NotifyClient {
    line: LineClient,
    /// The daemon's protocol revision (`ping`), once known.
    revision: Option<u64>,
    pinged: bool,
}

/// Tap haptics (protocol revision 4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Haptic {
    Selection,
    Light,
    Medium,
    Error,
}

impl Haptic {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "selection" => Some(Self::Selection),
            "light" => Some(Self::Light),
            "medium" => Some(Self::Medium),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// Its slot in `Settings::haptic_ms`.
    pub fn index(self) -> usize {
        match self {
            Self::Selection => 0,
            Self::Light => 1,
            Self::Medium => 2,
            Self::Error => 3,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Selection => "selection",
            Self::Light => "light",
            Self::Medium => "medium",
            Self::Error => "error",
        }
    }
}

impl NotifyClient {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            line: LineClient::new("moon-notifyd", path, REQUEST_TIMEOUT),
            revision: None,
            pinged: false,
        }
    }

    /// A short vibrator pulse, if the daemon knows `haptic` (revision 4);
    /// an older one would refuse it.
    /// `ms` overrides the style's length (Moon's tuned lengths); only a
    /// revision-5 daemon takes it (revision 4 would refuse the whole
    /// haptic), so older ones get the style alone.
    pub fn haptic(&mut self, style: Haptic, ms: Option<u32>) {
        if self.line.connected() && self.revision.is_some_and(|revision| revision >= 4) {
            match ms.filter(|_| self.revision.is_some_and(|revision| revision >= 5)) {
                Some(ms) => self
                    .line
                    .send("haptic", json!({ "style": style.name(), "ms": ms })),
                None => self.line.send("haptic", json!({ "style": style.name() })),
            }
        }
    }

    pub fn connected(&self) -> bool {
        self.line.connected()
    }

    /// The descriptor to poll, and whether Moon has output waiting for it.
    pub fn poll_fd(&self) -> Option<(RawFd, bool)> {
        self.line.poll_fd()
    }

    /// When the loop must wake for this client (a reconnect attempt or a
    /// request deadline).
    pub fn next_deadline(&self) -> Option<Instant> {
        self.line.next_deadline()
    }

    // The commands Moon sends for the user's actions (§4). An object is
    // named by its kind (`notification`, `alarm`, `timer`); `id` is only
    // the request's own id (protocol revision 2).

    pub fn dismiss(&mut self, notification: &str) {
        self.line
            .send("notify.dismiss", json!({ "notification": notification }));
    }

    pub fn dismiss_all(&mut self, app: Option<&str>) {
        match app {
            Some(app) => self.line.send("notify.dismiss_all", json!({ "app": app })),
            None => self.line.send("notify.dismiss_all", json!({})),
        }
    }

    pub fn activate(&mut self, notification: &str) {
        self.line
            .send("notify.activate", json!({ "notification": notification }));
    }

    pub fn mark_seen(&mut self, notifications: &[String]) {
        if !notifications.is_empty() {
            self.line
                .send("notify.mark_seen", json!({ "ids": notifications }));
        }
    }

    pub fn set_do_not_disturb(&mut self, on: bool) {
        self.line
            .send("settings.set", json!({ "do_not_disturb": on }));
    }

    pub fn ring_action(&mut self, kind: RingKind, id: &str, action: RingAction) {
        let (cmd, key) = match (kind, action) {
            (RingKind::Alarm, RingAction::Stop) => ("alarm.stop", "alarm"),
            (RingKind::Alarm, RingAction::Again) => ("alarm.snooze", "alarm"),
            (RingKind::Timer, RingAction::Stop) => ("timer.stop", "timer"),
            (RingKind::Timer, RingAction::Again) => ("timer.restart", "timer"),
        };
        let mut arguments = serde_json::Map::new();
        arguments.insert(key.to_owned(), json!(id));
        self.line.send(cmd, Value::Object(arguments));
    }

    /// Connects when due, flushes output, and returns everything received.
    /// Never blocks.
    pub fn service(&mut self, now: Instant) -> Vec<Incoming> {
        let lines = self.line.service(now);
        if !self.line.connected() {
            self.pinged = false;
            self.revision = None;
        } else if !self.pinged {
            // Which revision answers decides what Moon may send.
            self.pinged = true;
            self.line.send("ping", json!({}));
        }
        lines
            .into_iter()
            .filter_map(|line| match line {
                Line::Event(value) => match serde_json::from_value::<Event>(value) {
                    Ok(event) => Some(Incoming::Event(event)),
                    Err(error) => {
                        eprintln!("moon-notifyd event not understood: {error}");
                        None
                    }
                },
                Line::Reply { cmd, result } => {
                    if cmd == "ping" {
                        // Absent means revision 2 (protocol §1).
                        self.revision = Some(result["revision"].as_u64().unwrap_or(2));
                    }
                    None
                }
                Line::Failed { cmd, code } => Some(Incoming::Failed { cmd, code }),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn events_parse_and_update_the_store() {
        let mut store = NotifyStore::default();
        let lines = [
            r#"{"event":"snapshot","notifications":[{"id":"n1","app":"system","tag":"wifi","title":"Wi-Fi","body":"On","urgency":"normal","silent":false,"posted_at":10,"expires_at":null,"seen":false,"open":{"app":"system","target":"wifi"}}],"alarms":[{"id":"a1","hour":7,"minute":30,"days":[1,2],"label":"Up","enabled":true,"sound":"radar","vibrate":true,"snooze_minutes":9,"next_fire_at":100,"snoozed_until":null,"ringing":false}],"timers":[],"ringing":[],"settings":{"alert_volume":70},"now":5}"#,
            r#"{"event":"notification_posted","notification":{"id":"n2","app":"browser","title":"Done","posted_at":11},"replaced":false,"wake":true}"#,
            r#"{"event":"timer_ringing","timer":{"id":"t1","label":"Pasta","duration_s":600,"state":"ringing","ends_at":null,"remaining_s":0,"sound":"chimes"}}"#,
            r#"{"event":"something_new","x":1}"#,
            r#"{"event":"notification_removed","id":"n1","reason":"dismissed"}"#,
        ];
        let mut wake = false;
        for line in lines {
            let event: Event = serde_json::from_str(line).unwrap();
            if let Event::NotificationPosted { wake: true, .. } = event {
                wake = true;
            }
            store.apply(&event);
        }
        assert!(wake);
        assert_eq!(store.notifications.len(), 1);
        assert_eq!(store.notifications[0].id, "n2");
        assert!(store.alarm_set());
        assert_eq!(
            store.ringing,
            vec![Ringing {
                kind: RingKind::Timer,
                id: "t1".into()
            }]
        );
        store.apply(&Event::TimerEnded { id: "t1".into() });
        assert!(store.ringing.is_empty());
    }

    #[test]
    fn requests_are_one_line_with_an_id_and_failures_are_reported() {
        let dir = std::env::temp_dir().join(format!("moon-notify-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("notify.sock");
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let mut client = NotifyClient::new(&path);
        assert!(client.service(Instant::now()).is_empty());
        assert!(client.connected());
        let (mut server, _) = listener.accept().unwrap();
        client.dismiss("n9");
        client.service(Instant::now());
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut received = String::new();
        let mut buffer = [0_u8; 512];
        while received.matches('\n').count() < 3 {
            let read = server.read(&mut buffer).unwrap();
            received.push_str(std::str::from_utf8(&buffer[..read]).unwrap());
        }
        let lines: Vec<Value> = received
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines[0]["cmd"], "subscribe");
        assert_eq!(lines[1]["cmd"], "ping");
        assert_eq!(lines[2]["cmd"], "notify.dismiss");
        assert!(lines[2]["id"].as_u64().is_some());
        assert_eq!(lines[2]["notification"], json!("n9"));
        // No haptics until the daemon says it has them (revision 4).
        client.haptic(Haptic::Error, None);
        server
            .write_all(
                format!(
                    "{{\"id\":{},\"ok\":true,\"result\":{{}}}}\n{{\"id\":{},\"ok\":true,\"result\":{{\"version\":1,\"revision\":4}}}}\n{{\"id\":{},\"ok\":false,\"error\":{{\"code\":\"not_found\",\"message\":\"x\"}}}}\n{{\"event\":\"notification_removed\",\"id\":\"n9\",\"reason\":\"dismissed\"}}\n",
                    lines[0]["id"], lines[1]["id"], lines[2]["id"]
                )
                .as_bytes(),
            )
            .unwrap();
        let mut got = Vec::new();
        for _ in 0..100 {
            got.extend(client.service(Instant::now()));
            if got.len() >= 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            matches!(&got[0], Incoming::Failed { cmd, code } if cmd == "notify.dismiss" && code == "not_found")
        );
        assert!(
            matches!(&got[1], Incoming::Event(Event::NotificationRemoved { id }) if id == "n9")
        );
        assert_eq!(client.revision, Some(4));
        client.haptic(Haptic::Light, None);
        client.service(Instant::now());
        let mut more = String::new();
        while !more.contains('\n') {
            let read = server.read(&mut buffer).unwrap();
            more.push_str(std::str::from_utf8(&buffer[..read]).unwrap());
        }
        let haptic: Value = serde_json::from_str(more.lines().next().unwrap()).unwrap();
        assert_eq!(haptic["cmd"], "haptic");
        assert_eq!(haptic["style"], "light");
        drop(server);
        let _ = std::fs::remove_file(&path);
    }
}
