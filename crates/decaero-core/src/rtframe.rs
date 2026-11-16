// made by CasualArclamp
//! The R and T channels: bursts from aircraft to ground.
//!
//! A burst is a preamble, the 32-bit unique word, then coded and interleaved
//! data with no header: the convolutional code and the scrambler both start
//! afresh. An R burst is one 19-octet signal unit (17 and a CRC) in 64 x 5
//! bits. A T burst is a 6-octet header (aircraft, ground station, CRC) and
//! one or more 12-octet signal units: 64 x 5 bits for the first, 64 x 3 more
//! for each further unit. At 600 and 1200 bit/s every one of those pieces is
//! interleaved by itself; at 10500 bit/s the whole burst is one block.
//! Layout from JAERO `RTChannelDeleaveFECScram` (`updateMSK`, `update`) and
//! `RISUData::update` (MIT, (C) Jonathan Olds).

use crate::fec::{self, Encoder, Viterbi};
use crate::pframe::{SU_BYTES, SignalUnit, UNIQUE_WORD};
use crate::su::UserData;

/// Octets in an R channel signal unit without its CRC.
pub const R_BYTES: usize = 17;
/// The most signal units a T burst is taken to carry.
pub const MAX_T_UNITS: usize = 32;

/// What a burst carried.
#[derive(Debug, Clone, PartialEq)]
pub enum Burst {
    /// One R channel signal unit (without its CRC).
    R { unit: [u8; R_BYTES] },
    /// A T channel burst: who sent it, to which ground station, and its units.
    T {
        aes: u32,
        ges: u8,
        units: Vec<SignalUnit>,
    },
}

/// A decoded burst with its quality figures.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub burst: Burst,
    /// Unique word bits that were wrong.
    pub uw_errors: u32,
    pub channel_errors: u32,
    pub channel_bits: u32,
    /// Soft bits from the first after the unique word to the end of the data.
    pub coded_bits: usize,
}

impl Decoded {
    pub fn pre_fec_ber(&self) -> f32 {
        if self.channel_bits == 0 {
            0.0
        } else {
            self.channel_errors as f32 / self.channel_bits as f32
        }
    }
}

/// Coded bits of a burst with `extra` signal units after the first.
const fn coded_len(extra: usize) -> usize {
    fec::ROWS * (5 + 3 * extra)
}

/// Undo the interleaving of the first `coded_len(extra)` soft bits.
/// `whole`: one block over the whole burst (10500 bit/s); otherwise a block
/// of 5 columns then blocks of 3 (JAERO `deinterleaveMSK_ba`).
fn deinterleave(soft: &[f32], extra: usize, whole: bool) -> Vec<f32> {
    if whole {
        return fec::deinterleave(&soft[..coded_len(extra)], 5 + 3 * extra);
    }
    let mut out = fec::deinterleave(&soft[..coded_len(0)], 5);
    for k in 0..extra {
        let at = coded_len(k);
        out.extend(fec::deinterleave(&soft[at..at + fec::ROWS * 3], 3));
    }
    out
}

fn interleave(coded: &[u8], extra: usize, whole: bool) -> Vec<u8> {
    if whole {
        return fec::interleave(coded, 5 + 3 * extra);
    }
    let mut out = fec::interleave(&coded[..coded_len(0)], 5);
    for k in 0..extra {
        let at = coded_len(k);
        out.extend(fec::interleave(&coded[at..at + fec::ROWS * 3], 3));
    }
    out
}

/// Information bits to the coded, interleaved bits of a burst.
fn encode(info: &[u8], extra: usize, whole: bool) -> Vec<u8> {
    let mut bits = info.to_vec();
    bits.resize(coded_len(extra) / 2, 0);
    fec::scramble(&mut bits);
    // The code is flushed by what follows the data; here that is the
    // scrambled padding, as the far end's zeros would be.
    interleave(&Encoder::new().encode(&bits), extra, whole)
}

/// The data bits of an R burst (after the unique word), for the test modulator.
pub fn encode_r(unit: &[u8; R_BYTES], whole: bool) -> Vec<u8> {
    encode(&fec::bytes_to_bits(&fec::with_crc(unit)), 0, whole)
}

/// The data bits of a T burst.
pub fn encode_t(aes: u32, ges: u8, units: &[[u8; SU_BYTES - 2]], whole: bool) -> Vec<u8> {
    let head = [(aes >> 16) as u8, (aes >> 8) as u8, aes as u8, ges];
    let mut bytes = fec::with_crc(&head);
    for u in units {
        bytes.extend(fec::with_crc(u));
    }
    encode(
        &fec::bytes_to_bits(&bytes),
        units.len().saturating_sub(1),
        whole,
    )
}

