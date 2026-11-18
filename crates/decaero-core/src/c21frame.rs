// made by CasualArclamp
//! The C channel at 21000 bit/s: the older voice service, 9.6 kbit/s speech.
//!
//! Nothing public describes this frame and JAERO does not receive it: the
//! layout here was worked out from a recording of a call (Rory, 2026-06-22,
//! `c21probe` and `c21info` in the examples), with the 8400 bit/s channel
//! (`cframe.rs`) as the model. It is right when the signal units' CRCs are.
//!
//! Every half second, 10500 channel bits, the two rails alternating:
//! * a 52-bit unique word on each rail (104 bits), the first rail's bit first;
//! * 28 more bits that were the same in every frame seen (14 a rail);
//! * 10368 coded bits: 27 interleaver blocks of 64 x 6, the rate 1/2 code
//!   with nothing punctured, over 5184 information bits, scrambled as on the
//!   other channels.
//!
//! The information bits: 84 spare bits (zeros but for the first six), then
//! 25 times 12 sub-band signalling bits and a 192-bit voice frame (20 ms of
//! speech at 9600 bit/s). The first 24 groups of signalling bits are three
//! 12-octet signal units; the 25th was always zero.
//!
//! The voice frames are handed out as they are; `mplpc` turns them into
//! sound.

use crate::cframe::CStats;
use crate::fec::{self, Encoder, Viterbi};
use crate::pframe::{SU_BYTES, SignalUnit};

/// The unique words of the two rails, first bit in bit 51. They differ in
/// their last few bits only.
pub const UW_A: u64 = 0x4_2DAF_34BB_1136;
pub const UW_B: u64 = 0x4_2DAF_34BB_11C6;
const UW_LEN: u32 = 52;
const UW_MASK: u64 = (1 << UW_LEN) - 1;
/// What follows the unique words, serial (rail A's bit first).
const TAIL: [u8; 28] = [
    0, 1, 0, 1, 1, 0, 1, 0, 0, 0, 1, 1, 1, 1, 0, 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 0, 1, 1,
];

/// Channel bits in a frame.
pub const FRAME_BITS: usize = 10_500;
/// Coded bits after the header.
pub const BODY_BITS: usize = 10_368;
pub const INFO_BITS: usize = 5184;
/// Voice frames in a frame, 20 ms each.
pub const VOICE_FRAMES: usize = 25;
/// Octets in a voice frame (192 bits).
pub const VOICE_BYTES: usize = 24;
const COLS: usize = 6;
const BLOCK: usize = fec::ROWS * COLS;
/// Spare bits before the first signalling group.
const LEAD: usize = 84;
/// A signalling group and a voice frame.
const STEP: usize = 12 + 8 * VOICE_BYTES;

/// One decoded half second of a call.
#[derive(Debug, Clone)]
pub struct C21Frame {
    /// The speech, as the unknown codec packed it.
    pub voice: [[u8; VOICE_BYTES]; VOICE_FRAMES],
    /// The three sub-band signal units.
    pub units: Vec<SignalUnit>,
    /// Unique word bits that were wrong, of 104.
    pub uw_errors: u32,
    pub channel_errors: u32,
    pub channel_bits: u32,
}

/// The serial channel bits of one frame (for tests and a test modulator).
pub fn encode(
    voice: &[[u8; VOICE_BYTES]; VOICE_FRAMES],
    units: &[[u8; SU_BYTES - 2]; 3],
) -> Vec<u8> {
    let sub: Vec<u8> = units
        .iter()
        .flat_map(|u| fec::bytes_to_bits(&fec::with_crc(u)))
        .collect();
    let mut info = vec![0u8; INFO_BITS];
    for (y, v) in voice.iter().enumerate() {
        let at = LEAD + y * STEP;
        if y < 24 {
            info[at..at + 12].copy_from_slice(&sub[y * 12..y * 12 + 12]);
        }
        info[at + 12..at + STEP].copy_from_slice(&fec::bytes_to_bits(v));
    }
    fec::scramble(&mut info);
    let coded = Encoder::new().encode(&info);
    let mut bits = Vec::with_capacity(FRAME_BITS);
    for i in (0..UW_LEN).rev() {
        bits.push(((UW_A >> i) & 1) as u8);
        bits.push(((UW_B >> i) & 1) as u8);
    }
    bits.extend(TAIL);
    for block in coded.chunks_exact(BLOCK) {
        bits.extend(fec::interleave(block, COLS));
    }
    bits
}

