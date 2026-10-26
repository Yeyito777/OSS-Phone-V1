//! GSM SMS PDUs (3GPP TS 23.040/23.038): SMS-SUBMIT encoding with
//! concatenation, SMS-DELIVER and SMS-STATUS-REPORT decoding.

/// GSM 7-bit default alphabet (TS 23.038 §6.2.1), index = septet.
const GSM7: [char; 128] = [
    '@', '£', '$', '¥', 'è', 'é', 'ù', 'ì', 'ò', 'Ç', '\n', 'Ø', 'ø', '\r', 'Å', 'å', //
    'Δ', '_', 'Φ', 'Γ', 'Λ', 'Ω', 'Π', 'Ψ', 'Σ', 'Θ', 'Ξ', '\u{1b}', 'Æ', 'æ', 'ß', 'É', //
    ' ', '!', '"', '#', '¤', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/', //
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ':', ';', '<', '=', '>', '?', //
    '¡', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', //
    'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', 'Ä', 'Ö', 'Ñ', 'Ü', '§', //
    '¿', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', //
    'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', 'ä', 'ö', 'ñ', 'ü', 'à', //
];

/// The default extension table (after ESC 0x1B).
const GSM7_EXT: [(u8, char); 10] = [
    (0x0a, '\u{0c}'),
    (0x14, '^'),
    (0x28, '{'),
    (0x29, '}'),
    (0x2f, '\\'),
    (0x3c, '['),
    (0x3d, '~'),
    (0x3e, ']'),
    (0x40, '|'),
    (0x65, '€'),
];

/// The septets for one character, or None if GSM 7-bit cannot carry it.
fn gsm7_septets(c: char) -> Option<Vec<u8>> {
    if c != '\u{1b}'
        && let Some(i) = GSM7.iter().position(|&g| g == c)
    {
        return Some(vec![i as u8]);
    }
    GSM7_EXT
        .iter()
        .find(|(_, g)| *g == c)
        .map(|(code, _)| vec![0x1b, *code])
}

fn gsm7_decode(septets: &[u8]) -> String {
    let mut text = String::new();
    let mut escape = false;
    for &s in septets {
        let s = s & 0x7f;
        if escape {
            escape = false;
            match GSM7_EXT.iter().find(|(code, _)| *code == s) {
                Some((_, c)) => text.push(*c),
                None => text.push(GSM7[s as usize]),
            }
        } else if s == 0x1b {
            escape = true;
        } else {
            text.push(GSM7[s as usize]);
        }
    }
    text
}

/// Packs septets into octets, starting after `fill` padding bits.
fn pack7(septets: &[u8], fill: usize) -> Vec<u8> {
    let bits = fill + septets.len() * 7;
    let mut out = vec![0u8; bits.div_ceil(8)];
    for (i, &s) in septets.iter().enumerate() {
        let bit = fill + i * 7;
        let (byte, shift) = (bit / 8, bit % 8);
        out[byte] |= (s << shift) as u8;
        if shift > 1 {
            out[byte + 1] |= s >> (8 - shift);
        }
    }
    out
}

fn unpack7(octets: &[u8], fill: usize, count: usize) -> Vec<u8> {
    (0..count)
        .map(|i| {
            let bit = fill + i * 7;
            let (byte, shift) = (bit / 8, bit % 8);
            let lo = octets.get(byte).copied().unwrap_or(0) as u16;
            let hi = octets.get(byte + 1).copied().unwrap_or(0) as u16;
            (((hi << 8 | lo) >> shift) & 0x7f) as u8
        })
        .collect()
}

/// An address field (TP-DA/TP-OA): length in digits, TOA, swapped BCD.
fn encode_address(number: &str) -> Option<Vec<u8>> {
    let (toa, digits) = match number.strip_prefix('+') {
        Some(rest) => (0x91, rest),
        None => (0x81, number),
    };
    let digits: Vec<u8> = digits
        .chars()
        .map(|c| match c {
            '0'..='9' => Some(c as u8 - b'0'),
            '*' => Some(0xa),
            '#' => Some(0xb),
            _ => None,
        })
        .collect::<Option<_>>()?;
    if digits.is_empty() || digits.len() > 20 {
        return None;
    }
    let mut out = vec![digits.len() as u8, toa];
    for pair in digits.chunks(2) {
        out.push(pair[0] | pair.get(1).copied().unwrap_or(0xf) << 4);
    }
    Some(out)
}

