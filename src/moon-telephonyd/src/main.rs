//! moon-telephonyd: OSS-Phone-V1's telephony service. SIM, signal, SMS,
//! voice calls and mobile data through the vendor radio HAL, with Android's
//! framework never running. See docs/telephony-protocol.md.

mod binder;
mod daemon;
mod radio;
mod sms;
mod store;

use std::time::{Duration, Instant};

use radio::{Event, Indication, Radio, Response, req};

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
    std::process::exit(2);
}

fn mask(text: &str) -> String {
    let keep = text.len().min(5);
    format!("{}{}", &text[..keep], "*".repeat(text.len() - keep))
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
            }
        }
        for slot in radio.take_dead_slots() {
            println!("slot{}: rild died", slot + 1);
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
        Some("probe") => probe(args.get(1).and_then(|s| s.parse().ok()).unwrap_or(5)),
        _ => usage(),
    };
    if let Err(error) = result {
        eprintln!("moon-telephonyd: {error}");
        std::process::exit(1);
    }
}