/// Unique-word errors allowed per rail when looking for a call.
const ACQUIRE_TOLERANCE: u32 = 9;
/// Frames with a poor unique word and no good signal unit before giving up.
const LOSE_AFTER: u32 = 4;

/// The decoder: soft bits in (rails alternating), frames out.
pub struct C21Decoder {
    /// The last 52 hard bits of each rail (index = bit parity).
    recent: [u64; 2],
    count: u64,
    locked: bool,
    inverted: [bool; 2],
    /// Header bits still to pass before the body.
    tail_left: usize,
    body: Vec<f32>,
    /// Bits of the next unique words seen so far.
    uw_seen: usize,
    uw_errors: u32,
    bad_frames: u32,
    last_good: usize,
    viterbi: Viterbi,
    stats: CStats,
}

impl Default for C21Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl C21Decoder {
    pub fn new() -> Self {
        Self {
            recent: [0; 2],
            count: 0,
            locked: false,
            inverted: [false; 2],
            tail_left: 0,
            body: Vec::with_capacity(BODY_BITS),
            uw_seen: 0,
            uw_errors: 0,
            bad_frames: 0,
            last_good: 0,
            viterbi: Viterbi::new(),
            stats: CStats::default(),
        }
    }

    pub fn locked(&self) -> bool {
        self.locked
    }
    pub fn stats(&self) -> CStats {
        self.stats
    }

    fn miss(&self, rail: usize, word: u64) -> u32 {
        ((self.recent[rail] ^ word) & UW_MASK).count_ones()
    }

    /// Feed soft bits (positive = 1); decoded frames are appended to `out`.
    pub fn push(&mut self, soft_bits: &[f32], out: &mut Vec<C21Frame>) {
        for &raw in soft_bits {
            let rail = (self.count & 1) as usize;
            self.count += 1;
            self.recent[rail] = (self.recent[rail] << 1) | u64::from(raw > 0.0);
            if !self.locked {
                // Rail B's word ends last: test when its last bit is in.
                let other = 1 - rail;
                let (b, a) = (self.miss(rail, UW_B), self.miss(other, UW_A));
                let fits = |m: u32| m <= ACQUIRE_TOLERANCE || m >= UW_LEN - ACQUIRE_TOLERANCE;
                if fits(a) && fits(b) {
                    self.locked = true;
                    self.inverted[rail] = b > ACQUIRE_TOLERANCE;
                    self.inverted[other] = a > ACQUIRE_TOLERANCE;
                    self.uw_errors = a.min(UW_LEN - a) + b.min(UW_LEN - b);
                    self.tail_left = TAIL.len();
                    self.body.clear();
                    self.uw_seen = 0;
                    self.bad_frames = 0;
                }
                continue;
            }
            if self.tail_left > 0 {
                self.tail_left -= 1;
                continue;
            }
            if self.body.len() < BODY_BITS {
                self.body.push(if self.inverted[rail] { -raw } else { raw });
                if self.body.len() == BODY_BITS {
                    let frame = self.decode_body();
                    let good = frame.units.iter().filter(|u| u.crc_ok).count();
                    self.stats.frames += 1;
                    self.stats.uw_bits_wrong += u64::from(frame.uw_errors);
                    self.stats.uw_bits += 2 * u64::from(UW_LEN);
                    self.stats.crc_good += good as u64;
                    self.stats.crc_bad += (frame.units.len() - good) as u64;
                    self.stats.channel_errors += u64::from(frame.channel_errors);
                    self.stats.channel_bits += u64::from(frame.channel_bits);
                    self.last_good = good;
                    out.push(frame);
                }
                continue;
            }
            // The next unique words are arriving.
            self.uw_seen += 1;
            if self.uw_seen < 2 * UW_LEN as usize {
                continue;
            }
            self.uw_seen = 0;
            self.body.clear();
            self.tail_left = TAIL.len();
            // The frame is an even number of bits: the rails keep their words.
            let mut total = 0;
            for (r, word) in [(rail, UW_B), (1 - rail, UW_A)] {
                let mut m = self.miss(r, word);
                if self.inverted[r] {
                    m = UW_LEN - m;
                }
                if m > UW_LEN * 3 / 4 {
                    // The carrier slipped: this rail is now the other way up.
                    self.inverted[r] = !self.inverted[r];
                    m = UW_LEN - m;
                }
                total += m;
            }
            self.uw_errors = total;
            if total > UW_LEN / 2 && self.last_good == 0 {
                self.bad_frames += 1;
                if self.bad_frames >= LOSE_AFTER {
                    self.locked = false;
                    self.stats.lock_losses += 1;
                }
            } else {
                self.bad_frames = 0;
            }
        }
    }