fn decode_bcd(octets: &[u8], digits: usize) -> String {
    let mut out = String::new();
    for &o in octets {
        for nibble in [o & 0xf, o >> 4] {
            if out.len() >= digits {
                break;
            }
            out.push(match nibble {
                0..=9 => (b'0' + nibble) as char,
                0xa => '*',
                0xb => '#',
                0xc => 'a',
                0xd => 'b',
                0xe => 'c',
                _ => continue,
            });
        }
    }
    out
}

/// Decodes TP-OA/TP-RA at `pdu[pos..]`; returns (address, octets used).
fn decode_address(pdu: &[u8], pos: usize) -> Option<(String, usize)> {
    let digits = *pdu.get(pos)? as usize;
    let toa = *pdu.get(pos + 1)?;
    let octets = digits.div_ceil(2);
    let body = pdu.get(pos + 2..pos + 2 + octets)?;
    let address = if (toa >> 4) & 7 == 5 {
        // Alphanumeric sender ("Freedom"): GSM 7-bit packed in `digits`
        // nibbles. Some networks count the padding nibble, which then reads
        // as a trailing '@' (septet 0); drop it.
        let mut septets = unpack7(body, 0, digits * 4 / 7);
        if digits * 4 % 7 == 0 && septets.last() == Some(&0) {
            septets.pop();
        }
        gsm7_decode(&septets)
    } else {
        let number = decode_bcd(body, digits);
        if (toa >> 4) & 7 == 1 {
            format!("+{number}")
        } else {
            number
        }
    };
    Some((address, 2 + octets))
}

