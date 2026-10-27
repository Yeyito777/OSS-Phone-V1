//! Persistent state: the SMS history and settings, as JSON files written
//! atomically (docs/telephony-protocol.md §8).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// The newest messages kept; older ones are dropped on save.
pub const MAX_MESSAGES: usize = 5000;

#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub id: u64,
    pub peer: String,
    pub incoming: bool,
    pub text: String,
    pub time: i64,
    /// "received", "sending", "sent", "delivered", "failed".
    pub status: String,
    pub read: bool,
    pub error: Option<String>,
    /// Network message references of the parts (for status reports).
    pub refs: Vec<i32>,
}

impl Message {
    pub fn to_json(&self) -> Value {
        json!({
            "id": format!("m{}", self.id),
            "peer": self.peer,
            "direction": if self.incoming { "in" } else { "out" },
            "text": self.text,
            "time": self.time,
            "status": self.status,
            "read": self.read,
            "error": self.error,
        })
    }

    fn to_store(&self) -> Value {
        let mut v = self.to_json();
        v["id"] = json!(self.id);
        v["refs"] = json!(self.refs);
        v
    }

    fn from_store(v: &Value) -> Option<Self> {
        Some(Self {
            id: v.get("id")?.as_u64()?,
            peer: v.get("peer")?.as_str()?.to_owned(),
            incoming: v.get("direction")?.as_str()? == "in",
            text: v.get("text")?.as_str()?.to_owned(),
            time: v.get("time")?.as_i64()?,
            status: v.get("status")?.as_str()?.to_owned(),
            read: v.get("read")?.as_bool()?,
            error: v.get("error").and_then(Value::as_str).map(str::to_owned),
            refs: v
                .get("refs")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|r| r.as_i64())
                        .map(|r| r as i32)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

/// Parses "m42" (or 42) into a message id.
pub fn parse_id(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str()?.strip_prefix('m')?.parse().ok())
}

/// A canonical thread key for a phone number: NANP numbers in +1 form,
/// separators dropped; anything else (alphanumeric senders) unchanged.
pub fn normalize_peer(raw: &str) -> String {
    let trimmed = raw.trim();
    let digits: String = trimmed
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | '(' | ')' | '.'))
        .collect();
    let body = digits.strip_prefix('+').unwrap_or(&digits);
    if body.is_empty()
        || !body
            .chars()
            .all(|c| c.is_ascii_digit() || c == '*' || c == '#')
    {
        return trimmed.to_owned();
    }
    if digits.starts_with('+') {
        return digits;
    }
    match body.len() {
        10 if !body.starts_with(['0', '1']) => format!("+1{body}"),
        11 if body.starts_with('1') => format!("+{body}"),
        _ => body.to_owned(),
    }
}

pub struct SmsStore {
    path: PathBuf,
    pub messages: Vec<Message>,
    next_id: u64,
}

impl SmsStore {
    pub fn load(path: &Path) -> Self {
        let messages: Vec<Message> = fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.get("messages")?.as_array().cloned())
            .map(|a| a.iter().filter_map(Message::from_store).collect())
            .unwrap_or_default();
        let next_id = messages.iter().map(|m| m.id).max().unwrap_or(0) + 1;
        Self {
            path: path.to_owned(),
            messages,
            next_id,
        }
    }

    pub fn save(&mut self) -> io::Result<()> {
        if self.messages.len() > MAX_MESSAGES {
            let excess = self.messages.len() - MAX_MESSAGES;
            self.messages.drain(..excess);
        }
        let all: Vec<Value> = self.messages.iter().map(Message::to_store).collect();
        let bytes = serde_json::to_vec(&json!({"version": 1, "messages": all}))?;
        atomic_write(&self.path, &bytes, 0o600)
    }

    pub fn add(&mut self, mut message: Message) -> &Message {
        message.id = self.next_id;
        self.next_id += 1;
        self.messages.push(message);
        self.messages.last().unwrap()
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Message> {
        self.messages.iter_mut().find(|m| m.id == id)
    }

    pub fn unread(&self) -> usize {
        self.messages
            .iter()
            .filter(|m| m.incoming && !m.read)
            .count()
    }

    /// Conversations, most recent first.
    pub fn threads(&self) -> Vec<Value> {
        let mut peers: Vec<(&str, &Message, usize, usize)> = Vec::new();
        for m in &self.messages {
            match peers.iter_mut().find(|(p, ..)| *p == m.peer) {
                Some(entry) => {
                    if m.time >= entry.1.time {
                        entry.1 = m;
                    }
                    entry.2 += 1;
                    entry.3 += (m.incoming && !m.read) as usize;
                }
                None => peers.push((&m.peer, m, 1, (m.incoming && !m.read) as usize)),
            }
        }
        peers.sort_by(|a, b| b.1.time.cmp(&a.1.time).then(b.1.id.cmp(&a.1.id)));
        peers
            .into_iter()
            .map(|(peer, last, count, unread)| json!({"peer": peer, "last": last.to_json(), "count": count, "unread": unread}))
            .collect()
    }

    /// A thread's messages, newest first, older than `before` (an id).
    pub fn list(&self, peer: &str, before: Option<u64>, limit: usize) -> Vec<Value> {
        let mut list: Vec<&Message> = self
            .messages
            .iter()
            .filter(|m| m.peer == peer && before.is_none_or(|b| m.id < b))
            .collect();
        list.sort_by(|a, b| b.time.cmp(&a.time).then(b.id.cmp(&a.id)));
        list.into_iter().take(limit).map(Message::to_json).collect()
    }
}

