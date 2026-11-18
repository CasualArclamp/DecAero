// made by CasualArclamp
//! What the signal units say: their names, the fields worth showing, and the
//! reassembly of user data (ACARS) from an initial unit and its followers.
//!
//! Types and field layouts follow JAERO `aerol.h` / `aerol.cpp` (MIT, (C)
//! Jonathan Olds): `AEROTypeP`, `CreateCAssignmentItem`, `ISUData::update`,
//! `ParserISU::parse`.

use crate::pframe::SU_BYTES;

/// The name of a P channel signal unit type.
pub fn p_name(kind: u8) -> &'static str {
    match kind {
        0x00 => "reserved",
        0x01 => "fill-in",
        0x05 => "system table: GES P/R channels",
        0x07 => "system table: GES beam support",
        0x0A => "system table: index",
        0x0C => "system table: satellite identification",
        0x10 => "log-on request",
        0x11 => "log-on confirm",
        0x12 => "log-off request",
        0x13 => "log-on reject",
        0x14 => "log-on interrogation",
        0x15 => "log-on/log-off acknowledge",
        0x16 => "log-on prompt",
        0x17 => "data channel reassignment",
        0x20 => "call announcement",
        0x21 => "call information (service address)",
        0x26 => "reserved 26",
        0x28 => "data EIRP table",
        0x30 => "call progress",
        0x31 => "C channel assignment (distress)",
        0x32 => "C channel assignment (flight safety)",
        0x33 => "C channel assignment (other safety)",
        0x34 => "C channel assignment (non-safety)",
        0x40 => "P/R channel control",
        0x41 => "T channel control",
        0x51 => "T channel assignment",
        0x60 => "telephony acknowledge",
        0x61 => "request for acknowledgement",
        0x62 => "acknowledge",
        0x71 => "user data (initial unit)",
        0x74 => "user data (3 octets)",
        0x76 => "user data (4 octets)",
        k if k & 0xC0 == 0xC0 => "user data (following unit)",
        _ => "unknown",
    }
}

/// Units that only fill the frame: not worth a line in the log.
pub fn is_filler(kind: u8) -> bool {
    matches!(kind, 0x00 | 0x01 | 0x26 | 0x0A)
}

/// A channel number as the system tables give it, in MHz (2.5 kHz steps
/// from 1510 MHz, aircraft receive side).
pub fn rx_mhz(channel: u16) -> f64 {
    f64::from(channel & 0x7fff) * 0.0025 + 1510.0
}

/// The aircraft transmit frequency of a channel number, in MHz.
pub fn tx_mhz(channel: u16) -> f64 {
    f64::from(channel & 0x7fff) * 0.0025 + 1611.5
}

fn aes(b: &[u8]) -> u32 {
    (u32::from(b[1]) << 16) | (u32::from(b[2]) << 8) | u32::from(b[3])
}

/// A voice channel handed to an aircraft: where the call will be.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CAssignment {
    pub aes: u32,
    pub ges: u8,
    /// 0x31 distress .. 0x34 non-safety.
    pub kind: u8,
    /// Ground to aircraft (L-band) frequency in MHz.
    pub rx_mhz: f64,
    /// Aircraft to ground frequency in MHz.
    pub tx_mhz: f64,
    pub spot_beam: bool,
}

/// A call progress unit (type 0x30): how a telephone call is getting on.
/// Layout from Rory's table of the unit (`aero_call.ods`, 2026-11-18):
/// octet 6 is the Q number (the call's priority) over the application
/// reference (the call's number), octet 7 a report-specific nibble over the
/// report type, and a result report has its cause in octet 8.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CallProgress {
    pub aes: u32,
    pub ges: u8,
    /// The Q number: 0xF distress, 0xE urgent, 0xD general.
    pub q_no: u8,
    /// The application reference: 0 to 7 for a call the aircraft made,
    /// 8 to 15 for one from the ground.
    pub call_ref: u8,
    /// 1 status, 2 connect, 3 test, 4 result, 5 release.
    pub report: u8,
    /// A status report: the change asked of the aircraft's power, dB.
    pub eirp_db: Option<i8>,
    /// A result report: where the cause arose, and its class and value.
    pub cause: Option<Cause>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cause {
    pub location: u8,
    pub class: u8,
    pub value: u8,
}