/// Decode, descramble and count the corrected errors of one trial length.
fn try_len(soft: &[f32], extra: usize, whole: bool, viterbi: &mut Viterbi) -> (Vec<u8>, u32, u32) {
    let plain = deinterleave(soft, extra, whole);
    let mut info = viterbi.decode_block(&plain);
    let (errors, total) = fec::channel_errors(&info, &plain, true);
    fec::scramble(&mut info);
    (fec::bits_to_bytes(&info), errors, total)
}

/// Decode a burst from the soft bits that follow its unique word. `whole`
/// says the burst is interleaved as one block (10500 bit/s). The burst's end
/// is wherever `soft` ends, give or take: every length that fits is tried.
pub fn decode(soft: &[f32], whole: bool) -> Option<Decoded> {
    if soft.len() < coded_len(0) {
        return None;
    }
    let mut viterbi = Viterbi::new();
    // An R burst is the shortest thing there is.
    let (bytes, errors, total) = try_len(soft, 0, whole, &mut viterbi);
    if fec::crc_ok(&bytes[..R_BYTES + 2]) {
        let mut unit = [0u8; R_BYTES];
        unit.copy_from_slice(&bytes[..R_BYTES]);
        return Some(Decoded {
            burst: Burst::R { unit },
            uw_errors: 0,
            channel_errors: errors,
            channel_bits: total,
            coded_bits: coded_len(0),
        });
    }
    // A T burst: the longest that fits first.
    let fits = ((soft.len() / fec::ROWS).saturating_sub(5) / 3).min(MAX_T_UNITS - 1);
    let lengths: Vec<usize> = if whole {
        // One interleaver over the burst: the length must be exactly right.
        (0..=fits).rev().collect()
    } else {
        // Separate blocks: the longest reading holds every shorter one.
        vec![fits]
    };
    for extra in lengths {
        let (bytes, errors, total) = try_len(soft, extra, whole, &mut viterbi);
        if !fec::crc_ok(&bytes[..6]) {
            continue;
        }
        let mut units: Vec<SignalUnit> = bytes[6..]
            .chunks_exact(SU_BYTES)
            .take(extra + 1)
            .map(|c| {
                let mut b = [0u8; SU_BYTES];
                b.copy_from_slice(c);
                SignalUnit {
                    bytes: b,
                    crc_ok: fec::crc_ok(c),
                }
            })
            .collect();
        // What lies past the real end of the burst is noise: drop it.
        while units.last().is_some_and(|u| !u.crc_ok) {
            units.pop();
        }
        if units.is_empty() {
            continue;
        }
        let coded_bits = coded_len(units.len() - 1);
        return Some(Decoded {
            burst: Burst::T {
                aes: (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]),
                ges: bytes[3],
                units,
            },
            uw_errors: 0,
            // Scaled to the part that was the burst.
            channel_errors: (u64::from(errors) * coded_bits as u64 / coded_len(extra) as u64)
                as u32,
            channel_bits: (u64::from(total) * coded_bits as u64 / coded_len(extra) as u64) as u32,
            coded_bits,
        });
    }
    None
}

/// Places in a soft-bit stream where the unique word ends, with how many of
/// its bits were wrong and whether each rail is upside down. `doubled`: the
/// word is on both rails of an A-QPSK burst, so each bit comes twice.
pub fn find_unique_words(
    soft: &[f32],
    doubled: bool,
    tolerance: u32,
) -> Vec<(usize, u32, [bool; 2])> {
    let len = if doubled { 64 } else { 32 };
    let mut word = 0u64;
    for i in (0..32).rev() {
        let b = u64::from((UNIQUE_WORD >> i) & 1);
        word = if doubled {
            (word << 2) | (b * 3)
        } else {
            (word << 1) | b
        };
    }
    let mask = if doubled { u64::MAX } else { (1 << 32) - 1 };
    let newest_rail = 0x5555_5555_5555_5555u64;
    let mut recent = 0u64;
    let mut found = Vec::new();
    for (i, &s) in soft.iter().enumerate() {
        recent = (recent << 1) | u64::from(s > 0.0);
        if i + 1 < len {
            continue;
        }
        let diff = (recent ^ word) & mask;
        let rails = if doubled {
            [
                (diff & newest_rail).count_ones(),
                (diff & !newest_rail).count_ones(),
            ]
        } else {
            [diff.count_ones(); 2]
        };
        if rails.iter().all(|&m| m <= tolerance || m >= 32 - tolerance) {
            // Index parity of the newest bit says which rail it is on.
            let newest = i & 1;
            let mut inverted = [false; 2];
            inverted[newest] = rails[0] > tolerance;
            inverted[1 - newest] = rails[1] > tolerance;
            let errors = if doubled {
                rails.iter().map(|&m| m.min(32 - m)).sum()
            } else {
                rails[0].min(32 - rails[0])
            };
            found.push((i + 1, errors, inverted));
        }
    }
    found
}

