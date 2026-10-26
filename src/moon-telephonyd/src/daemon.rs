//! moon-telephonyd's service loop: the radio HAL connection, the client
//! socket (docs/telephony-protocol.md), notifications through moon-notifyd,
//! call audio and mobile data hooks. One thread blocking in poll(2); timers
//! are deadlines, so nothing wakes while idle.

use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fs;
use std::io::{self, ErrorKind, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::radio::{
    self, CardStatus, Event, Indication, Radio, Registration, Response, Signal, args, req,
};
use crate::sms;
use crate::store::{self, CallLog, CallRecord, Message, Settings, SmsStore};

pub const VERSION: u32 = 1;
const SLOTS: [&str; 2] = ["slot1", "slot2"];
const MAX_LINE: usize = 16 * 1024;
const MAX_TEXT: usize = 1600;
const RECONNECT: Duration = Duration::from_secs(5);
/// dBm-only signal changes are broadcast at most this often.
const SIGNAL_EVENT_INTERVAL: Duration = Duration::from_secs(15);
/// Incomplete concatenated messages are delivered as-is after this long.
const CONCAT_TIMEOUT: Duration = Duration::from_secs(600);
const DATA_RETRY_MAX: Duration = Duration::from_secs(300);
/// SmsAcknowledgeFailCause MEMORY_CAPACITY_EXCEEDED: the network retries.
const SMS_ACK_MEMORY_FULL: i32 = 0xd3;

pub struct Paths {
    pub run_dir: PathBuf,
    pub socket: PathBuf,
    pub state_dir: PathBuf,
    pub notify_socket: PathBuf,
    pub group: PathBuf,
    pub libexec: PathBuf,
}

impl Paths {
    /// The real paths, or all of them under `MOON_TELEPHONY_ROOT`.
    pub fn from_environment() -> Self {
        let system = Self::system();
        let Some(root) = std::env::var_os("MOON_TELEPHONY_ROOT") else {
            return system;
        };
        let root = PathBuf::from(root);
        let join = |path: PathBuf| root.join(path.strip_prefix("/").unwrap_or(&path));
        Self {
            run_dir: join(system.run_dir),
            socket: join(system.socket),
            state_dir: join(system.state_dir),
            notify_socket: join(system.notify_socket),
            group: join(system.group),
            libexec: join(system.libexec),
        }
    }

    /// moon-telephonyd runs in Android's root namespace (it execs the
    /// bionic call-audio helper); clients reach its socket through the
    /// chroot's /run/moon-telephony bind mount.
    pub fn system() -> Self {
        let chroot = Path::new("/data/local/a26-linux");
        Self {
            run_dir: "/data/local/tmp/moon-telephony".into(),
            socket: "/data/local/tmp/moon-telephony/telephony.sock".into(),
            state_dir: chroot.join("var/lib/moon-telephony"),
            notify_socket: chroot.join("run/moon-notify/notify.sock"),
            group: chroot.join("etc/group"),
            libexec: chroot.join("opt/moon-telephony"),
        }
    }
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn now_s() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn log(message: impl AsRef<str>) {
    eprintln!("moon-telephonyd: {}", message.as_ref());
}

#[derive(Default)]
struct SlotState {
    connected: bool,
    radio: Option<i32>,
    card: Option<CardStatus>,
    voice: Option<Registration>,
    data: Option<Registration>,
    operator: Option<Vec<String>>,
    signal: Option<Signal>,
    ims: Option<bool>,
}

struct TrackedCall {
    id: u64,
    call: radio::Call,
    started: i64,
    connected: Option<i64>,
    announced: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Unlock,
    Call,
    Radio,
    ChannelOpen,
    Apdu,
    ChannelClose,
}

enum Pending {
    Query,
    Client { client: u64, id: u64, op: Op },
    SmsPart { message: u64 },
    DataSetup,
    DataDeactivate,
}

struct Outgoing {
    message: u64,
    parts: VecDeque<Vec<u8>>,
}

#[derive(Default, PartialEq, Clone, Copy, Debug)]
enum DataState {
    #[default]
    Off,
    Connecting,
    Connected,
    Disconnecting,
    Failed,
}

#[derive(Default)]
struct Data {
    state: DataState,
    call: Option<radio::DataCall>,
    error: Option<String>,
    retry_at: Option<Instant>,
    backoff: Duration,
}

struct Client {
    key: u64,
    /// Peer is root: may use the raw SIM channel commands.
    root: bool,
    stream: UnixStream,
    input: Vec<u8>,
    output: VecDeque<u8>,
    subscribed: bool,
    closing: bool,
}

/// A persistent, never-blocking connection to moon-notifyd. Posts are
/// queued while it is down (bounded) and its replies are discarded.
struct Notifier {
    path: PathBuf,
    stream: Option<UnixStream>,
    output: VecDeque<u8>,
    input: Vec<u8>,
    next_id: u64,
    retry_at: Option<Instant>,
    /// moon-notifyd's protocol revision (`ping`), once known; `ringing`
    /// posts need 3.
    revision: Option<u64>,
    ping_id: u64,
}

impl Notifier {
    fn send(&mut self, cmd: &str, mut fields: Map<String, Value>) {
        self.next_id += 1;
        fields.insert("id".into(), json!(self.next_id));
        fields.insert("cmd".into(), json!(cmd));
        let mut line = serde_json::to_vec(&Value::Object(fields)).unwrap_or_default();
        line.push(b'\n');
        if self.output.len() + line.len() > 64 * 1024 {
            return;
        }
        self.output.extend(line);
        self.pump();
    }
    fn post(&mut self, fields: Value) {
        if let Value::Object(map) = fields {
            self.send("notify.post", map);
        }
    }
    fn withdraw(&mut self, app: &str, tag: &str) {
        let Value::Object(map) = json!({"app": app, "tag": tag}) else {
            return;
        };
        self.send("notify.withdraw", map);
    }
    fn pump(&mut self) {
        if self.stream.is_none() {
            if self.output.is_empty() || self.retry_at.is_some_and(|t| Instant::now() < t) {
                return;
            }
            match UnixStream::connect(&self.path) {
                Ok(s) => {
                    let _ = s.set_nonblocking(true);
                    self.stream = Some(s);
                    self.retry_at = None;
                    self.input.clear();
                    // Learn the revision first (it may have been upgraded).
                    self.next_id += 1;
                    self.ping_id = self.next_id;
                    let ping = format!("{{\"id\":{},\"cmd\":\"ping\"}}\n", self.ping_id);
                    for (i, b) in ping.bytes().enumerate() {
                        self.output.insert(i, b);
                    }
                }
                Err(_) => {
                    self.retry_at = Some(Instant::now() + Duration::from_secs(5));
                    return;
                }
            }
        }
        let Some(stream) = self.stream.as_mut() else {
            return;
        };
        let mut sink = [0u8; 4096];
        loop {
            match stream.read(&mut sink) {
                Ok(0) => {
                    self.stream = None;
                    return;
                }
                Ok(n) => {
                    self.input.extend_from_slice(&sink[..n]);
                    while let Some(end) = self.input.iter().position(|b| *b == b'\n') {
                        let line: Vec<u8> = self.input.drain(..=end).collect();
                        let reply: Value = serde_json::from_slice(&line).unwrap_or(Value::Null);
                        if reply["id"] == json!(self.ping_id) {
                            self.revision = Some(reply["result"]["revision"].as_u64().unwrap_or(2));
                        } else if reply["ok"] == json!(false) {
                            log(format!(
                                "moon-notifyd refused a request: {}",
                                reply["error"]
                            ));
                        }
                    }
                    if self.input.len() > 64 * 1024 {
                        self.input.clear();
                    }
                    continue;
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.stream = None;
                    return;
                }
            }
        }
        while !self.output.is_empty() {
            let (front, _) = self.output.as_slices();
            match stream.write(front) {
                Ok(0) => break,
                Ok(n) => {
                    self.output.drain(..n);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    self.stream = None;
                    break;
                }
            }
        }
    }
}

struct Concat {
    parts: Vec<Option<String>>,
    sent_at: Option<i64>,
    first: Instant,
}

pub struct Daemon {
    paths: Paths,
    radio: Radio,
    slots: Vec<SlotState>,
    pending: HashMap<i32, Pending>,
    reconnect_at: Option<Instant>,
    listener: UnixListener,
    group: Option<u32>,
    clients: Vec<Client>,
    next_client: u64,
    notifier: Notifier,
    settings: Settings,
    sms: SmsStore,
    outgoing: VecDeque<Outgoing>,
    sending: bool,
    sms_reference: u8,
    concat: HashMap<(String, u16, u8), Concat>,
    calls: Vec<TrackedCall>,
    next_call: u64,
    call_log: CallLog,
    /// Incoming calls the user declined (not "missed").
    rejected: Vec<u64>,
    audio_mode: &'static str,
    speaker: bool,
    mute: bool,
    pin_retries: Option<i32>,
    sim_slots: Vec<radio::SimSlot>,
    data: Data,
    last_state: Value,
    last_signal_event: Instant,
}

pub fn run(paths: Paths) -> Result<(), Box<dyn Error>> {
    // SAFETY: installing async-signal-safe handlers that only store a flag.
    unsafe {
        libc::signal(
            libc::SIGTERM,
            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t,
        );
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    fs::create_dir_all(&paths.state_dir)?;
    fs::set_permissions(&paths.state_dir, fs::Permissions::from_mode(0o700))?;
    let group = group_id(&paths.group, "moon-telephony");
    let listener = bind(&paths, group)?;
    let radio = Radio::open(&SLOTS)?;
    let settings = Settings::load(&paths.state_dir.join("settings.json"));
    let sms = SmsStore::load(&paths.state_dir.join("sms.json"));
    let call_log = CallLog::load(&paths.state_dir.join("calls.json"));
    let mut daemon = Daemon {
        notifier: Notifier {
            path: paths.notify_socket.clone(),
            stream: None,
            output: VecDeque::new(),
            input: Vec::new(),
            next_id: 0,
            retry_at: None,
            revision: None,
            ping_id: 0,
        },
        paths,
        radio,
        slots: SLOTS.iter().map(|_| SlotState::default()).collect(),
        pending: HashMap::new(),
        reconnect_at: Some(Instant::now()),
        listener,
        group,
        clients: Vec::new(),
        next_client: 1,
        settings,
        sms,
        outgoing: VecDeque::new(),
        sending: false,
        sms_reference: (now_s() & 0xff) as u8,
        concat: HashMap::new(),
        calls: Vec::new(),
        // Call ids stay unique across restarts (they key the history).
        next_call: call_log.max_id() + 1,
        call_log,
        rejected: Vec::new(),
        audio_mode: "normal",
        speaker: false,
        mute: false,
        pin_retries: None,
        sim_slots: Vec::new(),
        data: Data::default(),
        last_state: Value::Null,
        last_signal_event: Instant::now(),
    };
    daemon.notifier.send("ping", Map::new());
    log(format!(
        "ready socket={} group={:?} messages={}",
        daemon.paths.socket.display(),
        group,
        daemon.sms.messages.len()
    ));
    while !STOP.load(Ordering::SeqCst) {
        daemon.poll_once()?;
    }
    daemon.flush_concat(true);
    if daemon.data.state == DataState::Connected {
        daemon.data_hook_down();
    }
    let _ = fs::remove_file(&daemon.paths.socket);
    log("stopped");
    Ok(())
}

impl Daemon {
    fn main_slot(&self) -> Option<usize> {
        let present =
            |s: &SlotState| s.connected && s.card.as_ref().is_some_and(|c| c.card_state == 1);
        self.slots
            .iter()
            .position(present)
            .or_else(|| self.slots.iter().position(|s| s.connected))
    }

    fn next_deadline(&self) -> Option<Instant> {
        let concat = self.concat.values().map(|c| c.first + CONCAT_TIMEOUT).min();
        [
            self.reconnect_at,
            self.data.retry_at,
            concat,
            self.notifier
                .retry_at
                .filter(|_| !self.notifier.output.is_empty()),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn poll_once(&mut self) -> Result<(), Box<dyn Error>> {
        let mut fds = vec![
            pollfd(self.listener.as_raw_fd(), libc::POLLIN),
            pollfd(self.radio.binder.fd(), libc::POLLIN),
        ];
        let notify_index = self.notifier.stream.as_ref().map(|s| {
            let events = if self.notifier.output.is_empty() {
                libc::POLLIN
            } else {
                libc::POLLIN | libc::POLLOUT
            };
            fds.push(pollfd(s.as_raw_fd(), events));
            fds.len() - 1
        });
        let first_client = fds.len();
        for client in &self.clients {
            let mut events = libc::POLLIN;
            if !client.output.is_empty() {
                events |= libc::POLLOUT;
            }
            fds.push(pollfd(client.stream.as_raw_fd(), events));
        }
        let timeout = self.next_deadline().map_or(-1, |at| {
            at.saturating_duration_since(Instant::now()).as_millis() as i32 + 1
        });
        // SAFETY: fds is a valid, initialized array of the given length.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout) };
        if ready < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == ErrorKind::Interrupted {
                return Ok(());
            }
            return Err(error.into());
        }
        if fds[1].revents != 0 {
            if let Err(e) = self.radio.process() {
                log(format!("binder: {e}"));
            }
        }
        self.handle_radio_events();
        if notify_index.is_some_and(|i| fds[i].revents != 0) {
            self.notifier.pump();
        }
        for (offset, fd) in fds[first_client..].iter().enumerate() {
            if fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                self.read_client(offset);
            }
        }
        if fds[0].revents & libc::POLLIN != 0 {
            self.accept();
        }
        self.timers();
        self.handle_radio_events();
        for client in &mut self.clients {
            flush(client);
        }
        self.clients.retain(|c| !(c.closing && c.output.is_empty()));
        self.notifier.pump();
        self.broadcast_state_if_changed();
        Ok(())
    }

    fn timers(&mut self) {
        let now = Instant::now();
        if self.reconnect_at.is_some_and(|t| now >= t) {
            self.reconnect_at = None;
            self.connect_slots();
        }
        if self.data.retry_at.is_some_and(|t| now >= t) {
            self.data.retry_at = None;
            self.data_reconcile();
        }
        if self
            .concat
            .values()
            .any(|c| now >= c.first + CONCAT_TIMEOUT)
        {
            self.flush_concat(false);
        }
    }

    fn connect_slots(&mut self) {
        if self.radio.config.is_none() {
            match self.radio.connect_config() {
                Ok(true) => {
                    if let Err(e) = self
                        .radio
                        .config_request(radio::config_req::GET_SIM_SLOTS_STATUS)
                    {
                        log(format!("IRadioConfig request: {e}"));
                    }
                }
                Ok(false) => {}
                Err(e) => log(format!("IRadioConfig connect: {e}")),
            }
        }
        for slot in 0..self.slots.len() {
            if self.slots[slot].connected {
                continue;
            }
            match self.radio.connect(slot) {
                Ok(true) => {
                    log(format!("{} connected", SLOTS[slot]));
                    self.slots[slot] = SlotState {
                        connected: true,
                        ..Default::default()
                    };
                    for code in [
                        req::GET_ICC_CARD_STATUS,
                        req::GET_VOICE_REGISTRATION_STATE,
                        req::GET_DATA_REGISTRATION_STATE,
                        req::GET_OPERATOR,
                        req::GET_SIGNAL_STRENGTH,
                        req::GET_IMS_REGISTRATION_STATE,
                        req::GET_CURRENT_CALLS,
                        req::GET_DATA_CALL_LIST,
                    ] {
                        self.query(slot, code);
                    }
                }
                Ok(false) => {}
                Err(e) => log(format!("{} connect: {e}", SLOTS[slot])),
            }
        }
        if self.slots.iter().any(|s| !s.connected) {
            // A slot whose IRadio is missing is retried slowly; rild
            // registers both at once, so this matters only after a crash.
            self.reconnect_at = Some(
                Instant::now()
                    + if self.slots.iter().any(|s| s.connected) {
                        RECONNECT * 12
                    } else {
                        RECONNECT
                    },
            );
        }
    }

    fn query(&mut self, slot: usize, code: u32) {
        match self.radio.request(slot, code, |_| {}) {
            Ok(serial) => {
                self.pending.insert(serial, Pending::Query);
            }
            Err(e) => self.radio_failed(slot, e),
        }
    }

    fn radio_failed(&mut self, slot: usize, e: crate::binder::Error) {
        log(format!("{} request failed: {e}", SLOTS[slot]));
        if matches!(e, crate::binder::Error::Dead) {
            self.slot_lost(slot);
        }
    }

    fn slot_lost(&mut self, slot: usize) {
        if self.slots[slot].connected {
            log(format!("{} lost (rild died?)", SLOTS[slot]));
        }
        self.slots[slot] = SlotState::default();
        self.radio.slots[slot].handle = None;
        // IRadioConfig lives in the same rild.
        if let Some(handle) = self.radio.config.take() {
            let _ = self.radio.binder.release(handle);
        }
        if self.reconnect_at.is_none() {
            self.reconnect_at = Some(Instant::now() + RECONNECT);
        }
    }

    // ---- radio events ----------------------------------------------------

    fn handle_radio_events(&mut self) {
        for slot in self.radio.take_dead_slots() {
            self.slot_lost(slot);
        }
        for event in self.radio.take_events() {
            match event {
                Event::AckWanted { slot } => {
                    if let Err(e) = self.radio.acknowledge(slot) {
                        self.radio_failed(slot, e);
                    }
                }
                Event::Indication { slot, indication } => self.indication(slot, indication),
                Event::ConfigResponse {
                    body: Response::SimSlots(slots),
                    ..
                }
                | Event::SimSlotsChanged(slots) => {
                    self.sim_slots = slots;
                }
                Event::ConfigResponse { .. } => {}
                Event::Response {
                    slot,
                    code,
                    serial,
                    error,
                    body,
                } => self.response(slot, code, serial, error, body),
            }
        }
    }

    fn indication(&mut self, slot: usize, indication: Indication) {
        match indication {
            Indication::RadioState(state) => {
                self.slots[slot].radio = Some(state);
                if state == 10 {
                    self.query(slot, req::GET_ICC_CARD_STATUS);
                    self.query(slot, req::GET_VOICE_REGISTRATION_STATE);
                    self.query(slot, req::GET_DATA_REGISTRATION_STATE);
                }
            }
            Indication::SimStatusChanged => self.query(slot, req::GET_ICC_CARD_STATUS),
            Indication::NetworkStateChanged | Indication::VoiceRadioTech(_) => {
                self.query(slot, req::GET_VOICE_REGISTRATION_STATE);
                self.query(slot, req::GET_DATA_REGISTRATION_STATE);
                self.query(slot, req::GET_OPERATOR);
            }
            Indication::ImsNetworkStateChanged => self.query(slot, req::GET_IMS_REGISTRATION_STATE),
            Indication::Signal(signal) => self.slots[slot].signal = Some(signal),
            Indication::CallStateChanged | Indication::CallRing => {
                self.query(slot, req::GET_CURRENT_CALLS)
            }
            Indication::NewSms(pdu) => self.incoming_sms(slot, &pdu),
            Indication::NewSmsStatusReport(pdu) => self.status_report(slot, &pdu),
            Indication::DataCallList(calls) => self.data_calls_changed(calls),
            Indication::Ussd { message, .. } => {
                self.broadcast(json!({"event": "ussd", "message": message}))
            }
            Indication::ModemReset(reason) => log(format!("{} modem reset: {reason}", SLOTS[slot])),
            Indication::RilConnected => {}
            _ => {}
        }
    }

    fn response(&mut self, slot: usize, code: u32, serial: i32, error: i32, body: Response) {
        let pending = self.pending.remove(&serial);
        match (pending, body) {
            (Some(Pending::Client { client, id, op }), body) => {
                self.client_response(client, id, op, error, body)
            }
            (Some(Pending::SmsPart { message }), Response::SmsSent { message_ref, .. }) => {
                self.sms_part_sent(slot, message, error, message_ref)
            }
            (Some(Pending::SmsPart { message }), _) => {
                self.sms_part_sent(slot, message, error.max(2), -1)
            }
            (Some(Pending::DataSetup), body) => self.data_setup_done(error, body),
            (Some(Pending::DataDeactivate), _) => {
                self.data_hook_down();
                self.data.state = DataState::Off;
                self.data.call = None;
                self.data_reconcile();
            }
            (_, body) if error == radio::error::NONE => self.state_response(slot, code, body),
            (_, _) => {}
        }
    }

    fn state_response(&mut self, slot: usize, code: u32, body: Response) {
        let s = &mut self.slots[slot];
        match body {
            Response::CardStatus(card) => {
                if s.card.as_ref() != Some(&card) {
                    log(format!(
                        "{} card state {} app {:?}",
                        SLOTS[slot],
                        card.card_state,
                        card.gsm_app().map(|a| a.app_state)
                    ));
                }
                s.card = Some(card);
                self.data_reconcile();
            }
            Response::Registration(reg) => {
                // Voice and data responses share the variant; tell them apart
                // by the code of the pending query.
                if code == 20 {
                    s.voice = Some(reg);
                } else {
                    s.data = Some(reg);
                    self.data_reconcile();
                }
            }
            Response::Texts(names) => s.operator = Some(names),
            Response::Signal(signal) => s.signal = Some(signal),
            Response::Ims { registered, .. } => s.ims = Some(registered),
            Response::Calls(calls) if Some(slot) == self.main_slot() || !calls.is_empty() => {
                self.calls_changed(calls)
            }
            Response::DataCalls(calls) => self.data_calls_changed(calls),
            _ => {}
        }
    }

    // ---- state -----------------------------------------------------------

    /// The physical slots as IRadioConfig reports them (eUICC = has an EID).
    fn slots_json(&self) -> Vec<Value> {
        self.sim_slots
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let card = match p.card_state {
                    0 => "absent",
                    1 => "present",
                    2 => "error",
                    3 => "restricted",
                    _ => "unknown",
                };
                json!({"physical": i + 1, "logical": p.logical_slot + 1, "card": card,
                       "esim": !p.eid.is_empty(), "active": p.active})
            })
            .collect()
    }

    fn sim_json(&self) -> Value {
        let Some(slot) = self.main_slot() else {
            return json!({"slot": null, "state": "unknown", "pin_retries": self.pin_retries, "slots": self.slots_json()});
        };
        let euicc = self
            .sim_slots
            .iter()
            .any(|p| p.active && p.logical_slot == slot as i32 && !p.eid.is_empty());
        let s = &self.slots[slot];
        let state = match &s.card {
            None => "unknown",
            Some(card) => match card.card_state {
                0 => "absent",
                2 => "error",
                3 => "restricted",
                _ => match card.gsm_app().map(|a| a.app_state) {
                    Some(2) => "pin_required",
                    Some(3) => "puk_required",
                    Some(4) => "network_locked",
                    Some(5) => "ready",
                    _ if card.universal_pin_state == 4 => "puk_required",
                    None if card.apps.is_empty() && euicc => "no_profile",
                    _ => "not_ready",
                },
            },
        };
        json!({"slot": slot + 1, "state": state, "pin_retries": self.pin_retries, "slots": self.slots_json()})
    }

    fn sim_ready(&self) -> bool {
        self.sim_json()["state"] == "ready"
    }

    fn state_json(&self) -> Value {
        let main = self.main_slot();
        let s = main.map(|i| &self.slots[i]);
        let reg_name = |r: Option<&Registration>| match r.map(|r| r.state) {
            None => "unknown",
            Some(1) => "home",
            Some(5) => "roaming",
            Some(2) | Some(12) => "searching",
            Some(3) | Some(13) => "denied",
            Some(0) | Some(10) => "none",
            Some(_) => "unknown",
        };
        let voice = s.and_then(|s| s.voice.as_ref());
        let data = s.and_then(|s| s.data.as_ref());
        let rat = data
            .filter(|r| r.registered())
            .or(voice)
            .map(|r| rat_name(r.rat));
        let operator = s.and_then(|s| s.operator.as_ref());
        let signal = s.and_then(|s| s.signal.as_ref());
        let radio = match s.and_then(|s| s.radio) {
            Some(0) => "off",
            Some(1) => "unavailable",
            Some(10) => "on",
            _ => "unknown",
        };
        json!({
            "available": s.is_some(),
            "radio": radio,
            "airplane": self.settings.airplane,
            "sim": self.sim_json(),
            "network": {
                "registration": reg_name(voice),
                // Not registered, but the modem says emergency calls work
                // (the *_EM registration states).
                "emergency_only": voice.is_some_and(|r| matches!(r.state, 10 | 12 | 13 | 14)),
                "data_registration": reg_name(data),
                "operator": operator.and_then(|o| o.first().or(o.get(1))).filter(|n| !n.is_empty()),
                "numeric": operator.and_then(|o| o.get(2)).filter(|n| !n.is_empty()),
                "rat": rat,
                "ims_registered": s.and_then(|s| s.ims),
                "cell": voice.or(data).and_then(|r| r.cell.as_ref()).map(|c| json!({"mcc": c.mcc, "mnc": c.mnc, "area": c.area, "id": c.id})),
            },
            "signal": signal.map(|g| json!({"level": g.level(), "dbm": g.lte_dbm().or(g.gw_dbm())})),
            "data": self.data_json(),
            "calls": self.calls_json(),
            "sms": {"unread": self.sms.unread()},
        })
    }

    fn data_json(&self) -> Value {
        let state = match self.data.state {
            DataState::Off => "off",
            DataState::Connecting => "connecting",
            DataState::Connected => "connected",
            DataState::Disconnecting => "disconnecting",
            DataState::Failed => "failed",
        };
        let call = self.data.call.as_ref();
        let split = |s: &str| s.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
        json!({
            "enabled": self.settings.data_enabled,
            "state": state,
            "apn": self.settings.apn.apn,
            "interface": call.map(|c| c.ifname.clone()),
            "addresses": call.map(|c| split(&c.addresses)).unwrap_or_default(),
            "dns": call.map(|c| split(&c.dnses)).unwrap_or_default(),
            "error": self.data.error,
        })
    }

    /// Broadcasts `state_changed` when anything but a small dBm change
    /// moved; those are coalesced to one per SIGNAL_EVENT_INTERVAL.
    fn broadcast_state_if_changed(&mut self) {
        let state = self.state_json();
        if state == self.last_state {
            return;
        }
        let mut without_dbm = state.clone();
        let mut last_without_dbm = self.last_state.clone();
        for v in [&mut without_dbm, &mut last_without_dbm] {
            if let Some(signal) = v.get_mut("signal").and_then(Value::as_object_mut) {
                signal.remove("dbm");
            }
        }
        if without_dbm == last_without_dbm
            && self.last_signal_event.elapsed() < SIGNAL_EVENT_INTERVAL
        {
            return;
        }
        self.last_signal_event = Instant::now();
        self.last_state = state.clone();
        self.broadcast(json!({"event": "state_changed", "state": state}));
    }

    // ---- calls -----------------------------------------------------------

    fn calls_json(&self) -> Vec<Value> {
        self.calls.iter().map(call_json).collect()
    }

    fn calls_changed(&mut self, calls: Vec<radio::Call>) {
        let now = now_s();
        let mut ended = Vec::new();
        self.calls.retain(|t| {
            let alive = calls
                .iter()
                .any(|c| c.index == t.call.index && c.incoming == t.call.incoming);
            if !alive {
                ended.push((t.id, t.call.clone(), t.started, t.connected));
            }
            alive
        });
        let mut new_incoming = Vec::new();
        for call in calls {
            match self
                .calls
                .iter_mut()
                .find(|t| t.call.index == call.index && t.call.incoming == call.incoming)
            {
                Some(t) => {
                    if call.state == 0 && t.connected.is_none() {
                        t.connected = Some(now);
                    }
                    t.call = call;
                }
                None => {
                    let id = self.next_call;
                    self.next_call += 1;
                    let incoming = call.incoming && matches!(call.state, 4 | 5);
                    self.calls.push(TrackedCall {
                        id,
                        connected: (call.state == 0).then_some(now),
                        call,
                        started: now,
                        announced: false,
                    });
                    if incoming {
                        new_incoming.push(id);
                    }
                }
            }
        }
        let mut history = Vec::new();
        for (id, call, started, connected) in ended {
            let rejected = self.rejected.contains(&id);
            self.rejected.retain(|r| *r != id);
            let missed = call.incoming && connected.is_none() && !rejected;
            self.call_log.records.push(CallRecord {
                id,
                number: (call.number_presentation == 0 && !call.number.is_empty())
                    .then(|| store::normalize_peer(&call.number)),
                name: (!call.name.is_empty()).then(|| call.name.clone()),
                incoming: call.incoming,
                started_at: started,
                connected_at: connected,
                ended_at: now,
                missed,
                rejected,
            });
            history.push(json!(format!("c{id}")));
            let duration = connected.map_or(0, |c| now - c);
            log(format!("call c{id} ended (missed={missed}, {duration}s)"));
            let mut ended_json = call_json(&TrackedCall {
                id,
                call: call.clone(),
                started,
                connected,
                announced: true,
            });
            ended_json["state"] = json!("ended");
            self.broadcast(json!({"event": "call_ended", "call": ended_json, "duration_s": duration, "missed": missed}));
            self.notifier.withdraw("phone", &format!("call-{id}"));
            if missed {
                let who = display_number(&call);
                self.notifier.post(json!({
                    "app": "phone", "tag": format!("missed-{}", store::normalize_peer(&who)),
                    "title": "Missed call", "body": who, "urgency": "normal",
                    "open": {"app": "phone", "target": format!("call:{}", store::normalize_peer(&call.number))},
                }));
            }
        }
        if !history.is_empty() {
            if let Err(e) = self.call_log.save() {
                log(format!("cannot save call history: {e}"));
            }
            self.broadcast(json!({"event": "call_history_changed", "ids": history}));
        }
        for id in new_incoming {
            let Some(t) = self.calls.iter_mut().find(|t| t.id == id) else {
                continue;
            };
            t.announced = true;
            let call = call_json(t);
            let who = display_number(&t.call);
            log(format!("incoming call c{id}"));
            self.broadcast(json!({"event": "call_incoming", "call": call}));
            let mut post = json!({
                "app": "phone", "tag": format!("call-{id}"), "title": "Incoming call", "body": who,
                "urgency": "high", "open": {"app": "phone", "target": format!("c{id}")},
            });
            // moon-notifyd r3 loops the ringtone until we withdraw it.
            if self.notifier.revision.is_some_and(|r| r >= 3) {
                post["ringing"] = json!(true);
            }
            self.notifier.post(post);
        }
        // An answered call stops ringing.
        let answered: Vec<u64> = self
            .calls
            .iter()
            .filter(|t| t.call.state == 0 && t.call.incoming)
            .map(|t| t.id)
            .collect();
        for id in answered {
            self.notifier.withdraw("phone", &format!("call-{id}"));
        }
        let calls = self.calls_json();
        self.broadcast(json!({"event": "calls_changed", "calls": calls}));
        self.update_audio();
    }

    fn update_audio(&mut self) {
        let mode = if self
            .calls
            .iter()
            .any(|t| matches!(t.call.state, 0 | 1 | 2 | 3))
        {
            "in_call"
        } else if self.calls.iter().any(|t| matches!(t.call.state, 4 | 5)) {
            "ringtone"
        } else {
            "normal"
        };
        if mode != self.audio_mode {
            self.audio_mode = mode;
            if mode == "normal" {
                self.speaker = false;
                self.mute = false;
            }
            self.call_audio(&["mode", mode]);
        }
    }

    fn call_audio(&self, arguments: &[&str]) {
        let helper = self.paths.libexec.join("moon-call-audio");
        match Command::new(&helper)
            .args(arguments)
            .stdin(Stdio::null())
            .spawn()
        {
            // Reaped by SIGCHLD=SIG_IGN? No: reap below, the helper is quick.
            Ok(mut child) => {
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
            Err(e) => log(format!("call audio helper {}: {e}", helper.display())),
        }
    }

    // ---- SMS -------------------------------------------------------------

    fn incoming_sms(&mut self, slot: usize, pdu: &[u8]) {
        let mut stored = true;
        match sms::decode(pdu) {
            Some(sms::Incoming::Deliver(d)) => match d.concat.clone() {
                None => stored = self.store_incoming(&d.sender, d.text, d.sent_at),
                Some(c) => {
                    let key = (store::normalize_peer(&d.sender), c.reference, c.total);
                    let entry = self.concat.entry(key.clone()).or_insert_with(|| Concat {
                        parts: vec![None; c.total as usize],
                        sent_at: d.sent_at,
                        first: Instant::now(),
                    });
                    entry.parts[c.sequence as usize - 1] = Some(d.text);
                    if entry.parts.iter().all(Option::is_some) {
                        let entry = self.concat.remove(&key).unwrap();
                        let text: String = entry.parts.into_iter().flatten().collect();
                        stored = self.store_incoming(&d.sender, text, entry.sent_at);
                    }
                }
            },
            Some(sms::Incoming::StatusReport(_)) => {
                self.status_report(slot, pdu);
                return;
            }
            None => log(format!(
                "undecodable SMS PDU ({} bytes) acknowledged",
                pdu.len()
            )),
        }
        let (success, cause) = if stored {
            (true, 0)
        } else {
            (false, SMS_ACK_MEMORY_FULL)
        };
        match self
            .radio
            .request(slot, req::ACKNOWLEDGE_LAST_INCOMING_GSM_SMS, |p| {
                p.write_bool(success);
                p.write_i32(cause);
            }) {
            Ok(serial) => {
                self.pending.insert(serial, Pending::Query);
            }
            Err(e) => self.radio_failed(slot, e),
        }
    }

    /// Stores and announces an incoming message; false if it could not be
    /// saved (the network will then retry it).
    fn store_incoming(&mut self, sender: &str, text: String, sent_at: Option<i64>) -> bool {
        let peer = store::normalize_peer(sender);
        let message = self
            .sms
            .add(Message {
                id: 0,
                peer: peer.clone(),
                incoming: true,
                text,
                time: sent_at.unwrap_or_else(now_s),
                status: "received".into(),
                read: false,
                error: None,
                refs: Vec::new(),
            })
            .clone();
        if let Err(e) = self.sms.save() {
            log(format!("cannot save SMS store: {e}"));
            self.sms.messages.pop();
            return false;
        }
        log(format!("SMS m{} received", message.id));
        self.broadcast(json!({"event": "sms_received", "message": message.to_json()}));
        let unread = self
            .sms
            .messages
            .iter()
            .filter(|m| m.peer == peer && m.incoming && !m.read)
            .count();
        let body: String = message.text.chars().take(200).collect();
        self.notifier.post(json!({
            "app": "messages", "tag": format!("sms-{}", tag_safe(&peer)), "title": peer,
            "body": if unread > 1 { format!("{body}\n({unread} new messages)") } else { body },
            "urgency": "normal", "open": {"app": "messages", "target": format!("thread:{peer}")},
        }));
        true
    }

    fn flush_concat(&mut self, all: bool) {
        let keys: Vec<_> = self
            .concat
            .iter()
            .filter(|(_, c)| all || c.first.elapsed() >= CONCAT_TIMEOUT)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            let entry = self.concat.remove(&key).unwrap();
            let text: String = entry
                .parts
                .into_iter()
                .map(|p| p.unwrap_or_else(|| "…".into()))
                .collect();
            self.store_incoming(&key.0, text, entry.sent_at);
        }
    }

    fn status_report(&mut self, slot: usize, pdu: &[u8]) {
        if let Some(sms::Incoming::StatusReport(r)) = sms::decode(pdu) {
            let delivered = r.status < 0x20;
            let failed = r.status >= 0x40;
            let peer = store::normalize_peer(&r.recipient);
            let found = self.sms.messages.iter_mut().rev().find(|m| {
                !m.incoming && m.peer == peer && m.refs.contains(&(r.message_ref as i32))
            });
            if let Some(m) = found {
                m.refs.retain(|x| *x != r.message_ref as i32);
                if failed {
                    m.status = "failed".into();
                    m.error = Some(format!("not delivered (status 0x{:02x})", r.status));
                } else if delivered && m.refs.is_empty() && m.status == "sent" {
                    m.status = "delivered".into();
                }
                let json = m.to_json();
                let _ = self.sms.save();
                self.broadcast(json!({"event": "sms_updated", "message": json}));
            }
        }
        match self
            .radio
            .request(slot, req::ACKNOWLEDGE_LAST_INCOMING_GSM_SMS, |p| {
                p.write_bool(true);
                p.write_i32(0);
            }) {
            Ok(serial) => {
                self.pending.insert(serial, Pending::Query);
            }
            Err(e) => self.radio_failed(slot, e),
        }
    }

    fn send_next_part(&mut self) {
        if self.sending {
            return;
        }
        let Some(slot) = self.main_slot() else { return };
        let Some(out) = self.outgoing.front_mut() else {
            return;
        };
        let Some(part) = out.parts.pop_front() else {
            self.outgoing.pop_front();
            return self.send_next_part();
        };
        let more = !out.parts.is_empty();
        let message = out.message;
        let code = if more {
            req::SEND_SMS_EXPECT_MORE
        } else {
            req::SEND_SMS
        };
        match self
            .radio
            .request(slot, code, |p| args::gsm_sms(p, "", &sms::hex(&part)))
        {
            Ok(serial) => {
                self.sending = true;
                self.pending.insert(serial, Pending::SmsPart { message });
            }
            Err(e) => {
                self.radio_failed(slot, e);
                self.sms_failed(message, "radio unavailable".into());
            }
        }
    }

    fn sms_failed(&mut self, message: u64, error: String) {
        self.outgoing.retain(|o| o.message != message);
        if let Some(m) = self.sms.get_mut(message) {
            m.status = "failed".into();
            m.error = Some(error);
            let json = m.to_json();
            let _ = self.sms.save();
            self.broadcast(json!({"event": "sms_updated", "message": json}));
        }
        self.sending = false;
        self.send_next_part();
    }

    fn sms_part_sent(&mut self, _slot: usize, message: u64, error: i32, message_ref: i32) {
        self.sending = false;
        if error != radio::error::NONE {
            log(format!(
                "SMS m{message} failed: {}",
                radio::error_name(error)
            ));
            return self.sms_failed(message, radio::error_name(error).into());
        }
        let done = self
            .outgoing
            .front()
            .is_none_or(|o| o.message != message || o.parts.is_empty());
        if let Some(m) = self.sms.get_mut(message) {
            m.refs.push(message_ref);
            if done {
                m.status = "sent".into();
                let json = m.to_json();
                let _ = self.sms.save();
                log(format!("SMS m{message} sent"));
                self.broadcast(json!({"event": "sms_updated", "message": json}));
            }
        }
        if done {
            self.outgoing.retain(|o| o.message != message);
        }
        self.send_next_part();
    }

    // ---- mobile data -----------------------------------------------------

    fn data_wanted(&self) -> bool {
        let registered = self
            .main_slot()
            .and_then(|i| self.slots[i].data.as_ref())
            .is_some_and(Registration::registered);
        self.settings.data_enabled && !self.settings.airplane && self.sim_ready() && registered
    }

    /// Moves data towards what the settings and the network allow.
    fn data_reconcile(&mut self) {
        let Some(slot) = self.main_slot() else { return };
        match self.data.state {
            DataState::Off | DataState::Failed
                if self.data_wanted() && self.data.retry_at.is_none() =>
            {
                let apn = self.settings.apn.clone();
                let rat = self.slots[slot].data.as_ref().map_or(14, |r| r.rat);
                let result = self.radio.request(slot, req::SETUP_DATA_CALL, |p| {
                    p.write_i32(rat); // RadioTechnology (ril_service adds the legacy +2)
                    args::data_profile(
                        p,
                        &args::Apn {
                            apn: &apn.apn,
                            protocol: &apn.protocol,
                            user: &apn.user,
                            password: &apn.password,
                            auth: apn.auth,
                            types: 0x1 | 0x4 | 0x100,
                        },
                    );
                    p.write_bool(false); // modemCognitive
                    p.write_bool(false); // roamingAllowed
                    p.write_bool(false); // isRoaming
                });
                match result {
                    Ok(serial) => {
                        log(format!("data: setting up APN {}", apn.apn));
                        self.pending.insert(serial, Pending::DataSetup);
                        self.data.state = DataState::Connecting;
                        self.data.error = None;
                    }
                    Err(e) => self.radio_failed(slot, e),
                }
            }
            DataState::Connected if !self.data_wanted() && !self.settings.data_enabled => {
                let cid = self.data.call.as_ref().map_or(0, |c| c.cid);
                match self.radio.request(slot, req::DEACTIVATE_DATA_CALL, |p| {
                    p.write_i32(cid);
                    p.write_bool(false);
                }) {
                    Ok(serial) => {
                        self.pending.insert(serial, Pending::DataDeactivate);
                        self.data.state = DataState::Disconnecting;
                    }
                    Err(e) => self.radio_failed(slot, e),
                }
            }
            _ => {}
        }
    }

    fn data_setup_done(&mut self, error: i32, body: Response) {
        let call = match body {
            Response::DataCall(c) => Some(c),
            _ => None,
        };
        match call {
            Some(call)
                if error == 0
                    && call.status == 0
                    && call.active != 0
                    && !call.ifname.is_empty() =>
            {
                log(format!("data: up on {} ({})", call.ifname, call.addresses));
                self.data.backoff = Duration::ZERO;
                self.data.state = DataState::Connected;
                self.data.call = Some(call);
                self.data_hook_up();
                if !self.settings.data_enabled {
                    self.data_reconcile();
                }
            }
            other => {
                let reason = match &other {
                    Some(c) if error == 0 => format!("setup failed (cause {})", c.status),
                    _ => format!("setup failed ({})", radio::error_name(error)),
                };
                log(format!("data: {reason}"));
                self.data.state = DataState::Failed;
                self.data.error = Some(reason);
                self.data.backoff =
                    (self.data.backoff * 2).clamp(Duration::from_secs(10), DATA_RETRY_MAX);
                self.data.retry_at = Some(Instant::now() + self.data.backoff);
            }
        }
    }

    fn data_calls_changed(&mut self, calls: Vec<radio::DataCall>) {
        let Some(ours) = self.data.call.clone() else {
            return;
        };
        match calls.into_iter().find(|c| c.cid == ours.cid) {
            Some(c) if c.active != 0 => {
                if c.addresses != ours.addresses || c.ifname != ours.ifname {
                    self.data_hook_down();
                    self.data.call = Some(c);
                    self.data_hook_up();
                }
            }
            _ => {
                log("data: connection lost");
                self.data_hook_down();
                self.data.call = None;
                self.data.state = DataState::Off;
                self.data.retry_at = Some(Instant::now() + Duration::from_secs(5));
            }
        }
    }

    fn data_hook(&self, arguments: &[String]) {
        let hook = self.paths.libexec.join("mobile-data");
        match Command::new(&hook)
            .args(arguments)
            .stdin(Stdio::null())
            .status()
        {
            Ok(status) if status.success() => {}
            Ok(status) => log(format!("data hook {arguments:?} exited {status}")),
            Err(e) => log(format!("data hook {}: {e}", hook.display())),
        }
    }

    fn data_hook_up(&self) {
        let Some(c) = &self.data.call else { return };
        self.data_hook(&[
            "up".into(),
            c.ifname.clone(),
            c.mtu.to_string(),
            c.addresses.clone(),
            c.gateways.clone(),
            c.dnses.clone(),
        ]);
    }

    fn data_hook_down(&self) {
        if let Some(c) = &self.data.call {
            self.data_hook(&["down".into(), c.ifname.clone()]);
        }
    }

    // ---- clients ---------------------------------------------------------

    fn broadcast(&mut self, event: Value) {
        for client in self.clients.iter_mut().filter(|c| c.subscribed) {
            queue(client, &event);
        }
    }

    fn reply(&mut self, key: u64, id: u64, result: Result<Value, (&str, String)>) {
        let response = match result {
            Ok(result) => json!({"id": id, "ok": true, "result": result}),
            Err((code, message)) => {
                json!({"id": id, "ok": false, "error": {"code": code, "message": message}})
            }
        };
        if let Some(client) = self.clients.iter_mut().find(|c| c.key == key) {
            queue(client, &response);
        }
    }

    fn client_response(&mut self, client: u64, id: u64, op: Op, error: i32, body: Response) {
        if op == Op::Unlock {
            if let Response::Retries(n) = body {
                self.pin_retries = (n >= 0).then_some(n);
            }
            self.query_main(req::GET_ICC_CARD_STATUS);
        }
        if op == Op::Call {
            self.query_main(req::GET_CURRENT_CALLS);
        }
        let result = if error == radio::error::NONE {
            Ok(match body {
                Response::Channel { id, select } => {
                    json!({"channel": id, "select": sms::hex(&select)})
                }
                Response::IccIo { sw1, sw2, data } => {
                    json!({"response": format!("{data}{:02X}{:02X}", sw1 & 0xff, sw2 & 0xff)})
                }
                _ => json!({}),
            })
        } else {
            let mut message = radio::error_name(error).to_owned();
            if op == Op::Unlock
                && let Some(n) = self.pin_retries
            {
                message = format!("{message}; {n} attempts left");
            }
            Err(("radio_error", message))
        };
        self.reply(client, id, result);
    }

    fn query_main(&mut self, code: u32) {
        if let Some(slot) = self.main_slot() {
            self.query(slot, code);
        }
    }

    fn accept(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(true);
                    let peer = peer_uid(&stream, self.group);
                    let allowed = peer.is_some();
                    let mut client = Client {
                        key: self.next_client,
                        root: peer == Some(0),
                        stream,
                        input: Vec::new(),
                        output: VecDeque::new(),
                        subscribed: false,
                        closing: false,
                    };
                    self.next_client += 1;
                    if !allowed {
                        queue(
                            &mut client,
                            &json!({"id": null, "ok": false, "error": {"code": "permission_denied", "message": "root or moon-telephony group required"}}),
                        );
                        client.closing = true;
                    }
                    self.clients.push(client);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => {
                    log(format!("accept failed: {e}"));
                    break;
                }
            }
        }
    }

    fn read_client(&mut self, index: usize) {
        let mut buffer = [0u8; 8192];
        loop {
            let client = &mut self.clients[index];
            if client.closing {
                return;
            }
            match client.stream.read(&mut buffer) {
                Ok(0) => {
                    client.closing = true;
                    client.output.clear();
                    return;
                }
                Ok(n) => client.input.extend_from_slice(&buffer[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == ErrorKind::Interrupted => continue,
                Err(_) => {
                    client.closing = true;
                    client.output.clear();
                    return;
                }
            }
        }
        while let Some(end) = self.clients[index].input.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.clients[index].input.drain(..=end).collect();
            self.request(index, &line[..line.len() - 1]);
        }
        let client = &mut self.clients[index];
        if client.input.len() > MAX_LINE {
            client.closing = true;
            client.output.clear();
        }
    }

    fn request(&mut self, index: usize, line: &[u8]) {
        let key = self.clients[index].key;
        // serde_json keeps the last of duplicate keys silently; a repeated
        // "id" would then lose an argument without an error.
        if let Err(message) = crate::json_strict::check(line) {
            queue(
                &mut self.clients[index],
                &json!({"id": null, "ok": false, "error": {"code": "invalid_request", "message": message}}),
            );
            return;
        }
        let parsed = serde_json::from_slice::<Value>(line)
            .ok()
            .and_then(|value| match value {
                Value::Object(mut map) => {
                    let id = map.remove("id")?.as_u64().filter(|id| *id != 0)?;
                    let cmd = map.remove("cmd")?.as_str()?.to_owned();
                    Some((id, cmd, map))
                }
                _ => None,
            });
        let Some((id, cmd, args)) = parsed else {
            queue(
                &mut self.clients[index],
                &json!({"id": null, "ok": false, "error": {"code": "invalid_request", "message": "expected a JSON object with a non-zero id and cmd"}}),
            );
            return;
        };
        if let Some(result) = self.command(key, id, &cmd, &args) {
            self.reply(key, id, result);
        }
        if cmd == "subscribe" && args.is_empty() {
            let state = self.state_json();
            if let Some(client) = self.clients.iter_mut().find(|c| c.key == key) {
                client.subscribed = true;
                queue(
                    client,
                    &json!({"event": "snapshot", "state": state, "now": now_s()}),
                );
            }
        }
    }

    /// Runs a command. None: the reply comes later (radio response).
    fn command(
        &mut self,
        key: u64,
        id: u64,
        cmd: &str,
        a: &Map<String, Value>,
    ) -> Option<Result<Value, (&'static str, String)>> {
        let bad = |m: &str| Some(Err(("invalid_argument", m.to_owned())));
        let allowed: &[&str] = match cmd {
            "ping" | "subscribe" | "status" | "sms.threads" | "call.answer" | "call.reject"
            | "call.swap" | "data.get" => &[],
            "sim.unlock" => &["pin", "force"],
            "sms.send" => &["to", "text"],
            "sms.list" => &["peer", "before", "limit"],
            "sms.mark_read" => &["peer", "ids"],
            "sms.delete" => &["peer", "ids"],
            "call.dial" => &["number"],
            "call.hangup" => &["call"],
            "call.dtmf" => &["digits"],
            "call.audio" => &["speaker", "mute"],
            "call.history" => &["before", "limit"],
            "call.history_delete" => &["ids", "all"],
            "data.set" => &["enabled", "apn", "protocol", "user", "password", "auth"],
            "radio.set" => &["airplane"],
            "sim.channel_open" => &["aid", "p2", "slot"],
            "sim.apdu" => &["channel", "apdu", "slot"],
            "sim.channel_close" => &["channel", "slot"],
            _ => return Some(Err(("unknown_command", format!("unknown command {cmd:?}")))),
        };
        if let Some(extra) = a.keys().find(|k| !allowed.contains(&k.as_str())) {
            return bad(&format!("unexpected argument {extra:?}"));
        }
        let text = |k: &str| a.get(k).and_then(Value::as_str);
        let main = self.main_slot();
        let need_radio = || main.ok_or(("unavailable", "radio HAL not connected".to_owned()));
        match cmd {
            "ping" => Some(Ok(json!({"version": VERSION}))),
            "subscribe" => Some(Ok(json!({}))),
            "status" => Some(Ok(self.state_json())),
            "sim.unlock" => {
                let slot = match need_radio() {
                    Ok(s) => s,
                    Err(e) => return Some(Err(e)),
                };
                let Some(pin) = text("pin").filter(|p| {
                    (4..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_digit())
                }) else {
                    return bad("pin must be 4-8 digits");
                };
                if self.sim_json()["state"] != "pin_required" {
                    return Some(Err((
                        "invalid_state",
                        format!("SIM is {}, not waiting for a PIN", self.sim_json()["state"]),
                    )));
                }
                // A wrong PIN with one attempt left would PUK-lock the SIM.
                let force = a.get("force").and_then(Value::as_bool).unwrap_or(false);
                if self.pin_retries.is_some_and(|n| n <= 1) && !force {
                    return Some(Err((
                        "refused",
                        "one PIN attempt left; pass force=true to use it".into(),
                    )));
                }
                let aid = self.slots[slot]
                    .card
                    .as_ref()
                    .and_then(|c| c.gsm_app())
                    .map(|a| a.aid.clone())
                    .unwrap_or_default();
                let pin = pin.to_owned();
                self.radio_command(slot, key, id, Op::Unlock, req::SUPPLY_ICC_PIN, move |p| {
                    p.write_string(&pin);
                    p.write_string(&aid);
                })
            }
            "sms.send" => {
                let (Some(to), Some(body)) = (text("to"), text("text")) else {
                    return bad("to and text are required");
                };
                if body.chars().count() > MAX_TEXT {
                    return bad("text is too long");
                }
                let peer = store::normalize_peer(to);
                self.sms_reference = self.sms_reference.wrapping_add(1);
                let parts = match sms::encode_submit(&peer, body, self.sms_reference, true) {
                    Ok(parts) => parts,
                    Err(e) => return bad(&format!("cannot encode: {e:?}")),
                };
                if main.is_none() || !self.sim_ready() {
                    return Some(Err(("unavailable", "no SIM ready".into())));
                }
                let message = self
                    .sms
                    .add(Message {
                        id: 0,
                        peer,
                        incoming: false,
                        text: body.to_owned(),
                        time: now_s(),
                        status: "sending".into(),
                        read: true,
                        error: None,
                        refs: Vec::new(),
                    })
                    .clone();
                if let Err(e) = self.sms.save() {
                    self.sms.messages.pop();
                    return Some(Err(("internal", format!("cannot save: {e}"))));
                }
                self.outgoing.push_back(Outgoing {
                    message: message.id,
                    parts: parts.into_iter().map(|p| p.pdu).collect(),
                });
                self.send_next_part();
                self.broadcast(json!({"event": "sms_updated", "message": message.to_json()}));
                Some(Ok(json!({"message": message.to_json()})))
            }
            "sms.threads" => Some(Ok(json!({"threads": self.sms.threads()}))),
            "sms.list" => {
                let Some(peer) = text("peer") else {
                    return bad("peer is required");
                };
                let before = a.get("before").and_then(store::parse_id);
                let limit = a
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(50)
                    .clamp(1, 200) as usize;
                Some(Ok(json!({"messages": self.sms.list(peer, before, limit)})))
            }
            "sms.mark_read" | "sms.delete" => {
                let ids: Option<Vec<u64>> = a
                    .get("ids")
                    .and_then(Value::as_array)
                    .map(|v| v.iter().filter_map(store::parse_id).collect());
                let peer = text("peer").map(str::to_owned);
                if ids.is_none() && peer.is_none() {
                    return bad("peer or ids is required");
                }
                let hit = |m: &Message| {
                    ids.as_ref().is_some_and(|i| i.contains(&m.id))
                        || peer.as_deref() == Some(m.peer.as_str())
                };
                let mut changed = Vec::new();
                if cmd == "sms.delete" {
                    self.sms.messages.retain(|m| {
                        let gone = hit(m);
                        if gone {
                            changed.push(json!(format!("m{}", m.id)));
                        }
                        !gone
                    });
                } else {
                    for m in self.sms.messages.iter_mut().filter(|m| hit(m) && !m.read) {
                        m.read = true;
                        changed.push(json!(format!("m{}", m.id)));
                    }
                }
                if let Err(e) = self.sms.save() {
                    return Some(Err(("internal", format!("cannot save: {e}"))));
                }
                if !changed.is_empty() {
                    let event = if cmd == "sms.delete" {
                        "sms_deleted"
                    } else {
                        "sms_read"
                    };
                    self.broadcast(json!({"event": event, "ids": changed}));
                    if let Some(p) = &peer {
                        self.notifier
                            .withdraw("messages", &format!("sms-{}", tag_safe(p)));
                    }
                }
                Some(Ok(json!({"count": changed.len()})))
            }
            "call.dial" => {
                let slot = match need_radio() {
                    Ok(s) => s,
                    Err(e) => return Some(Err(e)),
                };
                let Some(number) = text("number").map(str::trim).filter(|n| {
                    !n.is_empty()
                        && n.len() <= 40
                        && n.chars().all(|c| c.is_ascii_digit() || "+*#,;".contains(c))
                }) else {
                    return bad("number must be digits, + * # , ;");
                };
                if self.calls.iter().any(|t| matches!(t.call.state, 2 | 3)) {
                    return Some(Err(("busy", "already dialing".into())));
                }
                let number = number.to_owned();
                self.radio_command(slot, key, id, Op::Call, req::DIAL, move |p| {
                    args::dial(p, &number)
                })
            }
            "call.answer" => {
                let slot = match need_radio() {
                    Ok(s) => s,
                    Err(e) => return Some(Err(e)),
                };
                if !self.calls.iter().any(|t| matches!(t.call.state, 4 | 5)) {
                    return Some(Err(("invalid_state", "no incoming call".into())));
                }
                // A waiting call is answered by holding the active one.
                let code = if self.calls.iter().any(|t| t.call.state == 0) {
                    req::SWITCH_WAITING_OR_HOLDING_AND_ACTIVE
                } else {
                    req::ACCEPT_CALL
                };
                self.radio_command(slot, key, id, Op::Call, code, |_| {})
            }
            "call.reject" => {
                let slot = match need_radio() {
                    Ok(s) => s,
                    Err(e) => return Some(Err(e)),
                };
                let ringing: Vec<u64> = self
                    .calls
                    .iter()
                    .filter(|t| matches!(t.call.state, 4 | 5))
                    .map(|t| t.id)
                    .collect();
                if ringing.is_empty() {
                    return Some(Err(("invalid_state", "no incoming call".into())));
                }
                self.rejected.extend(ringing);
                let code = if self.calls.iter().any(|t| t.call.state == 4) {
                    req::REJECT_CALL
                } else {
                    req::HANGUP_WAITING_OR_BACKGROUND
                };
                self.radio_command(slot, key, id, Op::Call, code, |_| {})
            }
            "call.swap" => {
                let slot = match need_radio() {
                    Ok(s) => s,
                    Err(e) => return Some(Err(e)),
                };
                self.radio_command(
                    slot,
                    key,
                    id,
                    Op::Call,
                    req::SWITCH_WAITING_OR_HOLDING_AND_ACTIVE,
                    |_| {},
                )
            }
            "call.hangup" => {
                let slot = match need_radio() {
                    Ok(s) => s,
                    Err(e) => return Some(Err(e)),
                };
                let target = match a.get("call").and_then(Value::as_str) {
                    Some(cid) => self.calls.iter().find(|t| format!("c{}", t.id) == cid),
                    None => self
                        .calls
                        .iter()
                        .find(|t| matches!(t.call.state, 0 | 2 | 3))
                        .or_else(|| self.calls.iter().find(|t| matches!(t.call.state, 4 | 5)))
                        .or_else(|| self.calls.first()),
                };
                let Some(t) = target else {
                    return Some(Err(("not_found", "no such call".into())));
                };
                if matches!(t.call.state, 4 | 5) {
                    self.rejected.push(t.id);
                }
                if matches!(t.call.state, 4) {
                    return self.radio_command(slot, key, id, Op::Call, req::REJECT_CALL, |_| {});
                }
                let index = t.call.index;
                self.radio_command(slot, key, id, Op::Call, req::HANGUP, move |p| {
                    p.write_i32(index)
                })
            }
            "call.dtmf" => {
                let slot = match need_radio() {
                    Ok(s) => s,
                    Err(e) => return Some(Err(e)),
                };
                let Some(digits) = text("digits").filter(|d| {
                    !d.is_empty()
                        && d.len() <= 32
                        && d.chars()
                            .all(|c| c.is_ascii_digit() || "*#ABCD".contains(c))
                }) else {
                    return bad("digits must be 0-9 * # A-D");
                };
                if !self.calls.iter().any(|t| t.call.state == 0) {
                    return Some(Err(("invalid_state", "no active call".into())));
                }
                let digits = digits.to_owned();
                self.radio_command(slot, key, id, Op::Call, req::SEND_DTMF, move |p| {
                    p.write_string(&digits)
                })
            }
            "call.history" => {
                let before = a.get("before").and_then(store::parse_call_id);
                let limit = a
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(50)
                    .clamp(1, 200) as usize;
                Some(Ok(json!({"calls": self.call_log.list(before, limit)})))
            }
            "call.history_delete" => {
                let all = a.get("all").and_then(Value::as_bool).unwrap_or(false);
                let ids: Option<Vec<u64>> = a
                    .get("ids")
                    .and_then(Value::as_array)
                    .map(|v| v.iter().filter_map(store::parse_call_id).collect());
                if !all && ids.is_none() {
                    return bad("ids or all=true is required");
                }
                let mut removed = Vec::new();
                self.call_log.records.retain(|r| {
                    let gone = all || ids.as_ref().is_some_and(|i| i.contains(&r.id));
                    if gone {
                        removed.push(json!(format!("c{}", r.id)));
                    }
                    !gone
                });
                if let Err(e) = self.call_log.save() {
                    return Some(Err(("internal", format!("cannot save: {e}"))));
                }
                if !removed.is_empty() {
                    self.broadcast(json!({"event": "call_history_changed", "ids": removed}));
                }
                Some(Ok(json!({"count": removed.len()})))
            }
            "call.audio" => {
                if let Some(on) = a.get("speaker").and_then(Value::as_bool) {
                    self.speaker = on;
                    self.call_audio(&["speaker", if on { "on" } else { "off" }]);
                }
                if let Some(on) = a.get("mute").and_then(Value::as_bool) {
                    self.mute = on;
                    self.call_audio(&["mute", if on { "on" } else { "off" }]);
                }
                Some(Ok(json!({"speaker": self.speaker, "mute": self.mute})))
            }
            "data.get" => Some(Ok(
                json!({"data": self.data_json(), "apn": self.settings.apn_json(false)}),
            )),
            "data.set" => {
                if let Some(on) = a.get("enabled") {
                    let Some(on) = on.as_bool() else {
                        return bad("enabled must be a boolean");
                    };
                    self.settings.data_enabled = on;
                }
                for (k, field) in [("apn", 0), ("protocol", 1), ("user", 2), ("password", 3)] {
                    if let Some(v) = a.get(k) {
                        let Some(v) = v.as_str().filter(|v| v.len() <= 100) else {
                            return bad(&format!("{k} must be a string"));
                        };
                        let target = match field {
                            0 => &mut self.settings.apn.apn,
                            1 => &mut self.settings.apn.protocol,
                            2 => &mut self.settings.apn.user,
                            _ => &mut self.settings.apn.password,
                        };
                        *target = v.to_owned();
                    }
                }
                if let Some(v) = a.get("auth") {
                    let Some(v) = v.as_i64().filter(|v| (0..=3).contains(v)) else {
                        return bad("auth must be 0-3");
                    };
                    self.settings.apn.auth = v as i32;
                }
                if let Err(e) = self
                    .settings
                    .save(&self.paths.state_dir.join("settings.json"))
                {
                    return Some(Err(("internal", format!("cannot save: {e}"))));
                }
                self.data.retry_at = None;
                self.data_reconcile();
                Some(Ok(
                    json!({"data": self.data_json(), "apn": self.settings.apn_json(false)}),
                ))
            }
            "sim.channel_open" | "sim.apdu" | "sim.channel_close" => {
                // Raw card access (eSIM management through lpac's stdio
                // driver): root only.
                if !self.clients.iter().any(|c| c.key == key && c.root) {
                    return Some(Err(("permission_denied", "root only".into())));
                }
                let slot = match a.get("slot").and_then(Value::as_u64) {
                    Some(n) if (1..=SLOTS.len() as u64).contains(&n) => n as usize - 1,
                    Some(_) => return bad("slot must be 1 or 2"),
                    None => match need_radio() {
                        Ok(s) => s,
                        Err(e) => return Some(Err(e)),
                    },
                };
                let hex = |k: &str| {
                    text(k)
                        .filter(|h| {
                            h.len() % 2 == 0
                                && h.len() <= 520
                                && h.bytes().all(|b| b.is_ascii_hexdigit())
                        })
                        .map(str::to_ascii_uppercase)
                };
                let channel = a.get("channel").and_then(Value::as_i64).map(|c| c as i32);
                match cmd {
                    "sim.channel_open" => {
                        let Some(aid) = hex("aid").filter(|a| a.len() >= 10) else {
                            return bad("aid must be 5-16 bytes of hex");
                        };
                        let p2 = a.get("p2").and_then(Value::as_i64).unwrap_or(0) as i32;
                        self.radio_command(
                            slot,
                            key,
                            id,
                            Op::ChannelOpen,
                            req::ICC_OPEN_LOGICAL_CHANNEL,
                            move |p| {
                                p.write_string(&aid);
                                p.write_i32(p2);
                            },
                        )
                    }
                    "sim.apdu" => {
                        let (Some(channel), Some(apdu)) = (channel, hex("apdu")) else {
                            return bad("channel and apdu (hex) are required");
                        };
                        let b = (0..apdu.len())
                            .step_by(2)
                            .map(|i| u8::from_str_radix(&apdu[i..i + 2], 16).unwrap())
                            .collect::<Vec<u8>>();
                        if b.len() < 4 {
                            return bad("apdu is shorter than its header");
                        }
                        // A 4-byte APDU has no P3 (p3 < 0); otherwise P3 is Lc
                        // (or Le) and the body follows.
                        let p3 = b.get(4).map_or(-1, |&v| v as i32);
                        let body_end = if b.len() > 5 {
                            (5 + b[4] as usize).min(b.len())
                        } else {
                            b.len()
                        };
                        let data = sms::hex(b.get(5..body_end).unwrap_or(&[]));
                        let header = [b[0] as i32, b[1] as i32, b[2] as i32, b[3] as i32, p3];
                        self.radio_command(
                            slot,
                            key,
                            id,
                            Op::Apdu,
                            req::ICC_TRANSMIT_APDU_LOGICAL_CHANNEL,
                            move |p| args::sim_apdu(p, channel, header, &data),
                        )
                    }
                    _ => {
                        let Some(channel) = channel else {
                            return bad("channel is required");
                        };
                        self.radio_command(
                            slot,
                            key,
                            id,
                            Op::ChannelClose,
                            req::ICC_CLOSE_LOGICAL_CHANNEL,
                            move |p| p.write_i32(channel),
                        )
                    }
                }
            }
            "radio.set" => {
                let slot = match need_radio() {
                    Ok(s) => s,
                    Err(e) => return Some(Err(e)),
                };
                let Some(airplane) = a.get("airplane").and_then(Value::as_bool) else {
                    return bad("airplane must be a boolean");
                };
                self.settings.airplane = airplane;
                if let Err(e) = self
                    .settings
                    .save(&self.paths.state_dir.join("settings.json"))
                {
                    return Some(Err(("internal", format!("cannot save: {e}"))));
                }
                self.radio_command(slot, key, id, Op::Radio, req::SET_RADIO_POWER, move |p| {
                    p.write_bool(!airplane)
                })
            }
            _ => unreachable!(),
        }
    }

    fn radio_command(
        &mut self,
        slot: usize,
        key: u64,
        id: u64,
        op: Op,
        code: u32,
        args: impl FnOnce(&mut crate::binder::Parcel),
    ) -> Option<Result<Value, (&'static str, String)>> {
        match self.radio.request(slot, code, args) {
            Ok(serial) => {
                self.pending.insert(
                    serial,
                    Pending::Client {
                        client: key,
                        id,
                        op,
                    },
                );
                None
            }
            Err(e) => {
                self.radio_failed(slot, e);
                Some(Err(("unavailable", "radio request failed".into())))
            }
        }
    }
}