impl CallProgress {
    pub fn priority(&self) -> &'static str {
        match self.q_no {
            0xF => "distress",
            0xE => "urgent",
            0xD => "general",
            _ => "",
        }
    }

    pub fn report_name(&self) -> &'static str {
        match self.report {
            1 => "status",
            2 => "connect",
            3 => "test",
            4 => "result",
            5 => "release",
            _ => "",
        }
    }

    pub fn from_ground(&self) -> bool {
        self.call_ref >= 8
    }

    /// One line for the log.
    pub fn describe(&self) -> String {
        let mut out = match self.report_name() {
            "" => format!("report {}", self.report),
            name => name.to_string(),
        };
        match self.priority() {
            "" => out += &format!("  Q {:X}", self.q_no),
            p => out += &format!("  {p} priority"),
        }
        out += &format!(
            "  call {} ({})",
            self.call_ref,
            if self.from_ground() {
                "ground to air"
            } else {
                "air to ground"
            }
        );
        if let Some(db) = self.eirp_db {
            out += &match db {
                0 => "  power as it is".to_string(),
                db => format!("  power {db:+} dB"),
            };
        }
        if let Some(c) = self.cause {
            out += &format!(
                "  cause class {} value {} at location {}",
                c.class, c.value, c.location
            );
        }
        out
    }
}

/// A call announced to an aircraft on the P channel (type 0x20): what kind
/// of call the voice channel that follows will carry. ICAO Annex 10 Volume
/// III (Amendment 80), Appendix 2 figure A2-8 and Appendix 3 items 14 and 62.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Announcement {
    pub aes: u32,
    pub ges: u8,
    pub q_no: u8,
    pub call_ref: u8,
    /// 1 telephone, 2 data through a modem on the ground (and fax), 3 data
    /// to a digital network.
    pub service: u8,
    /// The modem or speed of a data call (see `describe`).
    pub rate: u8,
}

impl Announcement {
    /// A data or fax call: the voice channel will not carry speech.
    pub fn is_data(&self) -> bool {
        matches!(self.service, 2 | 3)
    }

    pub fn describe(&self) -> String {
        match (self.service, self.rate) {
            (1, _) => "telephone".into(),
            (2, 15) => "fax (Group 3)".into(),
            (2, r) => format!(
                "data by modem, {}",
                match r {
                    1 => "V.22 1200 bit/s",
                    2 => "V.22 bis 1200 bit/s",
                    3 => "V.22 bis 2400 bit/s",
                    4 => "V.32 4800 bit/s",
                    5 => "V.32 9600 bit/s",
                    6 => "V.23 1200 bit/s",
                    _ => "modem not named",
                }
            ),
            (3, r) => format!(
                "data, digital, {}",
                match r {
                    1 => "1200 bit/s",
                    2 => "2400 bit/s",
                    3 => "4800 bit/s",
                    4 => "9600 bit/s",
                    _ => "speed not named",
                }
            ),
            (s, _) => format!("service {s}"),
        }
    }
}

/// Read a call announcement; `None` for any other type.
pub fn announcement(b: &[u8; SU_BYTES]) -> Option<Announcement> {
    (b[0] == 0x20).then(|| Announcement {
        aes: aes(b),
        ges: b[4],
        q_no: b[5] >> 4,
        call_ref: b[5] & 0x0f,
        service: b[6] & 0x0f,
        rate: b[7] >> 4,
    })
}

/// Read a call progress unit; `None` for any other type.
pub fn call_progress(b: &[u8; SU_BYTES]) -> Option<CallProgress> {
    if b[0] != 0x30 {
        return None;
    }
    let report = b[6] & 0x0f;
    // A four-bit signed number: 0xD is -3 dB.
    let nibble = (b[6] >> 4) as i8;
    Some(CallProgress {
        aes: aes(b),
        ges: b[4],
        q_no: b[5] >> 4,
        call_ref: b[5] & 0x0f,
        report,
        eirp_db: (report == 1).then_some(if nibble >= 8 { nibble - 16 } else { nibble }),
        cause: (report == 4).then_some(Cause {
            location: b[6] >> 4,
            class: (b[7] >> 4) & 0x07,
            value: b[7] & 0x0f,
        }),
    })
}

