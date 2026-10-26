//! The vendor radio HAL (android.hardware.radio@1.x::IRadio, served by
//! Samsung's rild) as a client: requests, and decoding of the responses and
//! indications rild calls back. We register as an @1.0 client, so rild
//! answers with the @1.0 methods and structs (layouts below follow HIDL's C++
//! layout; libgbinder-radio's radio_types.h asserts the same sizes).

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use crate::binder::{Binder, Blob, Error, Handler, Incoming, Parcel, View};

pub const IRADIO: &str = "android.hardware.radio@1.0::IRadio";
const IRADIO_RESPONSE: &str = "android.hardware.radio@1.0::IRadioResponse";
const IRADIO_INDICATION: &str = "android.hardware.radio@1.0::IRadioIndication";
/// The newest IRadio the service must implement for us to use it.
pub const IRADIO_FQNAME: &str = "android.hardware.radio@1.0::IRadio";
/// The SIM slot/eUICC configuration HAL (one instance, "default").
pub const IRADIO_CONFIG: &str = "android.hardware.radio.config@1.0::IRadioConfig";
/// We take its @1.2 callbacks: getSimSlotsStatusResponse_1_2 carries the EID.
const CONFIG_RESPONSE_CHAIN: [&str; 3] = [
    "android.hardware.radio.config@1.2::IRadioConfigResponse",
    "android.hardware.radio.config@1.1::IRadioConfigResponse",
    "android.hardware.radio.config@1.0::IRadioConfigResponse",
];
const CONFIG_INDICATION_CHAIN: [&str; 3] = [
    "android.hardware.radio.config@1.2::IRadioConfigIndication",
    "android.hardware.radio.config@1.1::IRadioConfigIndication",
    "android.hardware.radio.config@1.0::IRadioConfigIndication",
];

/// IRadioConfig method codes (1.0 then 1.1).
pub mod config_req {
    pub const SET_RESPONSE_FUNCTIONS: u32 = 1;
    pub const GET_SIM_SLOTS_STATUS: u32 = 2;
    pub const GET_PHONE_CAPABILITY: u32 = 4;
}

/// IRadio@1.0 method codes (declaration order, from 1). The full set we
/// may use, not only what is used today.
#[allow(dead_code)]
pub mod req {
    pub const SET_RESPONSE_FUNCTIONS: u32 = 1;
    pub const GET_ICC_CARD_STATUS: u32 = 2;
    pub const SUPPLY_ICC_PIN: u32 = 3;
    pub const SUPPLY_ICC_PUK: u32 = 4;
    pub const GET_CURRENT_CALLS: u32 = 10;
    pub const DIAL: u32 = 11;
    pub const GET_IMSI: u32 = 12;
    pub const HANGUP: u32 = 13;
    pub const HANGUP_WAITING_OR_BACKGROUND: u32 = 14;
    pub const HANGUP_FOREGROUND_RESUME_BACKGROUND: u32 = 15;
    pub const SWITCH_WAITING_OR_HOLDING_AND_ACTIVE: u32 = 16;
    pub const REJECT_CALL: u32 = 18;
    pub const GET_LAST_CALL_FAIL_CAUSE: u32 = 19;
    pub const GET_SIGNAL_STRENGTH: u32 = 20;
    pub const GET_VOICE_REGISTRATION_STATE: u32 = 21;
    pub const GET_DATA_REGISTRATION_STATE: u32 = 22;
    pub const GET_OPERATOR: u32 = 23;
    pub const SET_RADIO_POWER: u32 = 24;
    pub const SEND_DTMF: u32 = 25;
    pub const SEND_SMS: u32 = 26;
    pub const SEND_SMS_EXPECT_MORE: u32 = 27;
    pub const SETUP_DATA_CALL: u32 = 28;
    pub const ACKNOWLEDGE_LAST_INCOMING_GSM_SMS: u32 = 38;
    pub const ACCEPT_CALL: u32 = 39;
    pub const DEACTIVATE_DATA_CALL: u32 = 40;
    pub const GET_FACILITY_LOCK: u32 = 41;
    pub const GET_BASEBAND_VERSION: u32 = 50;
    pub const SET_MUTE: u32 = 52;
    pub const GET_DATA_CALL_LIST: u32 = 55;
    pub const GET_DEVICE_IDENTITY: u32 = 89;
    pub const GET_SMSC_ADDRESS: u32 = 91;
    pub const REPORT_SMS_MEMORY_STATUS: u32 = 93;
    pub const GET_VOICE_RADIO_TECHNOLOGY: u32 = 99;
    pub const SET_INITIAL_ATTACH_APN: u32 = 102;
    pub const GET_IMS_REGISTRATION_STATE: u32 = 103;
    pub const SET_DATA_ALLOWED: u32 = 114;
    pub const ICC_TRANSMIT_APDU_BASIC_CHANNEL: u32 = 105;
    pub const ICC_OPEN_LOGICAL_CHANNEL: u32 = 106;
    pub const ICC_CLOSE_LOGICAL_CHANNEL: u32 = 107;
    pub const ICC_TRANSMIT_APDU_LOGICAL_CHANNEL: u32 = 108;
    pub const SET_INDICATION_FILTER: u32 = 128;
    pub const RESPONSE_ACKNOWLEDGEMENT: u32 = 130;
}

