// made by CasualArclamp
//! ADS-C reports in downlink ACARS text (ARINC 622 wrapping, ARINC 745 groups).
//!
//! An aircraft's ACARS text starts with a short header (originator, message
//! number, block letter, flight) and, for an ADS-C report, goes on
//! `/GROUND.ADS.REGISTR<hex groups><hex CRC>`. The layout of every group and
//! its scale is JAERO's `ArincParse::parseDownlinkmessage` (MIT).

/// One group of an ADS-C report.
#[derive(Debug, Clone, PartialEq)]
pub enum Group {
    /// Position: tags 7 (basic), 9 (emergency), 10, 18, 19, 20 (events).
    Position {
        tag: u8,
        latitude: f64,
        longitude: f64,
        altitude_ft: f64,
        /// Seconds past the hour the position was taken.
        past_hour_s: f64,
        /// Figure of merit and its flags (5 bits).
        fom: u8,
    },
    /// Over the ground (tag 14).
    EarthReference {
        true_track: Option<f64>,
        ground_speed_kt: f64,
        vertical_fpm: f64,
    },
    /// Through the air (tag 15).
    AirReference {
        true_heading: Option<f64>,
        mach: f64,
        vertical_fpm: f64,
    },
    /// Tag 16.
    Weather {
        wind_kt: f64,
        wind_from: Option<f64>,
        temperature_c: f64,
    },
    /// Tag 12.
    FlightId(String),
    /// Next waypoint (tag 13) or a fixed intent point (tag 23).
    Waypoint {
        fixed_intent: bool,
        latitude: f64,
        longitude: f64,
        altitude_ft: f64,
        /// Seconds from the report to the point.
        eta_s: f64,
    },
    /// Tag 22.
    IntermediateIntent {
        distance_nm: f64,
        true_track: Option<f64>,
        altitude_ft: f64,
        time_s: f64,
    },
    Acknowledge {
        contract: u8,
    },
    NegativeAcknowledge {
        contract: u8,
        reason: u8,
    },
    /// Understood only by name.
    Other(&'static str),
}

/// What an aircraft's ACARS text says beyond the text itself.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Downlink {
    /// Flight from the text header ("QF0012" is given as "QF12"), if it
    /// looks like one.
    pub flight: Option<String>,
    /// Registration inside the ADS-C wrapping, dots removed.
    pub registration: String,
    /// ADS-C groups; empty if the text is not an ADS-C report or its check
    /// failed.
    pub groups: Vec<Group>,
    /// A group was cut short or unknown: the groups before it stand.
    pub truncated: bool,
}

impl Downlink {
    pub fn position(&self) -> Option<&Group> {
        self.groups
            .iter()
            .find(|g| matches!(g, Group::Position { .. }))
    }
    pub fn earth_reference(&self) -> Option<&Group> {
        self.groups
            .iter()
            .find(|g| matches!(g, Group::EarthReference { .. }))
    }

    /// The groups in words, a line each; empty without groups.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        for g in &self.groups {
            let line = match g {
                Group::Position {
                    tag,
                    latitude,
                    longitude,
                    altitude_ft,
                    past_hour_s,
                    fom,
                } => format!(
                    "{}: {latitude:.4} {longitude:.4}, {altitude_ft:.0} ft, at {:02}m {:02}s past the hour, FOM {fom:02X}",
                    position_name(*tag),
                    (*past_hour_s / 60.0) as u32,
                    (*past_hour_s % 60.0) as u32,
                ),
                Group::EarthReference {
                    true_track,
                    ground_speed_kt,
                    vertical_fpm,
                } => format!(
                    "{}ground speed {ground_speed_kt:.0} kt, vertical rate {vertical_fpm:.0} ft/min",
                    true_track
                        .map(|t| format!("true track {t:.0} deg, "))
                        .unwrap_or_default()
                ),
                Group::AirReference {
                    true_heading,
                    mach,
                    vertical_fpm,
                } => format!(
                    "{}Mach {mach:.2}, vertical rate {vertical_fpm:.0} ft/min",
                    true_heading
                        .map(|t| format!("true heading {t:.0} deg, "))
                        .unwrap_or_default()
                ),
                Group::Weather {
                    wind_kt,
                    wind_from,
                    temperature_c,
                } => format!(
                    "wind {wind_kt:.0} kt{}, {temperature_c:.2} deg C",
                    wind_from
                        .map(|d| format!(" from {d:.0} deg"))
                        .unwrap_or_default()
                ),
                Group::FlightId(id) => format!("flight {id}"),
                Group::Waypoint {
                    fixed_intent,
                    latitude,
                    longitude,
                    altitude_ft,
                    eta_s,
                } => format!(
                    "{}: {latitude:.4} {longitude:.4}, {altitude_ft:.0} ft, in {}",
                    if *fixed_intent {
                        "fixed intent"
                    } else {
                        "next waypoint"
                    },
                    span(*eta_s)
                ),
                Group::IntermediateIntent {
                    distance_nm,
                    true_track,
                    altitude_ft,
                    time_s,
                } => format!(
                    "intermediate intent: {distance_nm:.1} nm{}, {altitude_ft:.0} ft, in {}",
                    true_track
                        .map(|t| format!(", true track {t:.0} deg"))
                        .unwrap_or_default(),
                    span(*time_s)
                ),
                Group::Acknowledge { contract } => format!("acknowledge, contract {contract}"),
                Group::NegativeAcknowledge { contract, reason } => {
                    format!("negative acknowledge, contract {contract}, reason {reason:02X}")
                }
                Group::Other(name) => (*name).to_string(),
            };
            out.push_str(&line);
            out.push('\n');
        }
        if self.truncated {
            out.push_str("...\n");
        }
        out.pop();
        out
    }
}