/// What one good signal unit means, as far as the GUI needs.
#[derive(Debug, Clone, PartialEq)]
pub enum Meaning {
    /// Nothing beyond its name.
    Plain,
    /// A unit about one aircraft.
    Aircraft {
        aes: u32,
        ges: u8,
    },
    Assignment(CAssignment),
    /// A data channel a ground station announces: frequency and bit rate.
    Channel {
        ges: u8,
        mhz: f64,
        bit_rate: u32,
        spot_beam: bool,
    },
    /// The satellite: longitude east in degrees and its P channel(s).
    Satellite {
        id: u8,
        longitude: f64,
        psmc_mhz: [f64; 2],
    },
}

/// Read the fields of a P channel unit (JAERO `AeroL::Decode`).
pub fn p_meaning(b: &[u8; SU_BYTES]) -> Meaning {
    let word = |i: usize| (u16::from(b[i]) << 8) | u16::from(b[i + 1]);
    match b[0] {
        0x31..=0x34 => Meaning::Assignment(CAssignment {
            aes: aes(b),
            ges: b[4],
            kind: b[0],
            rx_mhz: rx_mhz(word(6)),
            tx_mhz: tx_mhz(word(8)),
            spot_beam: b[6] & 0x80 != 0,
        }),
        0x40 => {
            let bit_rate = match b[7] >> 4 {
                0 => 600,
                1 => 1200,
                2 => 2400,
                3 => 4800,
                4 => 6000,
                5 => 5250,
                6 => 10500,
                7 => 8400,
                9 => 21000,
                _ => 0,
            };
            Meaning::Channel {
                ges: b[4],
                mhz: rx_mhz(word(8)),
                bit_rate,
                spot_beam: b[8] & 0x80 != 0,
            }
        }
        0x0C => Meaning::Satellite {
            id: ((b[2] << 4) & 0x30) | (b[3] >> 4),
            longitude: f64::from(b[5]) * 1.5,
            psmc_mhz: [
                rx_mhz(word(6)),
                if word(8) & 0x7fff == 0 {
                    0.0
                } else {
                    rx_mhz(word(8))
                },
            ],
        },
        0x10..=0x17 | 0x21 | 0x30 | 0x51 | 0x60..=0x62 | 0x71 => Meaning::Aircraft {
            aes: aes(b),
            ges: b[4],
        },
        _ => Meaning::Plain,
    }
}

/// A complete piece of user data.
#[derive(Debug, Clone, PartialEq)]
pub struct UserData {
    pub aes: u32,
    pub ges: u8,
    /// Priority queue and reference number, which together name the message.
    pub q_no: u8,
    pub ref_no: u8,
    pub bytes: Vec<u8>,
}

struct Partial {
    aes: u32,
    ges: u8,
    q_no: u8,
    ref_no: u8,
    /// Following units still to come.
    remaining: u8,
    octets_in_last: u8,
    bytes: Vec<u8>,
    age: u32,
}

/// Puts user data back together from a 0x71 unit and the units that follow
/// it (JAERO `ISUData`).
#[derive(Default)]
pub struct Reassembler {
    partial: Vec<Partial>,
}

