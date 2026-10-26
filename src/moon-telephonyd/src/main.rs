//! moon-telephonyd: OSS-Phone-V1's telephony service. SIM, signal, SMS,
//! voice calls and mobile data through the vendor radio HAL, with Android's
//! framework never running. See docs/telephony-protocol.md.

mod binder;
mod daemon;
mod json_strict;
mod radio;
mod sms;
mod store;

use std::time::{Duration, Instant};

use radio::{Event, Indication, Radio, Response, SimSlot, config_req, req};

fn usage() -> ! {
    eprintln!(
        "usage: moon-telephonyd                   run the service (paths under $MOON_TELEPHONY_ROOT if set)"
    );
    eprintln!(
        "       moon-telephonyd probe [SECONDS]   status dump of both SIM slots (registers as rild's client)"
    );
    eprintln!(
        "       moon-telephonyd selftest          binder check: looks IRadio up, asks only IBase (like lshal)"
    );
    eprintln!(
        "       moon-telephonyd esim-probe        reads the EID from each slot's ISD-R (a logical channel, read-only)"
    );
    std::process::exit(2);
}

fn mask(text: &str) -> String {
    let keep = text.len().min(5);
    format!("{}{}", &text[..keep], "*".repeat(text.len() - keep))
}

/// ICCIDs and EIDs identify the subscriber/device: print only their prefix
/// (issuer / eUICC manufacturer), which is what the probe needs.
fn mask_slots(slots: Vec<SimSlot>) -> Vec<SimSlot> {
    slots
        .into_iter()
        .map(|s| SimSlot {
            iccid: mask(&s.iccid),
            eid: if s.eid.len() > 8 {
                format!("{}…({} digits)", &s.eid[..8], s.eid.len())
            } else {
                s.eid
            },
            ..s
        })
        .collect()
}

/// Connects to rild, asks every slot for its state, prints what comes back.
/// Sends only queries; changes nothing in the modem.
fn probe(seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    let mut radio = Radio::open(&["slot1", "slot2"])?;
    for slot in 0..radio.slots.len() {
        match radio.connect(slot) {
            Ok(true) => println!("slot{}: connected", slot + 1),
            Ok(false) => println!("slot{}: IRadio not registered", slot + 1),
            Err(e) => println!("slot{}: connect failed: {e}", slot + 1),
        }
    }
    match radio.connect_config() {
        Ok(true) => {
            println!("IRadioConfig: connected");
            for code in [
                config_req::GET_SIM_SLOTS_STATUS,
                config_req::GET_PHONE_CAPABILITY,
            ] {
                if let Err(e) = radio.config_request(code) {
                    println!("IRadioConfig: request {code} failed: {e}");
                }
            }
        }
        Ok(false) => println!("IRadioConfig: not registered"),
        Err(e) => println!("IRadioConfig: connect failed: {e}"),
    }
    let queries = [
        req::GET_ICC_CARD_STATUS,
        req::GET_SIGNAL_STRENGTH,
        req::GET_VOICE_REGISTRATION_STATE,
        req::GET_DATA_REGISTRATION_STATE,
        req::GET_OPERATOR,
        req::GET_VOICE_RADIO_TECHNOLOGY,
        req::GET_IMS_REGISTRATION_STATE,
        req::GET_BASEBAND_VERSION,
        req::GET_CURRENT_CALLS,
        req::GET_DATA_CALL_LIST,
    ];
    for slot in 0..radio.slots.len() {
        if radio.slots[slot].handle.is_none() {
            continue;
        }
        for code in queries {
            if let Err(e) = radio.request(slot, code, |_| {}) {
                println!("slot{}: request {code} failed: {e}", slot + 1);
            }
        }
    }
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        let mut pfd = libc::pollfd {
            fd: radio.binder.fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        unsafe { libc::poll(&mut pfd, 1, 200) };
        radio.process()?;
        for event in radio.take_events() {
            match event {
                Event::Response {
                    slot,
                    code,
                    serial,
                    error,
                    body,
                } => {
                    let body = match body {
                        Response::Texts(t) if code == 88 => {
                            Response::Texts(t.iter().map(|s| mask(s)).collect())
                        }
                        Response::Text(t) if code == 11 => Response::Text(mask(&t)),
                        other => other,
                    };
                    println!(
                        "slot{} response code={code} serial={serial} error={error}({}) {body:?}",
                        slot + 1,
                        radio::error_name(error)
                    );
                }
                Event::Indication { slot, indication } => {
                    let shown = match &indication {
                        Indication::NewSms(pdu) => {
                            format!("NewSms({} bytes, not acknowledged by probe)", pdu.len())
                        }
                        other => format!("{other:?}"),
                    };
                    println!("slot{} indication {shown}", slot + 1);
                }
                Event::AckWanted { slot } => {
                    let _ = radio.acknowledge(slot);
                }
                Event::ConfigResponse {
                    serial,
                    error,
                    body,
                } => {
                    let body = match body {
                        Response::SimSlots(slots) => Response::SimSlots(mask_slots(slots)),
                        other => other,
                    };
                    println!(
                        "config response serial={serial} error={error}({}) {body:?}",
                        radio::error_name(error)
                    );
                }
                Event::SimSlotsChanged(slots) => println!(
                    "config indication simSlotsStatusChanged {:?}",
                    mask_slots(slots)
                ),
            }
        }
        for slot in radio.take_dead_slots() {
            println!("slot{}: rild died", slot + 1);
        }
    }
    Ok(())
}

