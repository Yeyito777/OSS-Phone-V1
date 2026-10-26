//! Moon's client for `moon-telephonyd` (docs/telephony-protocol.md, v1),
//! over the JSON-lines transport in line_client.rs: the status bar's SIM,
//! network and signal state, and the calls the call screen shows. Messages
//! and call history belong to the Phone and Messages apps, which connect
//! themselves.

use std::os::fd::RawFd;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use crate::line_client::{Line, LineClient};

pub const DEFAULT_SOCKET: &str = "/run/moon-telephony/telephony.sock";
/// Protocol §1: 1 s for most commands, 15 s for those that wait for the
/// modem (⧗).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const MODEM_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Deserialize)]
pub struct Sim {
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Deserialize)]
pub struct Network {
    #[serde(default)]
    pub registration: String,
    /// Not registered, but emergency calls are possible (revision 1.1).
    #[serde(default)]
    pub emergency_only: bool,
    #[serde(default)]
    pub operator: Option<String>,
    #[serde(default)]
    pub rat: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Deserialize)]
pub struct Signal {
    #[serde(default)]
    pub level: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Deserialize)]
pub struct Data {
    #[serde(default)]
    pub state: String,
}

/// The daemon's State object, the parts Moon uses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Deserialize)]
pub struct TelState {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub airplane: bool,
    #[serde(default)]
    pub sim: Sim,
    #[serde(default)]
    pub network: Network,
    #[serde(default)]
    pub signal: Option<Signal>,
    #[serde(default)]
    pub data: Data,
    #[serde(default)]
    pub calls: Vec<Call>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
pub struct Call {
    pub id: String,
    #[serde(default)]
    pub number: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    pub state: String,
    #[serde(default)]
    pub direction: String,
    #[serde(default)]
    pub started_at: i64,
    #[serde(default)]
    pub connected_at: Option<i64>,
}

impl Call {
    /// Ringing here: an incoming call, or one waiting behind another.
    pub fn ringing(&self) -> bool {
        matches!(self.state.as_str(), "incoming" | "waiting")
    }
}

/// A daemon event (§4). Unknown events are ignored, as the protocol asks.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Snapshot {
        state: TelState,
    },
    StateChanged {
        state: TelState,
    },
    CallsChanged {
        calls: Vec<Call>,
    },
    CallIncoming {
        call: Call,
    },
    CallEnded {
        call: Call,
    },
    #[serde(other)]
    Unknown,
}

/// What the status bar shows for the cellular network, iPhone style.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Cellular {
    /// No daemon or no modem: nothing is drawn.
    Hidden,
    Airplane,
    /// A short text in place of the bars ("No SIM", "SOS", "Searching").
    Text(&'static str),
    /// Bars (0-4) and the radio technology ("LTE", "5G").
    Bars {
        level: u8,
        rat: Option<&'static str>,
    },
}

impl TelState {
    pub fn cellular(&self) -> Cellular {
        if !self.available {
            return Cellular::Hidden;
        }
        if self.airplane {
            return Cellular::Airplane;
        }
        match self.sim.state.as_str() {
            "absent" | "no_profile" => return Cellular::Text("No SIM"),
            "pin_required" | "puk_required" | "network_locked" => {
                return Cellular::Text("SIM Locked");
            }
            "ready" => {}
            _ => return Cellular::Text("No SIM"),
        }
        match self.network.registration.as_str() {
            "home" | "roaming" => Cellular::Bars {
                level: self.signal.map_or(0, |s| s.level.min(4)),
                rat: self.data_rat(),
            },
            _ if self.network.emergency_only => Cellular::Text("SOS"),
            "searching" => Cellular::Text("Searching"),
            _ => Cellular::Text("No Service"),
        }
    }

    /// The technology label, shown while mobile data is connected.
    fn data_rat(&self) -> Option<&'static str> {
        if self.data.state != "connected" {
            return None;
        }
        match self.network.rat.as_deref()? {
            "nr" => Some("5G"),
            "lte" => Some("LTE"),
            "umts" => Some("3G"),
            "gsm" => Some("E"),
            _ => None,
        }
    }

    /// The carrier name for the lock screen and Control Centre.
    pub fn carrier(&self) -> Option<String> {
        if self.available && !self.airplane && self.network.emergency_only {
            return Some("Emergency calls only".into());
        }
        match self.cellular() {
            Cellular::Hidden => None,
            Cellular::Airplane => Some("Airplane Mode".into()),
            Cellular::Text(text) => Some(text.into()),
            Cellular::Bars { .. } => self.network.operator.clone().filter(|o| !o.is_empty()),
        }
    }
}

/// What Moon knows of the daemon, kept current from events.
#[derive(Debug, Default)]
pub struct TelStore {
    pub state: TelState,
    pub connected: bool,
}

impl TelStore {
    pub fn apply(&mut self, event: &Event) {
        match event {
            Event::Snapshot { state } | Event::StateChanged { state } => {
                self.state = state.clone();
            }
            Event::CallsChanged { calls } => self.state.calls = calls.clone(),
            Event::CallIncoming { call } => {
                if !self.state.calls.iter().any(|c| c.id == call.id) {
                    self.state.calls.push(call.clone());
                }
            }
            Event::CallEnded { call, .. } => self.state.calls.retain(|c| c.id != call.id),
            Event::Unknown => {}
        }
    }