    fn decode_body(&mut self) -> C21Frame {
        let mut plain = Vec::with_capacity(BODY_BITS);
        for block in self.body.chunks_exact(BLOCK) {
            plain.extend(fec::deinterleave(block, COLS));
        }
        let mut info = self.viterbi.decode_block(&plain);
        let (channel_errors, channel_bits) = fec::channel_errors(&info, &plain, true);
        fec::scramble(&mut info);

        let mut voice = [[0u8; VOICE_BYTES]; VOICE_FRAMES];
        let mut sub = Vec::with_capacity(288);
        for (y, v) in voice.iter_mut().enumerate() {
            let at = LEAD + y * STEP;
            if y < 24 {
                sub.extend_from_slice(&info[at..at + 12]);
            }
            v.copy_from_slice(&fec::bits_to_bytes(&info[at + 12..at + STEP]));
        }
        let units = fec::bits_to_bytes(&sub)
            .chunks_exact(SU_BYTES)
            .map(|c| {
                let mut bytes = [0u8; SU_BYTES];
                bytes.copy_from_slice(c);
                SignalUnit {
                    bytes,
                    crc_ok: fec::crc_ok(c),
                }
            })
            .collect();
        C21Frame {
            voice,
            units,
            uw_errors: self.uw_errors,
            channel_errors,
            channel_bits,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lcg(seed: &mut u32) -> u32 {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        *seed
    }

    #[test]
    fn frames_round_trip_either_way_up() {
        let mut seed = 21;
        let mut sent = Vec::new();
        let mut bits = vec![0u8; 37];
        for _ in 0..4 {
            let mut voice = [[0u8; VOICE_BYTES]; VOICE_FRAMES];
            for b in voice.iter_mut().flatten() {
                *b = (lcg(&mut seed) >> 24) as u8;
            }
            let mut units = [[0u8; 10]; 3];
            for b in units.iter_mut().flatten() {
                *b = (lcg(&mut seed) >> 24) as u8;
            }
            bits.extend(encode(&voice, &units));
            sent.push((voice, units));
        }
        assert_eq!(encode(&sent[0].0, &sent[0].1).len(), FRAME_BITS);
        // Plain, and with one rail upside down (a quarter-turn of the carrier).
        for flip_odd in [false, true] {
            let soft: Vec<f32> = bits
                .iter()
                .enumerate()
                .map(|(i, &b)| {
                    let v = if b == 1 { 1.0 } else { -1.0 };
                    if flip_odd && i & 1 == 1 { -v } else { v }
                })
                .collect();
            let mut decoder = C21Decoder::new();
            let mut out = Vec::new();
            for chunk in soft.chunks(777) {
                decoder.push(chunk, &mut out);
            }
            assert_eq!(out.len(), 4);
            for (frame, (voice, units)) in out.iter().zip(&sent) {
                assert_eq!(&frame.voice, voice);
                assert_eq!(frame.channel_errors, 0);
                for (got, want) in frame.units.iter().zip(units) {
                    assert!(got.crc_ok);
                    assert_eq!(&got.bytes[..10], want);
                }
            }
            assert_eq!(decoder.stats().crc_good, 12);
        }
    }
}