/// IRadioResponse@1.0 method codes.
mod resp {
    pub const GET_ICC_CARD_STATUS: u32 = 1;
    pub const SUPPLY_ICC_PIN: u32 = 2;
    pub const SUPPLY_ICC_PUK: u32 = 3;
    pub const GET_CURRENT_CALLS: u32 = 9;
    pub const GET_IMSI: u32 = 11;
    pub const GET_LAST_CALL_FAIL_CAUSE: u32 = 18;
    pub const GET_SIGNAL_STRENGTH: u32 = 19;
    pub const GET_VOICE_REGISTRATION_STATE: u32 = 20;
    pub const GET_DATA_REGISTRATION_STATE: u32 = 21;
    pub const GET_OPERATOR: u32 = 22;
    pub const SEND_SMS: u32 = 25;
    pub const SEND_SMS_EXPECT_MORE: u32 = 26;
    pub const SETUP_DATA_CALL: u32 = 27;
    pub const GET_FACILITY_LOCK: u32 = 40;
    pub const GET_BASEBAND_VERSION: u32 = 49;
    pub const GET_DATA_CALL_LIST: u32 = 54;
    pub const GET_DEVICE_IDENTITY: u32 = 88;
    pub const GET_SMSC_ADDRESS: u32 = 90;
    pub const GET_VOICE_RADIO_TECHNOLOGY: u32 = 98;
    pub const GET_IMS_REGISTRATION_STATE: u32 = 102;
    pub const ICC_TRANSMIT_APDU_BASIC_CHANNEL: u32 = 104;
    pub const ICC_OPEN_LOGICAL_CHANNEL: u32 = 105;
    pub const ICC_TRANSMIT_APDU_LOGICAL_CHANNEL: u32 = 107;
    pub const ACKNOWLEDGE_REQUEST: u32 = 129;
}

/// IRadioIndication@1.0 method codes.
mod ind {
    pub const RADIO_STATE_CHANGED: u32 = 1;
    pub const CALL_STATE_CHANGED: u32 = 2;
    pub const NETWORK_STATE_CHANGED: u32 = 3;
    pub const NEW_SMS: u32 = 4;
    pub const NEW_SMS_STATUS_REPORT: u32 = 5;
    pub const NEW_SMS_ON_SIM: u32 = 6;
    pub const ON_USSD: u32 = 7;
    pub const NITZ_TIME_RECEIVED: u32 = 8;
    pub const CURRENT_SIGNAL_STRENGTH: u32 = 9;
    pub const DATA_CALL_LIST_CHANGED: u32 = 10;
    pub const CALL_RING: u32 = 18;
    pub const SIM_STATUS_CHANGED: u32 = 19;
    pub const INDICATE_RINGBACK_TONE: u32 = 28;
    pub const RIL_CONNECTED: u32 = 33;
    pub const VOICE_RADIO_TECH_CHANGED: u32 = 34;
    pub const IMS_NETWORK_STATE_CHANGED: u32 = 36;
    pub const MODEM_RESET: u32 = 45;
}

/// RadioError values.
#[allow(dead_code)]
pub mod error {
    pub const NONE: i32 = 0;
    pub const RADIO_NOT_AVAILABLE: i32 = 1;
    pub const GENERIC_FAILURE: i32 = 2;
    pub const PASSWORD_INCORRECT: i32 = 3;
    pub const SIM_PIN2: i32 = 4;
    pub const SIM_PUK2: i32 = 5;
    pub const REQUEST_NOT_SUPPORTED: i32 = 6;
}