/// The newest call records kept.
pub const MAX_CALLS: usize = 500;

#[derive(Debug, Clone, PartialEq)]
pub struct CallRecord {
    pub id: u64,
    pub number: Option<String>,
    pub name: Option<String>,
    pub incoming: bool,
    pub started_at: i64,
    pub connected_at: Option<i64>,
    pub ended_at: i64,
    pub missed: bool,
    pub rejected: bool,
}

impl CallRecord {
    pub fn to_json(&self) -> Value {
        json!({
            "id": format!("c{}", self.id),
            "number": self.number,
            "name": self.name,
            "direction": if self.incoming { "incoming" } else { "outgoing" },
            "started_at": self.started_at,
            "connected_at": self.connected_at,
            "ended_at": self.ended_at,
            "duration_s": self.connected_at.map_or(0, |c| (self.ended_at - c).max(0)),
            "missed": self.missed,
            "rejected": self.rejected,
        })
    }

    fn from_json(v: &Value) -> Option<Self> {
        Some(Self {
            id: v.get("id")?.as_str()?.strip_prefix('c')?.parse().ok()?,
            number: v.get("number").and_then(Value::as_str).map(str::to_owned),
            name: v.get("name").and_then(Value::as_str).map(str::to_owned),
            incoming: v.get("direction")?.as_str()? == "incoming",
            started_at: v.get("started_at")?.as_i64()?,
            connected_at: v.get("connected_at").and_then(Value::as_i64),
            ended_at: v.get("ended_at")?.as_i64()?,
            missed: v.get("missed")?.as_bool()?,
            rejected: v.get("rejected")?.as_bool()?,
        })
    }
}

/// Parses "c42" (or 42) into a call id.
pub fn parse_call_id(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_str()?.strip_prefix('c')?.parse().ok())
}

pub struct CallLog {
    path: PathBuf,
    /// Oldest first.
    pub records: Vec<CallRecord>,
}

