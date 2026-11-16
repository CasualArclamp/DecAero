// made by CasualArclamp
//! The C channel at 8400 bit/s: one circuit-mode voice call.
//!
//! Every half second: a 52-bit unique word on each rail, then 4096 channel
//! bits. Those are sixteen interleaver blocks of 64 x 4, a rate 2/3 punctured
//! code over 2714 information bits plus six flush bits, and 16 dummy bits.
//! The information bits are one spare bit, then 25 voice frames of 96 bits
//! with 12 sub-band signalling bits (and a spare) after each of the first 24;
//! the 288 signalling bits are three 12-octet signal units.
//! Layout from JAERO `AeroL::DecodeC` (MIT, (C) Jonathan Olds).

use crate::fec::{self, Encoder, Viterbi};
use crate::pframe::{SU_BYTES, SignalUnit};

/// The unique words of the two rails, first bit in bit 51 (JAERO
/// `preambledetectorreal.setPreamble`).
pub const UW_I: u64 = 0xA_B376_938B_CA30;
pub const UW_Q: u64 = 0x0_C53D_1C96_ECD5;
const UW_LEN: u32 = 52;
const UW_MASK: u64 = (1 << UW_LEN) - 1;

/// Channel bits after the unique words.
pub const BODY_BITS: usize = 4096;
/// Information bits in a frame.
pub const INFO_BITS: usize = 2714;
/// Voice frames in a frame, 20 ms each.
pub const VOICE_FRAMES: usize = 25;
/// Octets in a voice frame.
pub const VOICE_BYTES: usize = 12;
const BLOCK: usize = fec::ROWS * 4;
/// Coded bits that carry the information and flush bits (the rest is dummy).
const CODED: usize = (INFO_BITS + 6) * 3 / 2;

/// One decoded half second of a call.
#[derive(Debug, Clone)]
pub struct CFrame {
    pub voice: [[u8; VOICE_BYTES]; VOICE_FRAMES],
    /// The three sub-band signal units.
    pub units: Vec<SignalUnit>,
    /// Unique word bits that were wrong, of 104.
    pub uw_errors: u32,
    pub channel_errors: u32,
    pub channel_bits: u32,
}

impl CFrame {
    pub fn pre_fec_ber(&self) -> f32 {
        if self.channel_bits == 0 {
            0.0
        } else {
            self.channel_errors as f32 / self.channel_bits as f32
        }
    }
}

/// The information bits of a frame from its voice frames and signal units
/// (for the test modulator).
fn pack_info(
    voice: &[[u8; VOICE_BYTES]; VOICE_FRAMES],
    units: &[[u8; SU_BYTES - 2]; 3],
) -> Vec<u8> {
    let sub: Vec<u8> = units
        .iter()
        .flat_map(|u| fec::bytes_to_bits(&fec::with_crc(u)))
        .collect();
    let mut info = vec![0u8; INFO_BITS];
    for (y, v) in voice.iter().enumerate() {
        let start = 1 + y * 109;
        info[start..start + 96].copy_from_slice(&fec::bytes_to_bits(v));
        if y < 24 {
            info[start + 96..start + 108].copy_from_slice(&sub[y * 12..y * 12 + 12]);
        }
    }
    info
}

/// The serial channel bits of one frame, unique words included: the two
/// rails alternate, the Q word's bit first.
pub fn encode(
    voice: &[[u8; VOICE_BYTES]; VOICE_FRAMES],
    units: &[[u8; SU_BYTES - 2]; 3],
) -> Vec<u8> {
    let mut info = pack_info(voice, units);
    fec::scramble(&mut info);
    info.extend([0; 6]);
    let mut coded = fec::puncture_2_3(&Encoder::new().encode(&info));
    coded.resize(BODY_BITS, 0);
    let mut bits = Vec::with_capacity(2 * UW_LEN as usize + BODY_BITS);
    for i in (0..UW_LEN).rev() {
        bits.push(((UW_Q >> i) & 1) as u8);
        bits.push(((UW_I >> i) & 1) as u8);
    }
    for block in coded.chunks_exact(BLOCK) {
        bits.extend(fec::interleave(block, 4));
    }
    bits
}