fn rat_name(rat: i32) -> &'static str {
    match rat {
        1 | 2 | 16 => "gsm",
        3 | 9 | 10 | 11 | 15 | 17 => "umts",
        14 | 19 => "lte",
        20 => "nr",
        18 => "iwlan",
        _ => "unknown",
    }
}

fn display_number(call: &radio::Call) -> String {
    match call.number_presentation {
        1 => "Private number".into(),
        2 | 3 => "Unknown".into(),
        _ if call.number.is_empty() => "Unknown".into(),
        _ => store::normalize_peer(&call.number),
    }
}

fn call_json(t: &TrackedCall) -> Value {
    let state = match t.call.state {
        0 => "active",
        1 => "held",
        2 => "dialing",
        3 => "alerting",
        4 => "incoming",
        5 => "waiting",
        _ => "unknown",
    };
    let number = (t.call.number_presentation == 0 && !t.call.number.is_empty())
        .then(|| store::normalize_peer(&t.call.number));
    json!({
        "id": format!("c{}", t.id),
        "number": number,
        "name": (!t.call.name.is_empty()).then(|| t.call.name.clone()),
        "state": state,
        "direction": if t.call.incoming { "incoming" } else { "outgoing" },
        "multiparty": t.call.multiparty,
        "started_at": t.started,
        "connected_at": t.connected,
    })
}

