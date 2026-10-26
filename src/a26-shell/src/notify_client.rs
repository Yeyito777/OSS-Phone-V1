//! Moon's client for `moon-notifyd` (docs/notifications-protocol.md, v1):
//! newline-delimited JSON over a Unix stream socket. The socket is
//! non-blocking and polled with the rest of Moon's event loop; a missing or
//! stalled daemon never holds up the UI. After every (re)connect Moon
//! subscribes, and the daemon's `snapshot` replaces all of `NotifyStore`.

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

pub const DEFAULT_SOCKET: &str = "/run/moon-notify/notify.sock";
/// The protocol's line limit; a longer line from the daemon is a protocol
/// error and drops the connection.
const MAX_LINE: usize = 16 * 1024;
/// Commands Moon could not hand to the socket yet; more means the daemon has
/// stopped reading.
const MAX_QUEUED_BYTES: usize = 64 * 1024;
const RETRY_FIRST: Duration = Duration::from_millis(500);
const RETRY_MAX: Duration = Duration::from_secs(5);
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
}

impl NotifyStore {
    pub fn apply(&mut self, event: &Event) {
        match event.clone() {
            Event::Snapshot {
                notifications,
                alarms,
                timers,
                ringing,
            } => {
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

/// A line from the daemon: an event, or the answer to one of Moon's
/// requests (only failures matter to Moon; they are logged).
#[derive(Debug)]
pub enum Incoming {
    Event(Event),
    Failed { cmd: String, code: String },
}

pub struct NotifyClient {
    path: PathBuf,
    stream: Option<UnixStream>,
    input: Vec<u8>,
    output: Vec<u8>,
    next_id: u64,
    /// Requests awaiting their response: id, command, sent at.
    pending: Vec<(u64, String, Instant)>,
    next_attempt: Instant,
    retry: Duration,
    logged_unavailable: bool,
}

impl NotifyClient {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            stream: None,
            input: Vec::new(),
            output: Vec::new(),
            next_id: 1,
            pending: Vec::new(),
            next_attempt: Instant::now(),
            retry: RETRY_FIRST,
            logged_unavailable: false,
        }
    }

    pub fn connected(&self) -> bool {
        self.stream.is_some()
    }

    /// The descriptor to poll, and whether Moon has output waiting for it.
    pub fn poll_fd(&self) -> Option<(RawFd, bool)> {
        self.stream
            .as_ref()
            .map(|stream| (stream.as_raw_fd(), !self.output.is_empty()))
    }

    /// When the loop must wake for this client (a reconnect attempt or a
    /// request deadline).
    pub fn next_deadline(&self) -> Option<Instant> {
        match &self.stream {
            None => Some(self.next_attempt),
            Some(_) => self.pending.first().map(|(_, _, at)| *at + REQUEST_TIMEOUT),
        }
    }

    // The commands Moon sends for the user's actions (§4). An object is
    // named by its kind (`notification`, `alarm`, `timer`); `id` is only
    // the request's own id (protocol revision 2).

    pub fn dismiss(&mut self, notification: &str) {
        self.send("notify.dismiss", json!({ "notification": notification }));
    }

    pub fn dismiss_all(&mut self, app: Option<&str>) {
        match app {
            Some(app) => self.send("notify.dismiss_all", json!({ "app": app })),
            None => self.send("notify.dismiss_all", json!({})),
        }
    }

    pub fn activate(&mut self, notification: &str) {
        self.send("notify.activate", json!({ "notification": notification }));
    }

    pub fn mark_seen(&mut self, notifications: &[String]) {
        if !notifications.is_empty() {
            self.send("notify.mark_seen", json!({ "ids": notifications }));
        }
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
        self.send(cmd, Value::Object(arguments));
    }

    /// Queues a command (`cmd` plus its arguments).
    fn send(&mut self, cmd: &str, mut arguments: Value) {
        if self.stream.is_none() {
            eprintln!("moon-notifyd unavailable; dropped {cmd}");
            return;
        }
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        if !arguments.is_object() {
            arguments = json!({});
        }
        arguments["id"] = json!(id);
        arguments["cmd"] = json!(cmd);
        self.output
            .extend_from_slice(arguments.to_string().as_bytes());
        self.output.push(b'\n');
        self.pending.push((id, cmd.to_owned(), Instant::now()));
        if self.output.len() > MAX_QUEUED_BYTES {
            self.disconnect("output backlog");
        }
    }

    /// Connects when due, flushes output, and returns everything received.
    /// Never blocks.
    pub fn service(&mut self, now: Instant) -> Vec<Incoming> {
        let mut incoming = Vec::new();
        if self.stream.is_none() {
            if now < self.next_attempt {
                return incoming;
            }
            match connect_nonblocking(&self.path) {
                Ok(stream) => {
                    eprintln!("moon-notifyd connected");
                    self.stream = Some(stream);
                    self.retry = RETRY_FIRST;
                    self.logged_unavailable = false;
                    self.input.clear();
                    self.output.clear();
                    self.pending.clear();
                    self.send("subscribe", json!({}));
                }
                Err(error) => {
                    if !self.logged_unavailable {
                        eprintln!("moon-notifyd unavailable ({error}); retrying");
                        self.logged_unavailable = true;
                    }
                    self.next_attempt = now + self.retry;
                    self.retry = (self.retry * 2).min(RETRY_MAX);
                    return incoming;
                }
            }
        }
        if let Err(error) = self.flush() {
            self.disconnect(&error.to_string());
            return incoming;
        }
        if let Err(error) = self.read_lines(&mut incoming) {
            self.disconnect(&error.to_string());
            return incoming;
        }
        if self
            .pending
            .first()
            .is_some_and(|(_, _, at)| now >= *at + REQUEST_TIMEOUT)
        {
            self.disconnect("request timed out");
        }
        incoming
    }

    fn flush(&mut self) -> io::Result<()> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(());
        };
        while !self.output.is_empty() {
            match stream.write(&self.output) {
                Ok(0) => return Err(io::Error::other("socket closed")),
                Ok(written) => {
                    self.output.drain(..written);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn read_lines(&mut self, incoming: &mut Vec<Incoming>) -> io::Result<()> {
        let Some(stream) = self.stream.as_mut() else {
            return Ok(());
        };
        let mut chunk = [0_u8; 8192];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => return Err(io::Error::other("daemon closed the connection")),
                Ok(read) => self.input.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        while let Some(end) = self.input.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = self.input.drain(..=end).collect();
            if let Some(item) = self.parse_line(&line[..line.len() - 1]) {
                incoming.push(item);
            }
        }
        if self.input.len() > MAX_LINE {
            return Err(io::Error::other("line too long"));
        }
        Ok(())
    }

    fn parse_line(&mut self, line: &[u8]) -> Option<Incoming> {
        let value: Value = match serde_json::from_slice(line) {
            Ok(value) => value,
            Err(error) => {
                eprintln!("moon-notifyd sent invalid JSON: {error}");
                return None;
            }
        };
        if value.get("event").is_some() {
            return match serde_json::from_value::<Event>(value) {
                Ok(event) => Some(Incoming::Event(event)),
                Err(error) => {
                    eprintln!("moon-notifyd event not understood: {error}");
                    None
                }
            };
        }
        let id = value.get("id").and_then(Value::as_u64)?;
        let position = self
            .pending
            .iter()
            .position(|(pending, _, _)| *pending == id)?;
        let (_, cmd, _) = self.pending.remove(position);
        if value.get("ok").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        let code = value
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_owned();
        Some(Incoming::Failed { cmd, code })
    }

    fn disconnect(&mut self, why: &str) {
        if self.stream.take().is_some() {
            eprintln!("moon-notifyd connection lost: {why}");
        }
        self.input.clear();
        self.output.clear();
        self.pending.clear();
        self.next_attempt = Instant::now() + self.retry;
    }
}

/// A non-blocking connect: a local socket either accepts at once or is
/// refused; a full backlog (`EAGAIN`) counts as unavailable.
fn connect_nonblocking(path: &Path) -> io::Result<UnixStream> {
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: plain socket(2) call; the descriptor is owned below.
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd is a fresh socket owned by nobody else.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    // SAFETY: sockaddr_un is plain old data; zeroed is a valid value.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    if bytes.len() >= address.sun_path.len() {
        return Err(io::Error::other("socket path too long"));
    }
    for (slot, byte) in address.sun_path.iter_mut().zip(bytes) {
        *slot = *byte as libc::c_char;
    }
    let length = std::mem::size_of::<libc::sa_family_t>() + bytes.len() + 1;
    // SAFETY: address is initialized and length is within its size.
    let result = unsafe {
        libc::connect(
            fd,
            (&address as *const libc::sockaddr_un).cast(),
            length as libc::socklen_t,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        while received.matches('\n').count() < 2 {
            let read = server.read(&mut buffer).unwrap();
            received.push_str(std::str::from_utf8(&buffer[..read]).unwrap());
        }
        let lines: Vec<Value> = received
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines[0]["cmd"], "subscribe");
        assert_eq!(lines[1]["cmd"], "notify.dismiss");
        assert!(lines[1]["id"].as_u64().is_some());
        assert_eq!(lines[1]["notification"], json!("n9"));
        server
            .write_all(
                format!(
                    "{{\"id\":{},\"ok\":true,\"result\":{{}}}}\n{{\"id\":{},\"ok\":false,\"error\":{{\"code\":\"not_found\",\"message\":\"x\"}}}}\n{{\"event\":\"notification_removed\",\"id\":\"n9\",\"reason\":\"dismissed\"}}\n",
                    lines[0]["id"], lines[1]["id"]
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
        drop(server);
        let _ = std::fs::remove_file(&path);
    }
}