impl CallLog {
    pub fn load(path: &Path) -> Self {
        let records = fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| v.get("calls")?.as_array().cloned())
            .map(|a| a.iter().filter_map(CallRecord::from_json).collect())
            .unwrap_or_default();
        Self {
            path: path.to_owned(),
            records,
        }
    }

    pub fn max_id(&self) -> u64 {
        self.records.iter().map(|r| r.id).max().unwrap_or(0)
    }

    pub fn save(&mut self) -> io::Result<()> {
        if self.records.len() > MAX_CALLS {
            let excess = self.records.len() - MAX_CALLS;
            self.records.drain(..excess);
        }
        let all: Vec<Value> = self.records.iter().map(CallRecord::to_json).collect();
        let bytes = serde_json::to_vec(&json!({"version": 1, "calls": all}))?;
        atomic_write(&self.path, &bytes, 0o600)
    }

    /// Newest first, older than the record `before` (an id).
    pub fn list(&self, before: Option<u64>, limit: usize) -> Vec<Value> {
        let mut list: Vec<&CallRecord> = self
            .records
            .iter()
            .filter(|r| before.is_none_or(|b| r.id < b))
            .collect();
        list.sort_by(|a, b| b.started_at.cmp(&a.started_at).then(b.id.cmp(&a.id)));
        list.into_iter()
            .take(limit)
            .map(CallRecord::to_json)
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Apn {
    pub apn: String,
    pub protocol: String,
    pub user: String,
    pub password: String,
    pub auth: i32,
}

impl Default for Apn {
    /// Freedom Mobile's internet APN (the SIM this phone runs).
    fn default() -> Self {
        Self {
            apn: "internet.freedommobile.ca".into(),
            protocol: "IPV4V6".into(),
            user: String::new(),
            password: String::new(),
            auth: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    pub data_enabled: bool,
    pub airplane: bool,
    pub apn: Apn,
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let Some(v) = fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        else {
            return Self::default();
        };
        let text =
            |o: &Value, k: &str, d: &str| o.get(k).and_then(Value::as_str).unwrap_or(d).to_owned();
        let default = Apn::default();
        let apn = v.get("apn").cloned().unwrap_or(Value::Null);
        Self {
            data_enabled: v
                .get("data_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            airplane: v.get("airplane").and_then(Value::as_bool).unwrap_or(false),
            apn: Apn {
                apn: text(&apn, "apn", &default.apn),
                protocol: text(&apn, "protocol", &default.protocol),
                user: text(&apn, "user", ""),
                password: text(&apn, "password", ""),
                auth: apn.get("auth").and_then(Value::as_i64).unwrap_or(0) as i32,
            },
        }
    }

    pub fn apn_json(&self, with_password: bool) -> Value {
        let mut v: Map<String, Value> = Map::new();
        v.insert("apn".into(), json!(self.apn.apn));
        v.insert("protocol".into(), json!(self.apn.protocol));
        v.insert("user".into(), json!(self.apn.user));
        v.insert("auth".into(), json!(self.apn.auth));
        if with_password {
            v.insert("password".into(), json!(self.apn.password));
        }
        Value::Object(v)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let v = json!({
            "version": 1,
            "data_enabled": self.data_enabled,
            "airplane": self.airplane,
            "apn": self.apn_json(true),
        });
        atomic_write(path, &serde_json::to_vec_pretty(&v)?, 0o600)
    }
}

pub fn atomic_write(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    let temporary = path.with_extension("new");
    {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(mode)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))?;
    fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_log_round_trips() {
        let dir = std::env::temp_dir().join(format!("moon-telephony-calls-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut log = CallLog::load(&dir.join("calls.json"));
        for (id, started, connected) in [(1, 100, Some(110)), (2, 200, None)] {
            log.records.push(CallRecord {
                id,
                number: Some("+16475551234".into()),
                name: None,
                incoming: id == 2,
                started_at: started,
                connected_at: connected,
                ended_at: started + 70,
                missed: connected.is_none(),
                rejected: false,
            });
        }
        log.save().unwrap();
        let loaded = CallLog::load(&dir.join("calls.json"));
        assert_eq!(loaded.records, log.records);
        let list = loaded.list(None, 50);
        assert_eq!(list[0]["id"], "c2");
        assert_eq!(list[1]["duration_s"], 60);
        assert_eq!(loaded.list(Some(2), 50).len(), 1);
        assert_eq!(loaded.max_id(), 2);
        assert_eq!(parse_call_id(&json!("c7")), Some(7));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn peers_are_normalized_for_threading() {
        assert_eq!(normalize_peer("(647) 555-1234"), "+16475551234");
        assert_eq!(normalize_peer("16475551234"), "+16475551234");
        assert_eq!(normalize_peer("+44 20 7946 0958"), "+442079460958");
        assert_eq!(normalize_peer("611"), "611");
        assert_eq!(normalize_peer("Freedom"), "Freedom");
    }

    #[test]
    fn store_round_trips_and_threads() {
        let dir = std::env::temp_dir().join(format!("moon-telephony-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sms.json");
        let mut store = SmsStore::load(&path);
        let msg = |peer: &str, incoming, time| Message {
            id: 0,
            peer: peer.into(),
            incoming,
            text: "hi".into(),
            time,
            status: "received".into(),
            read: false,
            error: None,
            refs: vec![3],
        };
        store.add(msg("+1", true, 10));
        store.add(msg("+2", true, 30));
        store.add(msg("+1", false, 20));
        store.save().unwrap();
        let loaded = SmsStore::load(&path);
        assert_eq!(loaded.messages, store.messages);
        let threads = loaded.threads();
        assert_eq!(threads[0]["peer"], "+2");
        assert_eq!(threads[1]["count"], 2);
        assert_eq!(threads[1]["unread"], 1);
        assert_eq!(loaded.list("+1", None, 1)[0]["direction"], "out");
        assert_eq!(parse_id(&json!("m3")), Some(3));
        fs::remove_dir_all(&dir).unwrap();
    }
}