/// Waits for the response with `serial`, printing anything else that comes.
fn wait_response(radio: &mut Radio, serial: i32, seconds: u64) -> Option<(i32, Response)> {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        let mut pfd = libc::pollfd {
            fd: radio.binder.fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        unsafe { libc::poll(&mut pfd, 1, 200) };
        if let Err(e) = radio.process() {
            println!("binder: {e}");
            return None;
        }
        for event in radio.take_events() {
            match event {
                Event::Response {
                    serial: s,
                    error,
                    body,
                    ..
                } if s == serial => return Some((error, body)),
                Event::AckWanted { slot } => {
                    let _ = radio.acknowledge(slot);
                }
                _ => {}
            }
        }
    }
    None
}

/// Reads the EID through each slot's ISD-R (GSMA SGP.22 ES10c GetEID), the
/// way an LPA starts. Only a SELECT and a read; the channel is closed after.
fn esim_probe() -> Result<(), Box<dyn std::error::Error>> {
    const ISD_R: &str = "A0000005591010FFFFFFFF8900000100";
    let mut radio = Radio::open(&["slot1", "slot2"])?;
    for slot in 0..radio.slots.len() {
        if !radio.connect(slot)? {
            println!("slot{}: IRadio not registered", slot + 1);
            continue;
        }
        let serial = radio.request(slot, req::ICC_OPEN_LOGICAL_CHANNEL, |p| {
            p.write_string(ISD_R);
            p.write_i32(0);
        })?;
        let channel = match wait_response(&mut radio, serial, 10) {
            Some((0, Response::Channel { id, select })) => {
                println!(
                    "slot{}: ISD-R open on channel {id}, SELECT response {}",
                    slot + 1,
                    sms::hex(&select)
                );
                id
            }
            Some((error, body)) => {
                println!(
                    "slot{}: ISD-R not available: error {error} ({}) {body:?}",
                    slot + 1,
                    radio::error_name(error)
                );
                continue;
            }
            None => {
                println!("slot{}: no answer to iccOpenLogicalChannel", slot + 1);
                continue;
            }
        };
        // STORE DATA (CLA 0x80 | channel) carrying GetEuiccDataRequest
        // BF3E 03 5C 01 5A (tag list: EID).
        let cla = 0x80 | (channel & 0x03);
        let serial = radio.request(slot, req::ICC_TRANSMIT_APDU_LOGICAL_CHANNEL, |p| {
            radio::args::sim_apdu(p, channel, [cla, 0xE2, 0x91, 0x00, 0x06], "BF3E035C015A")
        })?;
        match wait_response(&mut radio, serial, 10) {
            Some((0, Response::IccIo { sw1, sw2, data })) => {
                // BF3E len 5A 10 <16-byte EID>
                let eid = data
                    .find("5A10")
                    .map(|i| data[i + 4..].chars().take(32).collect::<String>())
                    .unwrap_or_default();
                let shown = if eid.len() == 32 {
                    format!("{}… (32 digits)", &eid[..8])
                } else {
                    format!("raw {data}")
                };
                println!("slot{}: GetEID sw={sw1:02X}{sw2:02X} EID {shown}", slot + 1);
            }
            other => println!("slot{}: GetEID failed: {other:?}", slot + 1),
        }
        let serial = radio.request(slot, req::ICC_CLOSE_LOGICAL_CHANNEL, |p| {
            p.write_i32(channel)
        })?;
        match wait_response(&mut radio, serial, 10) {
            Some((error, _)) => {
                println!("slot{}: channel {channel} closed (error {error})", slot + 1)
            }
            None => println!("slot{}: no answer to iccCloseLogicalChannel", slot + 1),
        }
    }
    Ok(())
}

/// Exercises the binder layer against rild without becoming its client:
/// the same IBase queries lshal makes.
fn selftest() -> Result<(), Box<dyn std::error::Error>> {
    let mut b = binder::Binder::open("/dev/hwbinder")?;
    let mut handler = |_: binder::Incoming| {};
    for slot in ["slot1", "slot2"] {
        let Some(handle) = b.get_service(radio::IRADIO_FQNAME, slot, &mut handler)? else {
            println!("{slot}: not registered");
            continue;
        };
        let request = binder::Parcel::request(binder::HIDL_IBASE);
        let chain = b.call(
            handle,
            binder::HIDL_DESCRIPTOR_CHAIN,
            &request,
            &mut handler,
            |r| {
                r.buffer()
                    .map(|v| v.vec(0, 16).iter().map(|s| s.string(0)).collect::<Vec<_>>())
                    .unwrap_or_default()
            },
        )?;
        let pid = b.call(
            handle,
            binder::HIDL_DEBUG_INFO,
            &request,
            &mut handler,
            |r| r.buffer().map(|v| v.i32(0)),
        )?;
        println!("{slot}: handle {handle} pid {pid:?} chain {chain:?}");
        b.release(handle)?;
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        None => daemon::run(daemon::Paths::from_environment()),
        Some("selftest") => selftest(),
        Some("esim-probe") => esim_probe(),
        Some("probe") => probe(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5)),
        _ => usage(),
    };
    if let Err(error) = result {
        eprintln!("moon-telephonyd: {error}");
        std::process::exit(1);
    }
}