    pub fn calls(&self) -> &[Call] {
        if self.connected {
            &self.state.calls
        } else {
            &[]
        }
    }
}

/// What the call screen asks the daemon to do (drained by the main loop).
/// Each is an explicit user action, as §3 requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallCommand {
    Answer,
    Reject,
    Hangup(Option<String>),
    Swap,
    Dtmf(String),
    Audio { speaker: bool, mute: bool },
}

#[derive(Debug)]
pub enum Incoming {
    Event(Event),
    Failed { cmd: String, code: String },
}

pub struct TelephonyClient {
    line: LineClient,
}

impl TelephonyClient {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            line: LineClient::new("moon-telephonyd", path, REQUEST_TIMEOUT),
        }
    }

    pub fn connected(&self) -> bool {
        self.line.connected()
    }

    pub fn poll_fd(&self) -> Option<(RawFd, bool)> {
        self.line.poll_fd()
    }

    pub fn next_deadline(&self) -> Option<Instant> {
        self.line.next_deadline()
    }

    pub fn send(&mut self, command: CallCommand) {
        let line = &mut self.line;
        match command {
            CallCommand::Answer => line.send_waiting("call.answer", json!({}), MODEM_TIMEOUT),
            CallCommand::Reject => line.send_waiting("call.reject", json!({}), MODEM_TIMEOUT),
            CallCommand::Hangup(None) => line.send_waiting("call.hangup", json!({}), MODEM_TIMEOUT),
            CallCommand::Hangup(Some(id)) => {
                // The call is named `call`: `id` is the request's own id.
                line.send_waiting("call.hangup", json!({ "call": id }), MODEM_TIMEOUT);
            }
            CallCommand::Swap => line.send_waiting("call.swap", json!({}), MODEM_TIMEOUT),
            CallCommand::Dtmf(digits) => {
                line.send_waiting("call.dtmf", json!({ "digits": digits }), MODEM_TIMEOUT);
            }
            CallCommand::Audio { speaker, mute } => {
                line.send("call.audio", json!({ "speaker": speaker, "mute": mute }));
            }
        }
    }

    pub fn service(&mut self, now: Instant) -> Vec<Incoming> {
        self.line
            .service(now)
            .into_iter()
            .filter_map(|line| match line {
                Line::Event(value) => match serde_json::from_value::<Event>(value) {
                    Ok(event) => Some(Incoming::Event(event)),
                    Err(error) => {
                        eprintln!("moon-telephonyd event not understood: {error}");
                        None
                    }
                },
                Line::Failed { cmd, code } => Some(Incoming::Failed { cmd, code }),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_parses_and_maps_to_the_status_bar() {
        let line = r#"{"event":"snapshot","now":5,"state":{"available":true,"radio":"on","airplane":false,"sim":{"slot":1,"state":"ready","pin_retries":null},"network":{"registration":"home","data_registration":"home","operator":"Freedom","numeric":"30249","rat":"lte","ims_registered":false,"cell":null},"signal":{"level":3,"dbm":-97},"data":{"enabled":true,"state":"connected","apn":"x","interface":"rmnet0","addresses":[],"dns":[],"error":null},"calls":[],"sms":{"unread":2}}}"#;
        let event: Event = serde_json::from_str(line).unwrap();
        let mut store = TelStore {
            connected: true,
            ..TelStore::default()
        };
        store.apply(&event);
        assert_eq!(
            store.state.cellular(),
            Cellular::Bars {
                level: 3,
                rat: Some("LTE")
            }
        );
        assert_eq!(store.state.carrier().as_deref(), Some("Freedom"));
        store.state.data.state = "off".into();
        assert_eq!(
            store.state.cellular(),
            Cellular::Bars {
                level: 3,
                rat: None
            }
        );
        store.state.network.registration = "none".into();
        store.state.network.emergency_only = true;
        assert_eq!(store.state.cellular(), Cellular::Text("SOS"));
        assert_eq!(
            store.state.carrier().as_deref(),
            Some("Emergency calls only")
        );
        store.state.sim.state = "absent".into();
        assert_eq!(store.state.cellular(), Cellular::Text("No SIM"));
        store.state.airplane = true;
        assert_eq!(store.state.cellular(), Cellular::Airplane);
        store.state.available = false;
        assert_eq!(store.state.cellular(), Cellular::Hidden);

        let incoming: Event = serde_json::from_str(
            r#"{"event":"call_incoming","call":{"id":"c3","number":"+16475551234","name":null,"state":"incoming","direction":"incoming","multiparty":false,"started_at":1,"connected_at":null}}"#,
        )
        .unwrap();
        store.apply(&incoming);
        assert!(store.calls()[0].ringing());
        let ended: Event = serde_json::from_str(
            r#"{"event":"call_ended","call":{"id":"c3","number":null,"state":"ended","direction":"incoming","started_at":1},"duration_s":0,"missed":true}"#,
        )
        .unwrap();
        store.apply(&ended);
        assert!(store.calls().is_empty());
        let unknown: Event = serde_json::from_str(r#"{"event":"ussd","message":"hi"}"#).unwrap();
        store.apply(&unknown);
    }
}