impl Reassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Give every good unit of the channel in order; returns user data when
    /// a unit completes one.
    pub fn push(&mut self, b: &[u8; SU_BYTES]) -> Option<UserData> {
        if b[0] == 0x71 {
            for p in &mut self.partial {
                p.age += 1;
            }
            self.partial.retain(|p| p.age <= 10);
            let new = Partial {
                aes: aes(b),
                ges: b[4],
                q_no: b[5] >> 4,
                ref_no: b[5] & 0x0f,
                remaining: b[6] & 0x3f,
                octets_in_last: (b[7] >> 4).min(8),
                bytes: b[8..10].to_vec(),
                age: 0,
            };
            self.partial
                .retain(|p| !(p.aes == new.aes && p.q_no == new.q_no && p.ref_no == new.ref_no));
            if new.remaining == 0 {
                return Some(UserData {
                    aes: new.aes,
                    ges: new.ges,
                    q_no: new.q_no,
                    ref_no: new.ref_no,
                    bytes: new.bytes,
                });
            }
            self.partial.push(new);
            return None;
        }
        if b[0] & 0xC0 != 0xC0 {
            return None;
        }
        // A following unit counts down: it belongs to the data that expects
        // its number next.
        let (seq, q_no, ref_no) = (b[0] & 0x3f, b[1] >> 4, b[1] & 0x0f);
        // The newest data that fits: its followers come straight after it.
        let i = self
            .partial
            .iter()
            .rposition(|p| p.q_no == q_no && p.ref_no == ref_no && p.remaining == seq + 1)?;
        let p = &mut self.partial[i];
        p.remaining -= 1;
        if p.remaining > 0 {
            p.bytes.extend_from_slice(&b[2..10]);
            return None;
        }
        p.bytes
            .extend_from_slice(&b[2..2 + p.octets_in_last as usize]);
        let done = self.partial.swap_remove(i);
        Some(UserData {
            aes: done.aes,
            ges: done.ges,
            q_no: done.q_no,
            ref_no: done.ref_no,
            bytes: done.bytes,
        })
    }
}

/// An ACARS block carried as user data.
#[derive(Debug, Clone, PartialEq)]
pub struct Acars {
    pub mode: char,
    /// Aircraft registration, leading dots removed.
    pub registration: String,
    pub ack: char,
    pub label: String,
    pub block_id: char,
    pub text: String,
    /// The message continues in another block.
    pub more: bool,
    /// False when a character failed its parity bit.
    pub parity_ok: bool,
}