/// Notification tags allow `[A-Za-z0-9._-]`.
fn tag_safe(peer: &str) -> String {
    peer.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(60)
        .collect()
}

fn pollfd(fd: RawFd, events: libc::c_short) -> libc::pollfd {
    libc::pollfd {
        fd,
        events,
        revents: 0,
    }
}

fn queue(client: &mut Client, value: &Value) {
    if client.closing {
        return;
    }
    if let Ok(mut line) = serde_json::to_vec(value) {
        line.push(b'\n');
        client.output.extend(line);
    }
    if client.output.len() > 4 * 1024 * 1024 {
        client.closing = true;
        client.output.clear();
    }
    flush(client);
}

fn flush(client: &mut Client) {
    while !client.output.is_empty() {
        let (front, _) = client.output.as_slices();
        match client.stream.write(front) {
            Ok(0) => break,
            Ok(n) => {
                client.output.drain(..n);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock => break,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => {
                client.closing = true;
                client.output.clear();
                break;
            }
        }
    }
}

fn bind(paths: &Paths, group: Option<u32>) -> io::Result<UnixListener> {
    fs::create_dir_all(&paths.run_dir)?;
    chown(&paths.run_dir, group)?;
    fs::set_permissions(&paths.run_dir, fs::Permissions::from_mode(0o750))?;
    match fs::remove_file(&paths.socket) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let listener = UnixListener::bind(&paths.socket)?;
    chown(&paths.socket, group)?;
    fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o660))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn chown(path: &Path, group: Option<u32>) -> io::Result<()> {
    if let Some(group) = group {
        std::os::unix::fs::chown(path, Some(0), Some(group))?;
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn peer_uid(_: &UnixStream, _: Option<u32>) -> Option<u32> {
    Some(0) // host builds are for tests only
}

/// The peer's uid if it may connect (root, our uid, or the group).
#[cfg(target_os = "linux")]
fn peer_uid(stream: &UnixStream, group: Option<u32>) -> Option<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: u32::MAX,
        gid: u32::MAX,
    };
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred is valid writable storage of `length` bytes.
    let rc = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut length,
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: geteuid has no preconditions.
    if cred.uid == 0 || cred.uid == unsafe { libc::geteuid() } {
        return Some(cred.uid);
    }
    let group = group?;
    if cred.gid == group {
        return Some(cred.uid);
    }
    fs::read_to_string(format!("/proc/{}/status", cred.pid))
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|line| line.strip_prefix("Groups:"))
                .map(|groups| groups.split_whitespace().any(|g| g.parse() == Ok(group)))
        })
        .unwrap_or(false)
        .then_some(cred.uid)
}

fn group_id(path: &Path, name: &str) -> Option<u32> {
    fs::read_to_string(path).ok()?.lines().find_map(|line| {
        let mut fields = line.split(':');
        (fields.next() == Some(name))
            .then(|| fields.nth(1)?.parse().ok())
            .flatten()
    })
}