/// Find the unique word in a burst's soft bits and decode what follows.
pub fn decode_burst(soft: &[f32], whole: bool) -> Option<Decoded> {
    // JAERO allows four wrong bits in a burst's unique word.
    for (start, uw_errors, inverted) in find_unique_words(soft, whole, 4) {
        let data: Vec<f32> = soft[start..]
            .iter()
            .enumerate()
            .map(|(k, &s)| if inverted[(start + k) & 1] { -s } else { s })
            .collect();
        if let Some(mut d) = decode(&data, whole) {
            d.uw_errors = uw_errors;
            return Some(d);
        }
    }
    None
}

/// The name of an R channel signalling unit (JAERO `AEROTypeR`).
pub fn r_name(kind: u8) -> &'static str {
    match kind {
        0x12 => "log-on/log-off control",
        0x15 => "log-on/log-off acknowledge",
        0x17 => "ready for reassignment",
        0x20 => "access request (telephone)",
        0x22 => "access request (data)",
        0x23 => "abbreviated access request (telephone)",
        0x30 => "call progress",
        0x60 => "telephony acknowledge",
        0x61 => "request for acknowledgement",
        0x62 => "acknowledge",
        _ => "signalling",
    }
}

/// An R channel unit that carries user data (JAERO `RISUData::update`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RData {
    pub aes: u32,
    pub ges: u8,
    pub q_no: u8,
    pub ref_no: u8,
    /// Which unit of how many (1 of 1 up to 3 of 3).
    pub index: usize,
    pub total: usize,
    /// Octets of user data in this unit (up to 11).
    pub octets: usize,
}

/// Read an R unit's header if it is a user-data unit.
pub fn r_data(unit: &[u8; R_BYTES]) -> Option<RData> {
    if unit[1] & 0x08 == 0 {
        return None;
    }
    let (total, index) = match unit[0] >> 4 {
        1 => (1, 0),
        2 => (2, 0),
        3 => (2, 1),
        4 => (3, 0),
        5 => (3, 1),
        6 => (3, 2),
        _ => return None,
    };
    let octets = usize::from(unit[0] & 0x0f);
    if !(1..=11).contains(&octets) {
        return None;
    }
    Some(RData {
        aes: (u32::from(unit[2]) << 16) | (u32::from(unit[3]) << 8) | u32::from(unit[4]),
        ges: unit[5],
        q_no: unit[1] >> 4,
        ref_no: unit[1] & 0x07,
        index,
        total,
        octets,
    })
}

struct RPartial {
    head: RData,
    bytes: Vec<u8>,
    have: u8,
    length: Option<usize>,
    age: u32,
}

/// Puts R channel user data back together from its one to three units.
#[derive(Default)]
pub struct RReassembler {
    partial: Vec<RPartial>,
}