pub fn error_name(code: i32) -> &'static str {
    match code {
        0 => "none",
        1 => "radio_not_available",
        2 => "generic_failure",
        3 => "password_incorrect",
        6 => "request_not_supported",
        10 => "sms_send_fail_retry",
        11 => "sim_absent",
        14 => "fdn_check_failure",
        36 => "invalid_sms_format",
        37 => "no_memory",
        38 => "system_err",
        39 => "encoding_err",
        40 => "invalid_smsc_address",
        41 => "modem_err",
        42 => "network_err",
        43 => "internal_err",
        44 => "request_rate_limited",
        45 => "sim_busy",
        46 => "sim_full",
        47 => "network_reject",
        48 => "operation_not_allowed",
        49 => "empty_record",
        50 => "invalid_sms_format",
        51 => "invalid_arguments",
        52 => "invalid_state",
        53 => "no_resources",
        54 => "no_such_element",
        55 => "invalid_modem_state",
        56 => "invalid_call_id",
        57 => "no_sms_to_ack",
        58 => "network_not_ready",
        59 => "not_provisioned",
        60 => "no_subscription",
        61 => "no_network_found",
        62 => "device_in_use",
        63 => "aborted",
        64 => "invalid_response",
        _ => "other",
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppStatus {
    pub app_type: i32,
    pub app_state: i32,
    pub aid: String,
    pub label: String,
    pub pin1: i32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct CardStatus {
    pub card_state: i32,
    pub universal_pin_state: i32,
    pub gsm_umts_app: i32,
    pub apps: Vec<AppStatus>,
}

impl CardStatus {
    fn decode(v: View) -> Self {
        Self {
            card_state: v.i32(0),
            universal_pin_state: v.i32(4),
            gsm_umts_app: v.i32(8),
            apps: v
                .vec(24, 64)
                .into_iter()
                .map(|a| AppStatus {
                    app_type: a.i32(0),
                    app_state: a.i32(4),
                    aid: a.string(16),
                    label: a.string(32),
                    pin1: a.i32(52),
                })
                .collect(),
        }
    }
    /// The GSM/UMTS subscription application, if any.
    pub fn gsm_app(&self) -> Option<&AppStatus> {
        usize::try_from(self.gsm_umts_app)
            .ok()
            .and_then(|i| self.apps.get(i))
    }
}

/// SignalStrength@1.0, raw.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Signal {
    pub gsm_asu: u32,
    pub gsm_ber: u32,
    pub lte_asu: u32,
    pub lte_rsrp: u32,
    pub lte_rsrq: u32,
    pub lte_rssnr: i32,
    pub tdscdma_rscp: u32,
}

impl Signal {
    fn decode(v: View) -> Self {
        Self {
            gsm_asu: v.i32(0) as u32,
            gsm_ber: v.i32(4) as u32,
            lte_asu: v.i32(32) as u32,
            lte_rsrp: v.i32(36) as u32,
            lte_rsrq: v.i32(40) as u32,
            lte_rssnr: v.i32(44),
            tdscdma_rscp: v.i32(56) as u32,
        }
    }
    /// LTE RSRP in dBm, if reported (@1.0 sends it negated, 44..140).
    pub fn lte_dbm(&self) -> Option<i32> {
        (44..=140)
            .contains(&self.lte_rsrp)
            .then(|| -(self.lte_rsrp as i32))
    }
    /// GSM/UMTS RSSI in dBm from the ASU (0..31), if reported.
    pub fn gw_dbm(&self) -> Option<i32> {
        (self.gsm_asu <= 31).then(|| -113 + 2 * self.gsm_asu as i32)
    }
    /// Android's default 0–4 bars: LTE RSRP thresholds -115/-105/-95/-85,
    /// GSM/UMTS ASU thresholds 5/8/12 (2 or less is none).
    pub fn level(&self) -> u8 {
        if let Some(dbm) = self.lte_dbm() {
            return match dbm {
                d if d >= -85 => 4,
                d if d >= -95 => 3,
                d if d >= -105 => 2,
                d if d >= -115 => 1,
                _ => 0,
            };
        }
        match self.gsm_asu {
            99 | 0..=2 => 0,
            12.. => 4,
            8.. => 3,
            5.. => 2,
            _ => 1,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Cell {
    pub mcc: String,
    pub mnc: String,
    pub area: i32,
    pub id: i64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Registration {
    pub state: i32,
    pub rat: i32,
    pub reason: i32,
    pub cell: Option<Cell>,
}

impl Registration {
    fn cell(v: View) -> Option<Cell> {
        // CellIdentity@1.0: type, then vectors gsm@8 wcdma@24 cdma@40 lte@56
        // tdscdma@72. GSM/WCDMA/TD-SCDMA/LTE elements are 48 bytes with
        // mcc@0 mnc@16 and (lac|tac, cid|ci) at 32/36 (LTE: ci@32 tac@40).
        let first = |offset: usize| v.vec(offset, 48).into_iter().next();
        if let Some(c) = first(56) {
            return Some(Cell {
                mcc: c.string(0),
                mnc: c.string(16),
                area: c.i32(40),
                id: c.i32(32) as i64,
            });
        }
        [8, 24, 72].into_iter().find_map(first).map(|c| Cell {
            mcc: c.string(0),
            mnc: c.string(16),
            area: c.i32(32),
            id: c.i32(36) as i64,
        })
    }
    fn decode_voice(v: View) -> Self {
        Self {
            state: v.i32(0),
            rat: v.i32(4),
            reason: v.i32(24),
            cell: Self::cell(v.sub(32, 88)),
        }
    }
    fn decode_data(v: View) -> Self {
        Self {
            state: v.i32(0),
            rat: v.i32(4),
            reason: v.i32(8),
            cell: Self::cell(v.sub(16, 88)),
        }
    }
    pub fn registered(&self) -> bool {
        matches!(self.state, 1 | 5)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Call {
    pub state: i32,
    pub index: i32,
    pub toa: i32,
    pub multiparty: bool,
    pub incoming: bool,
    pub voice: bool,
    pub number: String,
    pub number_presentation: i32,
    pub name: String,
}

impl Call {
    fn decode(v: View) -> Self {
        Self {
            state: v.i32(0),
            index: v.i32(4),
            toa: v.i32(8),
            multiparty: v.u8(12) != 0,
            incoming: v.u8(13) != 0,
            voice: v.u8(15) != 0,
            number: v.string(24),
            number_presentation: v.i32(40),
            name: v.string(48),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DataCall {
    pub status: i32,
    pub retry_ms: i32,
    pub cid: i32,
    pub active: i32,
    pub protocol: String,
    pub ifname: String,
    pub addresses: String,
    pub dnses: String,
    pub gateways: String,
    pub pcscf: String,
    pub mtu: i32,
}

impl DataCall {
    fn decode(v: View) -> Self {
        Self {
            status: v.i32(0),
            retry_ms: v.i32(4),
            cid: v.i32(8),
            active: v.i32(12),
            protocol: v.string(16),
            ifname: v.string(32),
            addresses: v.string(48),
            dnses: v.string(64),
            gateways: v.string(80),
            pcscf: v.string(96),
            mtu: v.i32(112),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    Empty,
    CardStatus(CardStatus),
    /// Remaining PIN/PUK attempts.
    Retries(i32),
    Calls(Vec<Call>),
    Text(String),
    Texts(Vec<String>),
    Int(i32),
    LastCallFail {
        cause: i32,
        vendor: String,
    },
    Signal(Signal),
    Registration(Registration),
    SmsSent {
        message_ref: i32,
        error_code: i32,
    },
    DataCall(DataCall),
    DataCalls(Vec<DataCall>),
    Ims {
        registered: bool,
        family: i32,
    },
    /// iccOpenLogicalChannel: the channel and the SELECT response.
    Channel {
        id: i32,
        select: Vec<u8>,
    },
    /// IccIoResult (APDU responses): status words and hex data.
    IccIo {
        sw1: i32,
        sw2: i32,
        data: String,
    },
    SimSlots(Vec<SimSlot>),
    PhoneCapability {
        max_active_data: u8,
        max_active_internet: u8,
        dsda: bool,
    },
}

/// SimSlotStatus@1.2 (IRadioConfig): one physical slot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SimSlot {
    pub card_state: i32,
    pub active: bool,
    pub atr: String,
    pub logical_slot: i32,
    pub iccid: String,
    pub eid: String,
}

impl SimSlot {
    fn decode(v: View, with_eid: bool) -> Self {
        Self {
            card_state: v.i32(0),
            active: v.i32(4) == 1,
            atr: v.string(8),
            logical_slot: v.i32(24),
            iccid: v.string(32),
            eid: if with_eid {
                v.string(48)
            } else {
                String::new()
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Indication {
    RadioState(i32),
    CallStateChanged,
    NetworkStateChanged,
    NewSms(Vec<u8>),
    NewSmsStatusReport(Vec<u8>),
    NewSmsOnSim(i32),
    Ussd { mode: i32, message: String },
    Nitz { time: String, received_ms: i64 },
    Signal(Signal),
    DataCallList(Vec<DataCall>),
    CallRing,
    SimStatusChanged,
    RingbackTone(bool),
    RilConnected,
    VoiceRadioTech(i32),
    ImsNetworkStateChanged,
    ModemReset(String),
    Other(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Response {
        slot: usize,
        code: u32,
        serial: i32,
        error: i32,
        body: Response,
    },
    Indication {
        slot: usize,
        indication: Indication,
    },
    /// rild wants responseAcknowledgement() (it holds a wakelock until then).
    AckWanted {
        slot: usize,
    },
    /// An IRadioConfig response.
    ConfigResponse {
        serial: i32,
        error: i32,
        body: Response,
    },
    SimSlotsChanged(Vec<SimSlot>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Role {
    Response(usize),
    Indication(usize),
    ConfigResponse,
    ConfigIndication,
}

fn decode_config_response(code: u32, r: &mut crate::binder::Reader) -> Response {
    match code {
        // getSimSlotsStatusResponse (1.0) / _1_2: vec<SimSlotStatus>
        1 => r.buffer().map(|v| {
            Response::SimSlots(
                v.vec(0, 48)
                    .into_iter()
                    .map(|s| SimSlot::decode(s, false))
                    .collect(),
            )
        }),
        7 => r.buffer().map(|v| {
            Response::SimSlots(
                v.vec(0, 64)
                    .into_iter()
                    .map(|s| SimSlot::decode(s, true))
                    .collect(),
            )
        }),
        // getPhoneCapabilityResponse: PhoneCapability { uint8 maxActiveData;
        // uint8 maxActiveInternetData; bool isInternetLingeringSupported;
        // vec<ModemInfo> logicalModemList; }
        3 => r.buffer().map(|v| Response::PhoneCapability {
            max_active_data: v.u8(0),
            max_active_internet: v.u8(1),
            dsda: v.u8(0) > 1,
        }),
        _ => Some(Response::Empty),
    }
    .unwrap_or(Response::Empty)
}

fn decode_response(code: u32, r: &mut crate::binder::Reader) -> Response {
    let mut buffer = || r.buffer();
    match code {
        resp::GET_ICC_CARD_STATUS => buffer().map(CardStatus::decode).map(Response::CardStatus),
        resp::SUPPLY_ICC_PIN | resp::SUPPLY_ICC_PUK => r.i32().map(Response::Retries),
        resp::GET_CURRENT_CALLS => {
            buffer().map(|v| Response::Calls(v.vec(0, 88).into_iter().map(Call::decode).collect()))
        }
        resp::GET_IMSI | resp::GET_BASEBAND_VERSION | resp::GET_SMSC_ADDRESS => {
            r.string().map(Response::Text)
        }
        resp::GET_LAST_CALL_FAIL_CAUSE => buffer().map(|v| Response::LastCallFail {
            cause: v.i32(0),
            vendor: v.string(8),
        }),
        resp::GET_SIGNAL_STRENGTH => buffer().map(Signal::decode).map(Response::Signal),
        resp::GET_VOICE_REGISTRATION_STATE => {
            buffer().map(|v| Response::Registration(Registration::decode_voice(v)))
        }
        resp::GET_DATA_REGISTRATION_STATE => {
            buffer().map(|v| Response::Registration(Registration::decode_data(v)))
        }
        resp::GET_OPERATOR | resp::GET_DEVICE_IDENTITY => {
            let mut texts = Vec::new();
            while let Some(text) = r.string() {
                texts.push(text);
            }
            Some(Response::Texts(texts))
        }
        resp::SEND_SMS | resp::SEND_SMS_EXPECT_MORE => buffer().map(|v| Response::SmsSent {
            message_ref: v.i32(0),
            error_code: v.i32(24),
        }),
        resp::SETUP_DATA_CALL => buffer().map(DataCall::decode).map(Response::DataCall),
        resp::GET_DATA_CALL_LIST => buffer().map(|v| {
            Response::DataCalls(v.vec(0, 120).into_iter().map(DataCall::decode).collect())
        }),
        resp::GET_FACILITY_LOCK | resp::GET_VOICE_RADIO_TECHNOLOGY => r.i32().map(Response::Int),
        resp::ICC_OPEN_LOGICAL_CHANNEL => {
            let id = r.i32();
            let select = r.buffer().map(|v| v.byte_vec(0)).unwrap_or_default();
            id.map(|id| Response::Channel { id, select })
        }
        resp::ICC_TRANSMIT_APDU_BASIC_CHANNEL | resp::ICC_TRANSMIT_APDU_LOGICAL_CHANNEL => buffer()
            .map(|v| Response::IccIo {
                sw1: v.i32(0),
                sw2: v.i32(4),
                data: v.string(8),
            }),
        resp::GET_IMS_REGISTRATION_STATE => {
            let registered = r.bool();
            let family = r.i32();
            registered.map(|registered| Response::Ims {
                registered,
                family: family.unwrap_or(0),
            })
        }
        _ => Some(Response::Empty),
    }
    .unwrap_or(Response::Empty)
}

fn decode_indication(code: u32, r: &mut crate::binder::Reader) -> Indication {
    match code {
        ind::RADIO_STATE_CHANGED => Indication::RadioState(r.i32().unwrap_or(-1)),
        ind::CALL_STATE_CHANGED => Indication::CallStateChanged,
        ind::NETWORK_STATE_CHANGED => Indication::NetworkStateChanged,
        ind::NEW_SMS => Indication::NewSms(r.byte_vec().unwrap_or_default()),
        ind::NEW_SMS_STATUS_REPORT => {
            Indication::NewSmsStatusReport(r.byte_vec().unwrap_or_default())
        }
        ind::NEW_SMS_ON_SIM => Indication::NewSmsOnSim(r.i32().unwrap_or(-1)),
        ind::ON_USSD => {
            let mode = r.i32().unwrap_or(0);
            Indication::Ussd {
                mode,
                message: r.string().unwrap_or_default(),
            }
        }
        ind::NITZ_TIME_RECEIVED => {
            let time = r.string().unwrap_or_default();
            Indication::Nitz {
                time,
                received_ms: r.i64().unwrap_or(0),
            }
        }
        ind::CURRENT_SIGNAL_STRENGTH => r
            .buffer()
            .map(Signal::decode)
            .map_or(Indication::Other(code), Indication::Signal),
        ind::DATA_CALL_LIST_CHANGED => Indication::DataCallList(
            r.buffer()
                .map(|v| v.vec(0, 120).into_iter().map(DataCall::decode).collect())
                .unwrap_or_default(),
        ),
        ind::CALL_RING => Indication::CallRing,
        ind::SIM_STATUS_CHANGED => Indication::SimStatusChanged,
        ind::INDICATE_RINGBACK_TONE => Indication::RingbackTone(r.bool().unwrap_or(false)),
        ind::RIL_CONNECTED => Indication::RilConnected,
        ind::VOICE_RADIO_TECH_CHANGED => Indication::VoiceRadioTech(r.i32().unwrap_or(0)),
        ind::IMS_NETWORK_STATE_CHANGED => Indication::ImsNetworkStateChanged,
        ind::MODEM_RESET => Indication::ModemReset(r.string().unwrap_or_default()),
        other => Indication::Other(other),
    }
}

/// Decodes an incoming call on one of our objects into `events`.
/// `objects[i]` is the role of object pointer i + 1.
fn on_incoming(objects: &[Role], events: &mut VecDeque<Event>, mut call: Incoming) {
    let Some(&role) = usize::try_from(call.object)
        .ok()
        .and_then(|p| p.checked_sub(1))
        .and_then(|i| objects.get(i))
    else {
        return;
    };
    let r = &mut call.data;
    match role {
        Role::Response(slot) => {
            if call.interface != IRADIO_RESPONSE || call.code == resp::ACKNOWLEDGE_REQUEST {
                return;
            }
            // RadioResponseInfo { type, serial, error }
            let Some(info) = r.buffer() else { return };
            let (kind, serial, error) = (info.i32(0), info.i32(4), info.i32(8));
            let body = decode_response(call.code, r);
            events.push_back(Event::Response {
                slot,
                code: call.code,
                serial,
                error,
                body,
            });
            if kind == 2 {
                events.push_back(Event::AckWanted { slot });
            }
        }
        Role::Indication(slot) => {
            if call.interface != IRADIO_INDICATION {
                return;
            }
            let kind = r.i32().unwrap_or(0);
            let indication = decode_indication(call.code, r);
            events.push_back(Event::Indication { slot, indication });
            if kind == 1 {
                events.push_back(Event::AckWanted { slot });
            }
        }
        Role::ConfigResponse => {
            if !CONFIG_RESPONSE_CHAIN.contains(&call.interface.as_str()) {
                return;
            }
            let Some(info) = r.buffer() else { return };
            let (serial, error) = (info.i32(4), info.i32(8));
            let body = decode_config_response(call.code, r);
            events.push_back(Event::ConfigResponse {
                serial,
                error,
                body,
            });
        }
        Role::ConfigIndication => {
            if !CONFIG_INDICATION_CHAIN.contains(&call.interface.as_str()) {
                return;
            }
            let _kind = r.i32();
            let with_eid = call.code == 2;
            let size = if with_eid { 64 } else { 48 };
            let slots = r
                .buffer()
                .map(|v| {
                    v.vec(0, size)
                        .into_iter()
                        .map(|s| SimSlot::decode(s, with_eid))
                        .collect()
                })
                .unwrap_or_default();
            events.push_back(Event::SimSlotsChanged(slots));
        }
    }
}

pub struct Slot {
    pub name: String,
    pub handle: Option<u32>,
    response: u64,
    indication: u64,
}

/// The radio HAL connection for all SIM slots.
pub struct Radio {
    pub binder: Binder,
    pub slots: Vec<Slot>,
    /// IRadioConfig handle, once connected.
    pub config: Option<u32>,
    config_objects: (u64, u64),
    objects: Vec<Role>,
    serial: i32,
    events: Rc<RefCell<VecDeque<Event>>>,
}

impl Radio {
    pub fn open(slot_names: &[&str]) -> Result<Self, Error> {
        let mut binder = Binder::open("/dev/hwbinder")?;
        let mut objects = Vec::new();
        let mut slots = Vec::new();
        for (i, name) in slot_names.iter().enumerate() {
            let response = binder.add_object(&[IRADIO_RESPONSE]);
            objects.push(Role::Response(i));
            let indication = binder.add_object(&[IRADIO_INDICATION]);
            objects.push(Role::Indication(i));
            slots.push(Slot {
                name: name.to_string(),
                handle: None,
                response,
                indication,
            });
        }
        let config_objects = (
            binder.add_object(&CONFIG_RESPONSE_CHAIN),
            binder.add_object(&CONFIG_INDICATION_CHAIN),
        );
        objects.push(Role::ConfigResponse);
        objects.push(Role::ConfigIndication);
        Ok(Self {
            binder,
            slots,
            config: None,
            config_objects,
            objects,
            serial: 0,
            events: Rc::new(RefCell::new(VecDeque::new())),
        })
    }

    fn with_handler<T>(&mut self, f: impl FnOnce(&mut Binder, &mut Handler) -> T) -> T {
        let objects = self.objects.clone();
        let events = Rc::clone(&self.events);
        let mut handler =
            move |call: Incoming| on_incoming(&objects, &mut events.borrow_mut(), call);
        f(&mut self.binder, &mut handler)
    }

    /// Looks the slot's IRadio up and registers our callbacks with it.
    /// Ok(false): the service is not registered (rild down or restarting).
    pub fn connect(&mut self, slot: usize) -> Result<bool, Error> {
        let name = self.slots[slot].name.clone();
        let Some(handle) = self.with_handler(|b, h| b.get_service(IRADIO_FQNAME, &name, h))? else {
            return Ok(false);
        };
        self.binder.link_to_death(handle, slot as u64 + 1)?;
        let mut parcel = Parcel::request(IRADIO);
        parcel.write_local_object(self.slots[slot].response);
        parcel.write_local_object(self.slots[slot].indication);
        if let Err(e) = self
            .with_handler(|b, h| b.call(handle, req::SET_RESPONSE_FUNCTIONS, &parcel, h, |_| ()))
        {
            let _ = self.binder.unlink_to_death(handle, slot as u64 + 1);
            let _ = self.binder.release(handle);
            return Err(e);
        }
        self.slots[slot].handle = Some(handle);
        Ok(true)
    }

    /// Connects to IRadioConfig/default (SIM slots, eUICC). Ok(false): absent.
    pub fn connect_config(&mut self) -> Result<bool, Error> {
        let Some(handle) = self.with_handler(|b, h| b.get_service(IRADIO_CONFIG, "default", h))?
        else {
            return Ok(false);
        };
        let mut parcel = Parcel::request(IRADIO_CONFIG);
        parcel.write_local_object(self.config_objects.0);
        parcel.write_local_object(self.config_objects.1);
        if let Err(e) = self.with_handler(|b, h| {
            b.call(
                handle,
                config_req::SET_RESPONSE_FUNCTIONS,
                &parcel,
                h,
                |_| (),
            )
        }) {
            let _ = self.binder.release(handle);
            return Err(e);
        }
        self.config = Some(handle);
        Ok(true)
    }

    pub fn config_request(&mut self, code: u32) -> Result<i32, Error> {
        let handle = self.config.ok_or(Error::Dead)?;
        let serial = self.next_serial();
        let mut parcel = Parcel::request(IRADIO_CONFIG);
        parcel.write_i32(serial);
        self.with_handler(|b, h| b.send_oneway(handle, code, &parcel, h))?;
        Ok(serial)
    }

    /// Slots whose rild died since the last call (they need `connect`).
    pub fn take_dead_slots(&mut self) -> Vec<usize> {
        let mut dead = Vec::new();
        for cookie in self.binder.take_deaths() {
            let slot = cookie as usize - 1;
            if let Some(handle) = self.slots.get_mut(slot).and_then(|s| s.handle.take()) {
                let _ = self.binder.unlink_to_death(handle, cookie);
                let _ = self.binder.release(handle);
                dead.push(slot);
            }
        }
        dead
    }

    pub fn process(&mut self) -> Result<(), Error> {
        self.with_handler(|b, h| b.process(h))
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        self.events.borrow_mut().drain(..).collect()
    }

    fn next_serial(&mut self) -> i32 {
        self.serial = self.serial % 0x3fff_ffff + 1;
        self.serial
    }

    /// Sends a one-way request; `args` appends the arguments after the
    /// serial. Returns the serial its response will carry.
    pub fn request(
        &mut self,
        slot: usize,
        code: u32,
        args: impl FnOnce(&mut Parcel),
    ) -> Result<i32, Error> {
        let handle = self
            .slots
            .get(slot)
            .and_then(|s| s.handle)
            .ok_or(Error::Dead)?;
        let serial = self.next_serial();
        let mut parcel = Parcel::request(IRADIO);
        parcel.write_i32(serial);
        args(&mut parcel);
        self.with_handler(|b, h| b.send_oneway(handle, code, &parcel, h))?;
        Ok(serial)
    }

    pub fn acknowledge(&mut self, slot: usize) -> Result<(), Error> {
        let handle = self
            .slots
            .get(slot)
            .and_then(|s| s.handle)
            .ok_or(Error::Dead)?;
        let parcel = Parcel::request(IRADIO);
        self.with_handler(|b, h| b.send_oneway(handle, req::RESPONSE_ACKNOWLEDGEMENT, &parcel, h))
    }
}

/// Request argument builders for the struct-typed requests.
pub mod args {
    use super::*;

    /// Dial@1.0 { string address; Clir clir; vec<UusInfo> uusInfo; }
    pub fn dial(p: &mut Parcel, number: &str) {
        let mut blob = Blob::new(40);
        blob.string(0, number).i32(16, 0 /* CLIR default */);
        blob.vec(24, 0, Blob::new(0));
        p.write_blob(blob);
    }

    /// SimApdu { int32 sessionId, cla, instruction, p1, p2, p3; string data; }
    pub fn sim_apdu(p: &mut Parcel, session: i32, apdu: [i32; 5], data_hex: &str) {
        let mut blob = Blob::new(40);
        blob.i32(0, session);
        for (i, v) in apdu.iter().enumerate() {
            blob.i32(4 + 4 * i, *v);
        }
        blob.string(24, data_hex);
        p.write_blob(blob);
    }

    /// GsmSmsMessage { string smscPdu; string pdu; } (hex strings)
    pub fn gsm_sms(p: &mut Parcel, smsc_hex: &str, pdu_hex: &str) {
        let mut blob = Blob::new(32);
        blob.string(0, smsc_hex).string(16, pdu_hex);
        p.write_blob(blob);
    }

    pub struct Apn<'a> {
        pub apn: &'a str,
        /// "IP", "IPV6" or "IPV4V6".
        pub protocol: &'a str,
        pub user: &'a str,
        pub password: &'a str,
        /// ApnAuthType: 0 none, 1 PAP, 2 CHAP, 3 PAP or CHAP.
        pub auth: i32,
        /// ApnTypes bitmap: 1 default, 2 mms, 4 supl, 8 dun, 0x40 ims, 0x100 ia.
        pub types: i32,
    }

    /// DataProfileInfo@1.0 (152 bytes).
    pub fn data_profile(p: &mut Parcel, apn: &Apn) {
        let mut blob = Blob::new(152);
        blob.i32(0, 0) // profileId DEFAULT
            .string(8, apn.apn)
            .string(24, apn.protocol)
            .string(40, apn.protocol)
            .i32(56, apn.auth)
            .string(64, apn.user)
            .string(80, apn.password)
            .i32(96, 1) // type 3GPP
            .u8(112, 1) // enabled
            .i32(116, apn.types)
            .string(136, ""); // mvnoMatchData
        p.write_blob(blob);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_levels_follow_android_defaults() {
        let lte = |rsrp| Signal {
            lte_rsrp: rsrp,
            gsm_asu: 99,
            ..Default::default()
        };
        assert_eq!(lte(80).level(), 4);
        assert_eq!(lte(100).level(), 2);
        assert_eq!(lte(120).level(), 0);
        assert_eq!(lte(0x7fff_ffff).lte_dbm(), None);
        let gsm = |asu| Signal {
            gsm_asu: asu,
            lte_rsrp: 0x7fff_ffff,
            ..Default::default()
        };
        assert_eq!(gsm(99).level(), 0);
        assert_eq!(gsm(10).level(), 3);
        assert_eq!(gsm(10).gw_dbm(), Some(-93));
    }
}