/// A service-centre timestamp (7 swapped-BCD octets) as Unix seconds.
fn decode_timestamp(t: &[u8]) -> Option<i64> {
    let bcd = |o: u8| ((o & 0xf) * 10 + (o >> 4)) as i64;
    let yy = bcd(t[0]);
    let year = if yy >= 90 { 1900 + yy } else { 2000 + yy };
    let (month, day) = (bcd(t[1]), bcd(t[2]));
    let (hour, minute, second) = (bcd(t[3]), bcd(t[4]), bcd(t[5]));
    // Time zone: quarter hours; bit 3 of the first semi-octet is the sign.
    let tz = t[6];
    let quarters = ((tz & 0x7) * 10 + (tz >> 4)) as i64;
    let offset = if tz & 0x8 != 0 { -quarters } else { quarters } * 15 * 60;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days since 1970-01-01 (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn ucs2_decode(octets: &[u8]) -> String {
    let units: Vec<u16> = octets
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    String::from_utf16_lossy(&units)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Concat {
    pub reference: u16,
    pub total: u8,
    pub sequence: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Deliver {
    pub sender: String,
    pub text: String,
    /// Service-centre time stamp, Unix seconds.
    pub sent_at: Option<i64>,
    pub concat: Option<Concat>,
    /// Class 0 ("flash") message.
    pub flash: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatusReport {
    pub message_ref: u8,
    pub recipient: String,
    /// TP-ST: 0x00–0x1f delivered/completed, 0x20–0x3f still trying,
    /// 0x40+ failed.
    pub status: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    Deliver(Deliver),
    StatusReport(StatusReport),
}

/// Text alphabet from TP-DCS.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Alphabet {
    Gsm7,
    Data8,
    Ucs2,
}

fn alphabet(dcs: u8) -> (Alphabet, bool) {
    let class0 = |dcs: u8| dcs & 0x13 == 0x10;
    match dcs >> 4 {
        0x0..=0x3 => (
            match (dcs >> 2) & 3 {
                1 => Alphabet::Data8,
                2 => Alphabet::Ucs2,
                _ => Alphabet::Gsm7,
            },
            class0(dcs),
        ),
        0xc | 0xd => (Alphabet::Gsm7, false),
        0xe => (Alphabet::Ucs2, false),
        0xf => (
            if dcs & 0x4 != 0 {
                Alphabet::Data8
            } else {
                Alphabet::Gsm7
            },
            dcs & 0x3 == 0,
        ),
        _ => (Alphabet::Gsm7, false),
    }
}

/// Parses a PDU as rild delivers it: SMSC address first (TS 27.005 +CMT).
pub fn decode(pdu: &[u8]) -> Option<Incoming> {
    let smsc_len = *pdu.first()? as usize;
    let tpdu = pdu.get(1 + smsc_len..)?;
    decode_tpdu(tpdu)
}

pub fn decode_tpdu(p: &[u8]) -> Option<Incoming> {
    let first = *p.first()?;
    match first & 0x3 {
        0 => decode_deliver(p, first),
        2 => {
            let message_ref = *p.get(1)?;
            let (recipient, used) = decode_address(p, 2)?;
            let pos = 2 + used + 14; // SCTS + discharge time
            Some(Incoming::StatusReport(StatusReport {
                message_ref,
                recipient,
                status: *p.get(pos)?,
            }))
        }
        _ => None,
    }
}

fn decode_deliver(p: &[u8], first: u8) -> Option<Incoming> {
    let (sender, used) = decode_address(p, 1)?;
    let mut pos = 1 + used;
    let _pid = *p.get(pos)?;
    let dcs = *p.get(pos + 1)?;
    let sent_at = decode_timestamp(p.get(pos + 2..pos + 9)?);
    pos += 9;
    let udl = *p.get(pos)? as usize;
    let ud = p.get(pos + 1..)?;
    let (alphabet, flash) = alphabet(dcs);
    let mut concat = None;
    let mut header_octets = 0;
    if first & 0x40 != 0 {
        let udhl = *ud.first()? as usize;
        let header = ud.get(1..1 + udhl)?;
        header_octets = 1 + udhl;
        let mut i = 0;
        while i + 2 <= header.len() {
            let (id, len) = (header[i], header[i + 1] as usize);
            let data = header.get(i + 2..i + 2 + len)?;
            match (id, len) {
                (0x00, 3) => {
                    concat = Some(Concat {
                        reference: data[0] as u16,
                        total: data[1],
                        sequence: data[2],
                    })
                }
                (0x08, 4) => {
                    concat = Some(Concat {
                        reference: u16::from_be_bytes([data[0], data[1]]),
                        total: data[2],
                        sequence: data[3],
                    })
                }
                _ => {}
            }
            i += 2 + len;
        }
    }
    let text = match alphabet {
        Alphabet::Gsm7 => {
            let header_septets = (header_octets * 8).div_ceil(7);
            let fill = header_septets * 7 - header_octets * 8;
            let count = udl.saturating_sub(header_septets);
            gsm7_decode(&unpack7(ud.get(header_octets..)?, fill, count))
        }
        Alphabet::Ucs2 => ucs2_decode(ud.get(header_octets..udl.min(ud.len()))?),
        Alphabet::Data8 => {
            let data = ud.get(header_octets..udl.min(ud.len()))?;
            String::from_utf8(data.to_vec())
                .unwrap_or_else(|_| data.iter().map(|&b| b as char).collect())
        }
    };
    Some(Incoming::Deliver(Deliver {
        sender,
        text,
        sent_at,
        concat: concat.filter(|c| c.total > 1 && c.sequence >= 1 && c.sequence <= c.total),
        flash,
    }))
}

/// One encoded SMS-SUBMIT (hex TPDU, no SMSC: rild uses the SIM's).
#[derive(Debug, Clone, PartialEq)]
pub struct Submit {
    pub pdu: Vec<u8>,
}

pub const MAX_PARTS: usize = 10;

#[derive(Debug, PartialEq)]
pub enum EncodeError {
    BadNumber,
    Empty,
    TooLong,
}

/// Splits `text` into SMS-SUBMIT PDUs (GSM 7-bit when possible, else UCS-2;
/// concatenated with an 8-bit reference when it does not fit one).
pub fn encode_submit(
    number: &str,
    text: &str,
    reference: u8,
    status_report: bool,
) -> Result<Vec<Submit>, EncodeError> {
    let address = encode_address(number).ok_or(EncodeError::BadNumber)?;
    if text.is_empty() {
        return Err(EncodeError::Empty);
    }
    let gsm: Option<Vec<Vec<u8>>> = text.chars().map(gsm7_septets).collect();
    // Units that must not be split: one character each.
    enum Body {
        Gsm(Vec<Vec<u8>>),
        Ucs2(Vec<Vec<u16>>),
    }
    let body = match gsm {
        Some(chars) => Body::Gsm(chars),
        None => Body::Ucs2(
            text.chars()
                .map(|c| {
                    let mut buf = [0u16; 2];
                    c.encode_utf16(&mut buf).to_vec()
                })
                .collect(),
        ),
    };
    let (single, multi) = match body {
        Body::Gsm(_) => (160, 153),
        Body::Ucs2(_) => (70, 67),
    };
    let size = |unit_len: usize| unit_len;
    let parts: Vec<(Vec<u8>, usize)> = match &body {
        Body::Gsm(chars) => {
            let total: usize = chars.iter().map(Vec::len).sum();
            split(
                chars,
                |c| size(c.len()),
                if total <= single { single } else { multi },
            )
            .into_iter()
            .map(|group| {
                let septets: Vec<u8> = group.concat();
                let n = septets.len();
                (septets, n)
            })
            .collect()
        }
        Body::Ucs2(chars) => {
            let total: usize = chars.iter().map(Vec::len).sum();
            split(
                chars,
                |c| size(c.len()),
                if total <= single { single } else { multi },
            )
            .into_iter()
            .map(|group| {
                let octets: Vec<u8> = group
                    .concat()
                    .iter()
                    .flat_map(|u| u.to_be_bytes())
                    .collect();
                let n = octets.len();
                (octets, n)
            })
            .collect()
        }
    };
    if parts.len() > MAX_PARTS {
        return Err(EncodeError::TooLong);
    }
    let gsm = matches!(body, Body::Gsm(_));
    let count = parts.len();
    Ok(parts
        .into_iter()
        .enumerate()
        .map(|(i, (units, _))| {
            let concat = count > 1;
            let mut pdu = vec![
                0x01 | if status_report { 0x20 } else { 0 } | if concat { 0x40 } else { 0 },
                0x00,
            ];
            pdu.extend_from_slice(&address);
            pdu.push(0x00); // TP-PID
            pdu.push(if gsm { 0x00 } else { 0x08 });
            let header: Vec<u8> = if concat {
                vec![0x05, 0x00, 0x03, reference, count as u8, i as u8 + 1]
            } else {
                Vec::new()
            };
            if gsm {
                let header_septets = (header.len() * 8).div_ceil(7);
                let fill = header_septets * 7 - header.len() * 8;
                pdu.push((header_septets + units.len()) as u8);
                pdu.extend_from_slice(&header);
                pdu.extend(pack7(&units, fill));
            } else {
                pdu.push((header.len() + units.len()) as u8);
                pdu.extend_from_slice(&header);
                pdu.extend_from_slice(&units);
            }
            Submit { pdu }
        })
        .collect())
}

/// Groups units greedily into chunks of at most `limit` (by `len`).
fn split<T: Clone>(units: &[T], len: impl Fn(&T) -> usize, limit: usize) -> Vec<Vec<T>> {
    let mut groups: Vec<Vec<T>> = vec![Vec::new()];
    let mut used = 0;
    for unit in units {
        let n = len(unit);
        if used + n > limit {
            groups.push(Vec::new());
            used = 0;
        }
        groups.last_mut().unwrap().push(unit.clone());
        used += n;
    }
    groups
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn decodes_a_textbook_deliver() {
        // "hellohello" from +27381000015 (the classic dreamfabric example).
        let pdu = unhex("07917283010010F5040BC87238880900F10000993092516195800AE8329BFD4697D9EC37");
        let Some(Incoming::Deliver(d)) = decode(&pdu) else {
            panic!()
        };
        assert_eq!(d.text, "hellohello");
        assert_eq!(d.sender, "27838890001");
        assert_eq!(
            d.sent_at,
            Some(days_from_civil(1999, 3, 29) * 86400 + 15 * 3600 + 16 * 60 + 59 - 2 * 3600)
        );
        assert_eq!(d.concat, None);
    }

    #[test]
    fn decodes_alphanumeric_senders() {
        let name = pack7(
            &"Freedom"
                .chars()
                .map(|c| gsm7_septets(c).unwrap()[0])
                .collect::<Vec<_>>(),
            0,
        );
        let mut tpdu = vec![0x04, 13, 0xd0];
        tpdu.extend(&name);
        tpdu.extend([0x00, 0x00, 0x62, 0x01, 0x62, 0x21, 0x43, 0x00, 0x00, 0x02]);
        tpdu.extend(pack7(&[0x48, 0x69], 0));
        let Some(Incoming::Deliver(d)) = decode_tpdu(&tpdu) else {
            panic!()
        };
        assert_eq!(d.sender, "Freedom");
        assert_eq!(d.text, "Hi");
    }

    #[test]
    fn submit_round_trips_through_deliver_decoding() {
        let text = "Hello {world} €5 — ok";
        let parts = encode_submit("+16475551234", text, 7, false).unwrap();
        assert_eq!(parts.len(), 1);
        // "—" forces UCS-2.
        assert_eq!(parts[0].pdu[2 + 2 + 6 + 1], 0x08);
        let gsm = encode_submit("+16475551234", "Hi [there]", 7, true).unwrap();
        assert_eq!(gsm[0].pdu[0], 0x21);
        assert_eq!(hex(&gsm[0].pdu[2..10]), "0B916174551532F4");
    }

    #[test]
    fn long_messages_are_concatenated_and_reassemble() {
        let text: String = (0..400).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let parts = encode_submit("6475551234", &text, 42, false).unwrap();
        assert_eq!(parts.len(), 3);
        let mut rebuilt = String::new();
        for (i, part) in parts.iter().enumerate() {
            // Turn the SUBMIT into a DELIVER: MTI 0, no MR, add PID/DCS/SCTS.
            let p = &part.pdu;
            let addr_len = 2 + (p[2] as usize).div_ceil(2);
            let mut d = vec![0x40];
            d.extend(&p[2..2 + addr_len]);
            d.extend(&p[2 + addr_len..2 + addr_len + 2]);
            d.extend([0x62, 0x01, 0x62, 0x21, 0x43, 0x00, 0x00]);
            d.extend(&p[2 + addr_len + 2..]);
            let Some(Incoming::Deliver(m)) = decode_tpdu(&d) else {
                panic!()
            };
            assert_eq!(
                m.concat,
                Some(Concat {
                    reference: 42,
                    total: 3,
                    sequence: i as u8 + 1
                })
            );
            rebuilt.push_str(&m.text);
        }
        assert_eq!(rebuilt, text);
    }

    #[test]
    fn ucs2_parts_keep_surrogate_pairs_whole() {
        let text: String = std::iter::repeat_n('😀', 40).collect();
        let parts = encode_submit("+15555550100", &text, 1, false).unwrap();
        assert_eq!(parts.len(), 2);
        // 67 units per part: 33 emoji (66 units) then the remaining 7.
        assert_eq!(parts[0].pdu[2 + 2 + 6 + 1 + 1], 6 + 132);
    }

    #[test]
    fn decodes_status_reports() {
        let tpdu = unhex("06070B916174551532F4620162214300006201622143010000");
        let Some(Incoming::StatusReport(r)) = decode_tpdu(&tpdu) else {
            panic!()
        };
        assert_eq!(r.message_ref, 7);
        assert_eq!(r.recipient, "+16475551234");
        assert_eq!(r.status, 0);
    }

    #[test]
    fn rejects_bad_numbers() {
        assert_eq!(
            encode_submit("call me", "x", 0, false),
            Err(EncodeError::BadNumber)
        );
    }
}