impl RReassembler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Give every R unit; returns the user data when one completes it.
    pub fn push(&mut self, unit: &[u8; R_BYTES]) -> Option<UserData> {
        let head = r_data(unit)?;
        for p in &mut self.partial {
            p.age += 1;
        }
        self.partial.retain(|p| p.age <= 10);
        let same = |p: &RPartial| {
            p.head.aes == head.aes
                && p.head.ges == head.ges
                && p.head.q_no == head.q_no
                && p.head.ref_no == head.ref_no
        };
        let i = match self.partial.iter().position(same) {
            Some(i) => i,
            None => {
                self.partial.push(RPartial {
                    head,
                    bytes: Vec::new(),
                    have: 0,
                    length: None,
                    age: 0,
                });
                self.partial.len() - 1
            }
        };
        let p = &mut self.partial[i];
        p.age = 0;
        // Units before the last are full (11 octets).
        let size = 11 * (head.total - 1)
            + if head.index + 1 == head.total {
                head.octets
            } else {
                11
            };
        if p.bytes.len() < size {
            p.bytes.resize(size, 0);
        }
        let n = if head.index + 1 == head.total {
            head.octets
        } else {
            11
        };
        p.bytes[11 * head.index..11 * head.index + n].copy_from_slice(&unit[6..6 + n]);
        p.have |= 1 << head.index;
        if head.index + 1 == head.total {
            // The last unit says how long the whole is.
            p.length = Some(11 * (head.total - 1) + head.octets);
        }
        if p.have == (1 << head.total) - 1 {
            let mut done = self.partial.swap_remove(i);
            if let Some(n) = done.length {
                done.bytes.truncate(n);
            }
            return Some(UserData {
                aes: done.head.aes,
                ges: done.head.ges,
                q_no: done.head.q_no,
                ref_no: done.head.ref_no,
                bytes: done.bytes,
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u32) -> u32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *seed
    }
    fn gauss(seed: &mut u32) -> f32 {
        (0..12)
            .map(|_| (lcg(seed) >> 8) as f32 / 16_777_216.0)
            .sum::<f32>()
            - 6.0
    }
    fn uw_bits(doubled: bool) -> Vec<u8> {
        let w = (0..32).rev().map(|i| ((UNIQUE_WORD >> i) & 1) as u8);
        if doubled {
            w.flat_map(|b| [b, b]).collect()
        } else {
            w.collect()
        }
    }
    /// A burst as the demodulator would hand it over: some preamble, the
    /// unique word, the data, a little noise after; `sigma` of noise on top.
    fn on_air(data: &[u8], doubled: bool, sigma: f32, flip: [bool; 2], seed: &mut u32) -> Vec<f32> {
        let mut bits: Vec<u8> = (0..41).map(|i| (i & 1) as u8).collect();
        bits.extend(uw_bits(doubled));
        bits.extend_from_slice(data);
        let mut soft: Vec<f32> = bits
            .iter()
            .enumerate()
            .map(|(i, &b)| {
                (2.0 * f32::from(b) - 1.0) * if flip[i & 1] { -1.0 } else { 1.0 }
                    + sigma * gauss(seed)
            })
            .collect();
        soft.extend((0..150).map(|_| 0.7 * gauss(seed)));
        soft
    }

    #[test]
    fn r_burst_round_trip_both_modulations() {
        let mut seed = 1;
        let mut unit = [0u8; R_BYTES];
        for b in &mut unit {
            *b = (lcg(&mut seed) >> 24) as u8;
        }
        for (whole, flip) in [
            (false, [false, false]),
            (false, [true, true]),
            (true, [false, true]),
            (true, [true, false]),
        ] {
            let soft = on_air(&encode_r(&unit, whole), whole, 0.0, flip, &mut seed);
            let d = decode_burst(&soft, whole).expect("decodes");
            assert_eq!(d.burst, Burst::R { unit });
            assert_eq!(d.channel_errors, 0);
            assert_eq!(d.uw_errors, 0);
        }
    }

    #[test]
    fn t_burst_round_trip_with_noise() {
        let mut seed = 9;
        for whole in [false, true] {
            for n in [1usize, 2, 5, 10] {
                let units: Vec<[u8; 10]> = (0..n)
                    .map(|_| {
                        let mut u = [0u8; 10];
                        for b in &mut u {
                            *b = (lcg(&mut seed) >> 24) as u8;
                        }
                        u
                    })
                    .collect();
                // Es/N0 about 3 dB: a few per cent of raw bit errors.
                let soft = on_air(
                    &encode_t(0x7C4A21, 0x82, &units, whole),
                    whole,
                    0.5,
                    [false, false],
                    &mut seed,
                );
                let d = decode_burst(&soft, whole)
                    .unwrap_or_else(|| panic!("{n} units, whole {whole}"));
                let Burst::T {
                    aes,
                    ges,
                    units: got,
                } = &d.burst
                else {
                    panic!("not a T burst")
                };
                assert_eq!((*aes, *ges), (0x7C4A21, 0x82));
                assert_eq!(got.len(), n, "whole {whole}");
                for (g, u) in got.iter().zip(&units) {
                    assert!(g.crc_ok);
                    assert_eq!(&g.bytes[..10], u);
                }
                assert!(
                    d.channel_errors > 0 && d.pre_fec_ber() < 0.08,
                    "{}",
                    d.pre_fec_ber()
                );
            }
        }
    }

    #[test]
    fn noise_is_not_a_burst() {
        let mut seed = 77;
        let noise: Vec<f32> = (0..40_000).map(|_| gauss(&mut seed)).collect();
        assert!(decode_burst(&noise, false).is_none());
        assert!(decode_burst(&noise, true).is_none());
    }

    #[test]
    fn r_user_data_from_two_units() {
        let data: Vec<u8> = (1..=15).collect();
        let mut r = RReassembler::new();
        let mut first = [0u8; R_BYTES];
        first[..6].copy_from_slice(&[0x2B, 0x38, 0x7C, 0x4A, 0x21, 0x82]);
        first[6..17].copy_from_slice(&data[..11]);
        assert!(r.push(&first).is_none());
        let mut second = [0u8; R_BYTES];
        second[..6].copy_from_slice(&[0x34, 0x38, 0x7C, 0x4A, 0x21, 0x82]);
        second[6..10].copy_from_slice(&data[11..]);
        let whole = r.push(&second).expect("complete");
        assert_eq!((whole.aes, whole.ges, whole.bytes), (0x7C4A21, 0x82, data));
    }
}