/// Running totals for the GUI.
#[derive(Debug, Clone, Copy, Default)]
pub struct CStats {
    pub frames: u64,
    pub uw_bits_wrong: u64,
    pub uw_bits: u64,
    pub crc_good: u64,
    pub crc_bad: u64,
    pub channel_errors: u64,
    pub channel_bits: u64,
    pub lock_losses: u64,
}

/// Unique-word errors allowed per rail when looking for a call. Chance
/// agreement of both rails within this is far below once a day.
const ACQUIRE_TOLERANCE: u32 = 11;
/// Frames with a poor unique word and no good signal unit before giving up.
const LOSE_AFTER: u32 = 4;

/// The C channel decoder: soft bits in (rails alternating), frames out.
pub struct CDecoder {
    /// The last 52 hard bits of each rail (index = bit parity).
    recent: [u64; 2],
    count: u64,
    locked: bool,
    inverted: [bool; 2],
    /// Which word the rail of even parity carries: true for the I word.
    even_is_i: bool,
    body: Vec<f32>,
    uw_seen: usize,
    uw_errors: u32,
    bad_frames: u32,
    /// Good signal units in the frame just decoded.
    last_good: usize,
    viterbi: Viterbi,
    stats: CStats,
}

impl Default for CDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl CDecoder {
    pub fn new() -> Self {
        Self {
            recent: [0; 2],
            count: 0,
            locked: false,
            inverted: [false; 2],
            even_is_i: false,
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
    pub fn push(&mut self, soft_bits: &[f32], out: &mut Vec<CFrame>) {
        for &raw in soft_bits {
            let rail = (self.count & 1) as usize;
            self.count += 1;
            self.recent[rail] = (self.recent[rail] << 1) | u64::from(raw > 0.0);
            if !self.locked {
                // Both words end together: test when the second rail's last bit is in.
                let other = 1 - rail;
                for (word_here, word_other, here_is_i) in [(UW_I, UW_Q, true), (UW_Q, UW_I, false)]
                {
                    let (a, b) = (self.miss(rail, word_here), self.miss(other, word_other));
                    let fits = |m: u32| m <= ACQUIRE_TOLERANCE || m >= UW_LEN - ACQUIRE_TOLERANCE;
                    if fits(a) && fits(b) {
                        self.locked = true;
                        self.inverted[rail] = a > ACQUIRE_TOLERANCE;
                        self.inverted[other] = b > ACQUIRE_TOLERANCE;
                        self.even_is_i = if rail == 0 { here_is_i } else { !here_is_i };
                        self.uw_errors = a.min(UW_LEN - a) + b.min(UW_LEN - b);
                        self.body.clear();
                        self.uw_seen = 0;
                        self.bad_frames = 0;
                        break;
                    }
                }
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
                    if good > 0 {
                        self.bad_frames = 0;
                    }
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
            let mut total = 0;
            for r in 0..2 {
                let word = if (r == 0) == self.even_is_i {
                    UW_I
                } else {
                    UW_Q
                };
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

    fn decode_body(&mut self) -> CFrame {
        let mut plain = Vec::with_capacity(BODY_BITS);
        for block in self.body.chunks_exact(BLOCK) {
            plain.extend(fec::deinterleave(block, 4));
        }
        let mut soft = Vec::with_capacity(CODED * 4 / 3);
        fec::depuncture_2_3(&plain[..CODED], &mut soft);
        let decoded = self.viterbi.decode_block(&soft);
        let (channel_errors, channel_bits) = fec::channel_errors(&decoded, &soft, true);
        let mut info = decoded[..INFO_BITS].to_vec();
        fec::scramble(&mut info);

        let mut voice = [[0u8; VOICE_BYTES]; VOICE_FRAMES];
        let mut sub = Vec::with_capacity(288);
        for (y, v) in voice.iter_mut().enumerate() {
            let start = 1 + y * 109;
            v.copy_from_slice(&fec::bits_to_bytes(&info[start..start + 96]));
            if y < 24 {
                sub.extend_from_slice(&info[start + 96..start + 108]);
            }
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
        CFrame {
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
    fn gauss(seed: &mut u32) -> f32 {
        (0..12)
            .map(|_| (lcg(seed) >> 8) as f32 / 16_777_216.0)
            .sum::<f32>()
            - 6.0
    }

    type Sent = ([[u8; VOICE_BYTES]; VOICE_FRAMES], [[u8; 10]; 3]);

    fn frames(n: usize, seed: &mut u32) -> (Vec<u8>, Vec<Sent>) {
        let mut bits = Vec::new();
        let mut sent = Vec::new();
        for _ in 0..n {
            let mut voice = [[0u8; VOICE_BYTES]; VOICE_FRAMES];
            for b in voice.iter_mut().flatten() {
                *b = (lcg(seed) >> 24) as u8;
            }
            let mut units = [[0u8; 10]; 3];
            for b in units.iter_mut().flatten() {
                *b = (lcg(seed) >> 24) as u8;
            }
            bits.extend(encode(&voice, &units));
            sent.push((voice, units));
        }
        (bits, sent)
    }

    #[test]
    fn frame_is_half_a_second() {
        let mut seed = 1;
        let (bits, _) = frames(1, &mut seed);
        assert_eq!(bits.len(), 4200);
        assert_eq!(CODED, 4080);
    }

    /// Clean frames, with each rail turned over in turn and a late start.
    #[test]
    fn clean_frames_decode_whatever_the_rail_signs() {
        for (flip_even, flip_odd, skip) in [
            (false, false, 0),
            (true, false, 1001),
            (false, true, 1000),
            (true, true, 77),
        ] {
            let mut seed = 3;
            let (bits, sent) = frames(5, &mut seed);
            let soft: Vec<f32> = bits
                .iter()
                .enumerate()
                .skip(skip)
                .map(|(i, &b)| {
                    let flip = if i % 2 == 0 { flip_even } else { flip_odd };
                    (2.0 * f32::from(b) - 1.0) * if flip { -1.0 } else { 1.0 }
                })
                .collect();
            let mut dec = CDecoder::new();
            let mut out = Vec::new();
            for chunk in soft.chunks(333) {
                dec.push(chunk, &mut out);
            }
            let first = usize::from(skip > 0);
            assert_eq!(out.len(), 5 - first);
            for (f, (voice, units)) in out.iter().zip(&sent[first..]) {
                assert_eq!(&f.voice, voice);
                assert_eq!(f.channel_errors, 0);
                for (u, want) in f.units.iter().zip(units) {
                    assert!(u.crc_ok);
                    assert_eq!(&u.bytes[..10], want);
                }
            }
        }
    }

    /// The rate 2/3 code is weaker than rate 1/2: check where it holds.
    #[test]
    fn noisy_frames_hold_at_5_db() {
        let mut seed = 11;
        let (bits, sent) = frames(40, &mut seed);
        // Rate 2/3: Es/N0 is 1.76 dB under Eb/N0.
        let esn0 = 10f32.powf((5.0 - 1.76) / 10.0);
        let sigma = (1.0 / (2.0 * esn0)).sqrt();
        let soft: Vec<f32> = bits
            .iter()
            .map(|&b| (2.0 * f32::from(b) - 1.0) + sigma * gauss(&mut seed))
            .collect();
        let mut dec = CDecoder::new();
        let mut out = Vec::new();
        dec.push(&soft, &mut out);
        assert_eq!(out.len(), 40);
        let wrong_voice: usize = out
            .iter()
            .zip(&sent)
            .map(|(f, (voice, _))| f.voice.iter().zip(voice).filter(|(a, b)| a != b).count())
            .sum();
        let st = dec.stats();
        assert!(
            wrong_voice < 20,
            "{wrong_voice} of 1000 voice frames wrong, {st:?}"
        );
        assert!(st.crc_good >= 116, "{st:?}");
        assert_eq!(st.lock_losses, 0);
    }
}