/// Read user data as ACARS (JAERO `ParserISU::parse`): two 0xFF, SOH, mode,
/// seven registration characters, acknowledge, two label characters, block
/// identifier, STX and text or ETX, then ETX or ETB, the block check and DEL.
/// Characters are 7 bits with odd parity.
pub fn acars(data: &[u8]) -> Option<Acars> {
    if data.len() <= 16 || data[0] != 0xFF || data[1] != 0xFF || !matches!(data[15], 0x02 | 0x83) {
        return None;
    }
    let ch = |i: usize| char::from(data[i] & 0x7f);
    let end = data.len() - 4;
    let mut parity_ok = data[4..11].iter().all(|b| b.count_ones() % 2 == 1);
    let mut text = String::new();
    if data[15] == 0x02 {
        for &b in &data[16..end] {
            parity_ok &= b.count_ones() % 2 == 1;
            match b & 0x7f {
                0x7f => text.push_str("<DEL>"),
                c @ (b'\n' | b'\r' | 0x20..=0x7e) => text.push(char::from(c)),
                _ => text.push('.'),
            }
        }
    }
    Some(Acars {
        mode: ch(3),
        registration: (4..11)
            .map(ch)
            .collect::<String>()
            .trim_start_matches('.')
            .to_string(),
        ack: ch(11),
        // The second label character is often DEL ("_DEL": no information).
        label: [ch(12), ch(13)]
            .iter()
            .map(|&c| if c == '\x7f' { 'd' } else { c })
            .collect(),
        block_id: ch(14),
        text,
        more: data[end] == 0x97,
        parity_ok,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn odd(c: u8) -> u8 {
        if c.count_ones() % 2 == 1 { c } else { c | 0x80 }
    }

    #[test]
    fn assignment_frequencies() {
        // Channel 0x3355 = 13141: 1510 + 13141 * 0.0025 = 1542.8525 MHz.
        let b = [
            0x34, 0x7C, 0x4A, 0x21, 0x82, 0, 0x33, 0x55, 0x33, 0x55, 0, 0,
        ];
        let Meaning::Assignment(a) = p_meaning(&b) else {
            panic!("not an assignment")
        };
        assert_eq!(a.aes, 0x7C4A21);
        assert_eq!(a.ges, 0x82);
        assert!((a.rx_mhz - 1542.8525).abs() < 1e-9);
        assert!((a.tx_mhz - 1644.3525).abs() < 1e-9);
        assert!(!a.spot_beam);
    }

    #[test]
    fn user_data_comes_back_from_three_units() {
        // An ACARS block of 23 octets: 2 in the initial unit, 8 + 8 + 5 after.
        let mut data = vec![0xFF, 0xFF, 0x01, odd(b'2')];
        data.extend(b".VH-XZA".iter().map(|&c| odd(c)));
        data.push(odd(0x15));
        data.extend([odd(b'H'), odd(b'1'), odd(b'F'), 0x02]);
        data.extend(b"HI".iter().map(|&c| odd(c)));
        data.extend([0x83, 0x12, 0x34, 0x7F]);
        assert_eq!(data.len(), 22);
        let mut r = Reassembler::new();
        let mut first = [0u8; 12];
        first[..8].copy_from_slice(&[0x71, 0x7C, 0x4A, 0x21, 0x82, 0x35, 3, 4 << 4]);
        first[8..10].copy_from_slice(&data[..2]);
        assert!(r.push(&first).is_none());
        let mut out = None;
        for (k, chunk) in data[2..].chunks(8).enumerate() {
            let mut su = [0u8; 12];
            su[0] = 0xC0 | (2 - k as u8);
            su[1] = 0x35;
            su[2..2 + chunk.len()].copy_from_slice(chunk);
            out = r.push(&su);
        }
        let got = out.expect("complete");
        assert_eq!(got.aes, 0x7C4A21);
        assert_eq!(got.bytes, data);
        let a = acars(&got.bytes).expect("ACARS");
        assert_eq!(a.registration, "VH-XZA");
        assert_eq!(a.label, "H1");
        assert_eq!(a.text, "HI");
        assert!(a.parity_ok && !a.more);
    }

    #[test]
    fn call_announcement_is_read() {
        // A fax call from the ground, general priority, call 9.
        let mut b = [0u8; SU_BYTES];
        b[..8].copy_from_slice(&[0x20, 0x89, 0x63, 0xE6, 0x90, 0xD9, 0x52, 0xF0]);
        let a = announcement(&b).unwrap();
        assert_eq!((a.aes, a.ges, a.q_no, a.call_ref), (0x8963E6, 0x90, 0xD, 9));
        assert!(a.is_data());
        assert_eq!(a.describe(), "fax (Group 3)");
        b[6] = 0x51;
        assert_eq!(announcement(&b).unwrap().describe(), "telephone");
        b[0] = 0x30;
        assert!(announcement(&b).is_none());
    }

    #[test]
    fn call_progress_is_read() {
        // Status: general priority, call 9 from the ground, power down 2 dB.
        let mut b = [0u8; SU_BYTES];
        b[..8].copy_from_slice(&[0x30, 0x89, 0x63, 0xE6, 0x90, 0xD9, 0xE1, 0x00]);
        let p = call_progress(&b).unwrap();
        assert_eq!((p.aes, p.ges), (0x8963E6, 0x90));
        assert_eq!(
            (p.priority(), p.call_ref, p.report_name()),
            ("general", 9, "status")
        );
        assert_eq!((p.eirp_db, p.cause), (Some(-2), None));
        assert_eq!(
            p.describe(),
            "status  general priority  call 9 (ground to air)  power -2 dB"
        );
        // Result: distress, call 2 from the aircraft, cause class 1 value 6.
        b[5..8].copy_from_slice(&[0xF2, 0x34, 0x96]);
        let p = call_progress(&b).unwrap();
        assert_eq!(
            (p.priority(), p.from_ground(), p.report_name()),
            ("distress", false, "result")
        );
        assert_eq!(
            p.cause,
            Some(Cause {
                location: 3,
                class: 1,
                value: 6
            })
        );
        assert_eq!(p.eirp_db, None);
        // Connect carries neither.
        b[6] = 0x02;
        let p = call_progress(&b).unwrap();
        assert_eq!(
            (p.report_name(), p.eirp_db, p.cause),
            ("connect", None, None)
        );
        b[0] = 0x31;
        assert!(call_progress(&b).is_none());
    }
}