fn position_name(tag: u8) -> &'static str {
    match tag {
        9 => "emergency position",
        10 => "lateral deviation change",
        18 => "vertical rate change",
        19 => "altitude range event",
        20 => "waypoint change",
        _ => "position",
    }
}

fn span(seconds: f64) -> String {
    let s = seconds as u32;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// ARINC 622 check: CRC-16 with polynomial 0x1021, most significant bit
/// first, start 0xFFFF, inverted (JAERO `calcusingbytesotherendines`).
pub fn crc_622(bytes: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &b in bytes {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    !crc
}

/// `bits` bits whose lowest one is bit `shift` of `b[last]` (bit 0 the least
/// significant), the rest above it in that octet and the ones before it.
/// This is how JAERO's `extractqint32` addresses a field.
fn field(b: &[u8], last: usize, shift: u32, bits: u32, signed: bool) -> f64 {
    let first = last.saturating_sub(4);
    let mut all = 0u64;
    for &x in &b[first..=last] {
        all = (all << 8) | u64::from(x);
    }
    let raw = (all >> shift) & ((1u64 << bits) - 1);
    if signed && raw >> (bits - 1) & 1 == 1 {
        raw as f64 - (1u64 << bits) as f64
    } else {
        raw as f64
    }
}

const ANGLE: f64 = 0.000171661376953125; // 180 / 2^20 degrees
const TRACK: f64 = 0.087890625; // 180 / 2^11 degrees

fn turn(deg: f64) -> f64 {
    if deg < 0.0 { deg + 360.0 } else { deg }
}

/// Read an aircraft's ACARS text. Always gives the flight if the header has
/// one; the groups only when the text is an ADS-C report whose check holds.
pub fn parse(text: &str) -> Downlink {
    let mut out = Downlink::default();
    if !text.is_ascii() || text.len() < 10 {
        return out;
    }
    // Header: originator, two digits of message number, block letter, then
    // six characters that are usually the flight.
    if text.as_bytes()[1..3].iter().all(u8::is_ascii_digit) {
        let id = &text[4..10];
        let letters = id.bytes().take_while(u8::is_ascii_uppercase).count();
        let flight = format!(
            "{}{}",
            &id[..letters],
            id[letters..].trim_start_matches('0')
        );
        if flight.len() >= 3 && letters > 0 && flight.bytes().all(|c| c.is_ascii_alphanumeric()) {
            out.flight = Some(flight);
        }
    }
    let mut parts = text.split('/');
    let (Some(_), Some(body), None) = (parts.next(), parts.next(), parts.next()) else {
        return out;
    };
    let Some((_ground, rest)) = body.split_once('.') else {
        return out;
    };
    let rest = rest.trim_end_matches(['\r', '\n', ' ']);
    if !rest.starts_with("ADS") || rest.len() < 3 + 7 + 4 {
        return out;
    }
    let tail = &rest[3..10];
    let hex = &rest[10..rest.len() - 4];
    let Ok(crc) = u16::from_str_radix(&rest[rest.len() - 4..], 16) else {
        return out;
    };
    if hex.len() % 2 != 0 {
        return out;
    }
    let Some(b) = (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok())
        .collect::<Option<Vec<u8>>>()
    else {
        return out;
    };
    let mut checked = rest.as_bytes()[..10].to_vec();
    checked.extend_from_slice(&b);
    if crc_622(&checked) != crc {
        return out;
    }
    out.registration = tail.replace('.', "");

    let mut i = 0;
    while i < b.len() {
        // `at(k)` is the k-th octet of the group, the tag being the first.
        let need = |n: usize| i + n <= b.len();
        let (group, size) = match b[i] {
            3 if need(2) => (Group::Acknowledge { contract: b[i + 1] }, 2),
            4 if need(4) => (
                Group::NegativeAcknowledge {
                    contract: b[i + 1],
                    reason: b[i + 2],
                },
                4,
            ),
            5 if need(6) => (Group::Other("noncompliance notification"), 6),
            6 => (Group::Other("emergency mode cancelled"), 1),
            tag @ (7 | 9 | 10 | 18 | 19 | 20) if need(11) => (
                Group::Position {
                    tag,
                    latitude: field(&b, i + 3, 3, 21, true) * ANGLE,
                    longitude: field(&b, i + 6, 6, 21, true) * ANGLE,
                    altitude_ft: field(&b, i + 8, 6, 16, true) * 4.0,
                    past_hour_s: field(&b, i + 10, 7, 15, false) * 0.125,
                    fom: b[i + 10] & 0x1f,
                },
                11,
            ),
            12 if need(7) => {
                // Eight characters of six bits; 1 to 26 are the letters.
                let id: String = (0..8)
                    .map(|k| {
                        let bit = 8 + 6 * k;
                        let last = i + (bit + 5) / 8;
                        let shift = 7 - (bit + 5) % 8;
                        let c = field(&b, last, shift as u32, 6, false) as u8;
                        char::from(if c <= 26 { c | 0x40 } else { c })
                    })
                    .collect();
                (Group::FlightId(id.trim().to_string()), 7)
            }
            tag @ (13 | 23) if need(if tag == 13 { 18 } else { 10 }) => (
                Group::Waypoint {
                    fixed_intent: tag == 23,
                    latitude: field(&b, i + 3, 3, 21, true) * ANGLE,
                    longitude: field(&b, i + 6, 6, 21, true) * ANGLE,
                    altitude_ft: field(&b, i + 8, 6, 16, true) * 4.0,
                    eta_s: field(&b, i + 9, 0, 14, false),
                },
                if tag == 13 { 18 } else { 10 },
            ),
            14 if need(6) => (
                Group::EarthReference {
                    true_track: (b[i + 1] & 0x80 == 0)
                        .then(|| turn(field(&b, i + 2, 3, 12, true) * TRACK)),
                    ground_speed_kt: field(&b, i + 4, 6, 13, false) * 0.5,
                    vertical_fpm: field(&b, i + 5, 2, 12, true) * 16.0,
                },
                6,
            ),
            15 if need(6) => (
                Group::AirReference {
                    true_heading: (b[i + 1] & 0x80 == 0)
                        .then(|| turn(field(&b, i + 2, 3, 12, true) * TRACK)),
                    mach: field(&b, i + 4, 6, 13, false) * 0.0005,
                    vertical_fpm: field(&b, i + 5, 2, 12, true) * 16.0,
                },
                6,
            ),
            16 if need(5) => (
                Group::Weather {
                    wind_kt: field(&b, i + 2, 7, 9, false) * 0.5,
                    wind_from: (b[i + 2] & 0x40 == 0)
                        .then(|| turn(field(&b, i + 3, 5, 9, true) * 0.703125)),
                    temperature_c: field(&b, i + 4, 1, 12, true) * 0.25,
                },
                5,
            ),
            17 if need(4) => (Group::Other("airframe identification"), 4),
            22 if need(9) => (
                Group::IntermediateIntent {
                    distance_nm: field(&b, i + 2, 0, 16, false) * 0.125,
                    true_track: (b[i + 3] & 0x80 == 0)
                        .then(|| turn(field(&b, i + 4, 3, 12, true) * TRACK)),
                    altitude_ft: field(&b, i + 6, 3, 16, true) * 4.0,
                    time_s: field(&b, i + 8, 5, 14, false),
                },
                9,
            ),
            _ => {
                out.truncated = true;
                break;
            }
        };
        out.groups.push(group);
        i += size;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // The example in JAERO's arincparse.cpp.
    const EXAMPLE: &str = "F58ANZ0124/AKLCDYA.ADS.N705DN07EEE19454DAC7D010D21D0DEEEC44556208024029F0588C71D7884D000E13B90F00000F12C1A280001029305F1019F4";

    #[test]
    fn jaero_example() {
        let d = parse(EXAMPLE);
        assert_eq!(d.flight.as_deref(), Some("NZ124"));
        assert_eq!(d.registration, "N705DN");
        assert!(!d.truncated, "{}", d.describe());
        let Some(Group::Position {
            latitude,
            longitude,
            altitude_ft,
            ..
        }) = d.position()
        else {
            panic!("no position")
        };
        assert!((-90.0..=90.0).contains(latitude));
        assert!((-180.0..=180.0).contains(longitude));
        assert!((20000.0..45000.0).contains(altitude_ft), "{altitude_ft}");
        assert!(d.earth_reference().is_some());
    }

    #[test]
    fn check_guards_the_groups() {
        let broken = EXAMPLE.replace("07EEE1", "07EEE0");
        assert!(parse(&broken).groups.is_empty());
        assert_eq!(parse(&broken).flight.as_deref(), Some("NZ124"));
    }

    #[test]
    fn field_addressing() {
        // 21 bits from the top of three octets, signed.
        let b = [0x00, 0xFF, 0xFF, 0xF8];
        assert_eq!(field(&b, 3, 3, 21, true), -1.0);
        assert_eq!(field(&b, 3, 3, 21, false), 2097151.0);
    }

    #[test]
    fn plain_text_is_left_alone() {
        let d = parse("hello there, nothing to see");
        assert!(d.groups.is_empty() && d.flight.is_none());
    }
}
