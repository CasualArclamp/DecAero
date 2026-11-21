// made by CasualArclamp
//! The Aero channel code: rate 1/2, constraint length 7 convolutional code,
//! decoded with a soft-decision Viterbi decoder; the scrambler; the block
//! interleaver; the 16-bit CRC.
//!
//! Layouts follow JAERO's `aerol.cpp` / `aerol.h` (MIT, (C) Jonathan Olds),
//! which uses libcorrect's convention for the code: the newest bit is the
//! least significant bit of the shift register and the two generators are
//! 109 and 79 (decimal), which are 133 and 171 octal read the other way.
//!
//! Soft bits are `f32`: positive for a 1, negative for a 0, 0.0 for "not
//! received" (a punctured bit).

/// The generators, newest bit in bit 0 (JAERO: `polys` 109, 79).
pub const POLY: [u8; 2] = [109, 79];
const STATES: usize = 64;
/// How far back the decoder looks before it commits to a bit.
const TRACEBACK: usize = 96;

/// The convolutional encoder (for the test modulator and for counting the
/// channel errors the decoder put right).
#[derive(Default, Clone)]
pub struct Encoder {
    sr: u8,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// One information bit in, two coded bits out.
    pub fn push(&mut self, bit: u8) -> [u8; 2] {
        self.sr = ((self.sr << 1) | (bit & 1)) & 0x7f;
        [
            (self.sr & POLY[0]).count_ones() as u8 & 1,
            (self.sr & POLY[1]).count_ones() as u8 & 1,
        ]
    }

    pub fn encode(&mut self, bits: &[u8]) -> Vec<u8> {
        bits.iter().flat_map(|&b| self.push(b)).collect()
    }
}

/// A streaming Viterbi decoder. Feed pairs of soft bits; decoded bits come
/// out `TRACEBACK` steps late (use [`Viterbi::delay`] to line frames up).
pub struct Viterbi {
    metric: [f32; STATES],
    /// One word per step: bit `s` says which of state `s`'s two predecessors won.
    history: Vec<u64>,
    /// Expected outputs for (state, input bit), as +1/-1 pairs.
    expect: [[[f32; 2]; 2]; STATES],
}

impl Default for Viterbi {
    fn default() -> Self {
        Self::new()
    }
}

impl Viterbi {
    pub fn new() -> Self {
        let mut expect = [[[0.0; 2]; 2]; STATES];
        for (s, e) in expect.iter_mut().enumerate() {
            for (b, out) in e.iter_mut().enumerate() {
                let full = ((s << 1) | b) as u8 & 0x7f;
                for (o, p) in out.iter_mut().zip(POLY) {
                    *o = if (full & p).count_ones() & 1 == 1 {
                        1.0
                    } else {
                        -1.0
                    };
                }
            }
        }
        Self {
            metric: [0.0; STATES],
            history: Vec::with_capacity(4 * TRACEBACK),
            expect,
        }
    }

    /// Bits of delay between a pair going in and its bit coming out.
    pub const fn delay() -> usize {
        TRACEBACK
    }

    pub fn reset(&mut self) {
        self.metric = [0.0; STATES];
        self.history.clear();
    }

    /// Start a block whose encoder began in the all-zero state.
    pub fn reset_to_zero_state(&mut self) {
        self.reset();
        self.metric = [-1e9; STATES];
        self.metric[0] = 0.0;
    }

    fn step(&mut self, a: f32, b: f32) {
        let mut next = [0f32; STATES];
        let mut decisions = 0u64;
        for (ns, n) in next.iter_mut().enumerate() {
            let bit = ns & 1;
            let p0 = ns >> 1;
            let p1 = p0 | 32;
            let e0 = self.expect[p0][bit];
            let e1 = self.expect[p1][bit];
            let m0 = self.metric[p0] + a * e0[0] + b * e0[1];
            let m1 = self.metric[p1] + a * e1[0] + b * e1[1];
            if m1 > m0 {
                *n = m1;
                decisions |= 1 << ns;
            } else {
                *n = m0;
            }
        }
        // Keep the metrics from growing without bound.
        let top = next.iter().copied().fold(f32::MIN, f32::max);
        for n in &mut next {
            *n -= top;
        }
        self.metric = next;
        self.history.push(decisions);
    }

    fn best_state(&self) -> usize {
        (0..STATES)
            .max_by(|&a, &b| self.metric[a].total_cmp(&self.metric[b]))
            .unwrap_or(0)
    }

    /// The bit decided `back` steps before the newest one.
    fn trace(&self, back: usize) -> u8 {
        let mut s = self.best_state();
        let n = self.history.len();
        for i in 0..back {
            let d = (self.history[n - 1 - i] >> s) & 1;
            s = (s >> 1) | ((d as usize) << 5);
        }
        (s & 1) as u8
    }

    /// Continuous decoding: `soft` holds whole pairs; decoded bits are
    /// appended to `out`, each `TRACEBACK` steps after its pair.
    pub fn push(&mut self, soft: &[f32], out: &mut Vec<u8>) {
        for pair in soft.chunks_exact(2) {
            self.step(pair[0], pair[1]);
            if self.history.len() > TRACEBACK {
                out.push(self.trace(TRACEBACK));
            }
            if self.history.len() >= 4 * TRACEBACK {
                self.history.drain(..2 * TRACEBACK);
            }
        }
    }

    /// Decode a whole block at once (bursts): every bit, traced back from the
    /// best final state.
    pub fn decode_block(&mut self, soft: &[f32]) -> Vec<u8> {
        self.reset_to_zero_state();
        self.history.reserve(soft.len() / 2);
        for pair in soft.chunks_exact(2) {
            self.step(pair[0], pair[1]);
        }
        let n = self.history.len();
        let mut out = vec![0u8; n];
        let mut s = self.best_state();
        for i in (0..n).rev() {
            out[i] = (s & 1) as u8;
            let d = (self.history[i] >> s) & 1;
            s = (s >> 1) | ((d as usize) << 5);
        }
        self.history.clear();
        out
    }
}

impl Viterbi {
    /// Decode a block whose encoder was already running when it began (any
    /// start state) and that ends flushed to the all-zero state: every bit,
    /// traced back from state 0.
    pub fn decode_terminated(&mut self, soft: &[f32]) -> Vec<u8> {
        self.reset();
        self.history.reserve(soft.len() / 2);
        for pair in soft.chunks_exact(2) {
            self.step(pair[0], pair[1]);
        }
        let n = self.history.len();
        let mut out = vec![0u8; n];
        let mut s = 0usize;
        for i in (0..n).rev() {
            out[i] = (s & 1) as u8;
            let d = (self.history[i] >> s) & 1;
            s = (s >> 1) | ((d as usize) << 5);
        }
        self.history.clear();
        out
    }

    /// Decode a piece cut out of a continuous stream: any start state, and
    /// traced back from the best final state. The last few dozen bits are
    /// the least sure.
    pub fn decode_free(&mut self, soft: &[f32]) -> Vec<u8> {
        self.reset();
        self.history.reserve(soft.len() / 2);
        for pair in soft.chunks_exact(2) {
            self.step(pair[0], pair[1]);
        }
        let n = self.history.len();
        let mut out = vec![0u8; n];
        let mut s = self.best_state();
        for i in (0..n).rev() {
            out[i] = (s & 1) as u8;
            let d = (self.history[i] >> s) & 1;
            s = (s >> 1) | ((d as usize) << 5);
        }
        self.history.clear();
        out
    }
}

/// Put back the bits a rate 2/3 punctured code left out: after every three
/// received bits one "not received" (JAERO `depunture_soft_block`, pattern 4).
pub fn depuncture_2_3(soft: &[f32], out: &mut Vec<f32>) {
    for (i, &s) in soft.iter().enumerate() {
        out.push(s);
        if i % 3 == 2 {
            out.push(0.0);
        }
    }
}

/// The sending side of [`depuncture_2_3`]: drop every fourth coded bit.
pub fn puncture_2_3(coded: &[u8]) -> Vec<u8> {
    coded
        .iter()
        .enumerate()
        .filter(|(i, _)| i % 4 != 3)
        .map(|(_, &b)| b)
        .collect()
}

/// Channel bit errors the decoder put right: re-encode `decoded` and compare
/// with the signs of the soft bits that were received. Returns (errors, bits
/// compared). This is where "pre-FEC BER" and "FEC load" come from.
pub fn channel_errors(decoded: &[u8], soft: &[f32], start_state_zero: bool) -> (u32, u32) {
    let mut enc = Encoder::new();
    let (mut errors, mut total) = (0, 0);
    for (i, &bit) in decoded.iter().enumerate() {
        let coded = enc.push(bit);
        // A continuous stream starts mid-flow: skip until the register is full.
        if !start_state_zero && i < 6 {
            continue;
        }
        for (k, c) in coded.into_iter().enumerate() {
            let Some(&s) = soft.get(2 * i + k) else {
                continue;
            };
            if s != 0.0 {
                total += 1;
                errors += u32::from((s > 0.0) != (c == 1));
            }
        }
    }
    (errors, total)
}

/// The scrambler sequence (JAERO `AeroLScrambler`): a 15-stage register
/// started at 110100101011001, feedback from stages 1 and 15. It restarts at
/// every frame (and every burst), so it is a plain table.
pub fn scrambler_sequence(len: usize) -> Vec<u8> {
    let mut state = [1u8, 1, 0, 1, 0, 0, 1, 0, 1, 0, 1, 1, 0, 0, 1];
    (0..len)
        .map(|_| {
            let v = state[0] ^ state[14];
            state.rotate_right(1);
            state[0] = v;
            v
        })
        .collect()
}

/// Scramble or descramble (the same thing) from the start of a frame.
pub fn scramble(bits: &mut [u8]) {
    let sequence = scrambler_sequence(bits.len());
    for (b, s) in bits.iter_mut().zip(sequence) {
        *b ^= s;
    }
}

/// Rows in every Aero interleaver block.
pub const ROWS: usize = 64;

/// Row `i` of the block is sent in position `(27 i) mod 64`.
fn row_of(i: usize) -> usize {
    (i * 27) % ROWS
}

/// Interleave one block of 64 x `cols` coded bits (JAERO `interleave`): the
/// coded bits fill the block a column at a time and leave it a row at a
/// time, rows in permuted order.
pub fn interleave<T: Copy + Default>(block: &[T], cols: usize) -> Vec<T> {
    assert_eq!(block.len(), ROWS * cols);
    let mut out = vec![T::default(); block.len()];
    for i in 0..ROWS {
        for j in 0..cols {
            out[row_of(i) * cols + j] = block[j * ROWS + i];
        }
    }
    out
}

/// Undo [`interleave`] (JAERO `deinterleave`).
pub fn deinterleave<T: Copy + Default>(block: &[T], cols: usize) -> Vec<T> {
    assert_eq!(block.len(), ROWS * cols);
    let mut out = vec![T::default(); block.len()];
    for j in 0..cols {
        for i in 0..ROWS {
            out[j * ROWS + i] = block[row_of(i) * cols + j];
        }
    }
    out
}

/// The signal-unit check: CRC-16 with polynomial x^16 + x^12 + x^5 + 1, bits
/// taken least significant first, preset to ones and inverted at the end
/// (JAERO `AeroLcrc16::calcusingbytes`; also known as CRC-16/X-25). It is
/// sent low byte first.
pub fn crc16(bytes: &[u8]) -> u16 {
    let mut crc: u16 = 0xffff;
    for &byte in bytes {
        let mut b = byte;
        for _ in 0..8 {
            let mix = (crc ^ u16::from(b)) & 1;
            crc >>= 1;
            if mix == 1 {
                crc ^= 0x8408;
            }
            b >>= 1;
        }
    }
    !crc
}

/// True when the last two bytes of `unit` are the CRC of the rest.
pub fn crc_ok(unit: &[u8]) -> bool {
    let n = unit.len();
    n > 2 && crc16(&unit[..n - 2]) == u16::from_le_bytes([unit[n - 2], unit[n - 1]])
}

/// Append the CRC to a signal unit's payload.
pub fn with_crc(payload: &[u8]) -> Vec<u8> {
    let mut v = payload.to_vec();
    v.extend_from_slice(&crc16(payload).to_le_bytes());
    v
}

/// Bytes to bits, least significant bit first (how Aero sends octets).
pub fn bytes_to_bits(bytes: &[u8]) -> Vec<u8> {
    bytes
        .iter()
        .flat_map(|&b| (0..8).map(move |i| (b >> i) & 1))
        .collect()
}

/// Bits (least significant first) to bytes; a trailing part byte is dropped.
pub fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
    bits.chunks_exact(8)
        .map(|c| {
            c.iter()
                .enumerate()
                .fold(0, |acc, (i, &b)| acc | ((b & 1) << i))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(seed: &mut u32) -> f32 {
        // Sum of uniforms: close enough to Gaussian for a decoder test.
        let mut s = 0.0;
        for _ in 0..12 {
            *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            s += (*seed >> 8) as f32 / 16_777_216.0;
        }
        s - 6.0
    }

    fn random_bits(n: usize, seed: &mut u32) -> Vec<u8> {
        (0..n)
            .map(|_| {
                *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (*seed >> 31) as u8
            })
            .collect()
    }

    #[test]
    fn crc_matches_x25_check_value() {
        assert_eq!(crc16(b"123456789"), 0x906e);
        assert!(crc_ok(&with_crc(&[0x71, 1, 2, 3, 4, 5, 6, 7, 8, 9])));
    }

    #[test]
    fn interleaver_round_trip_and_spread() {
        for cols in [3, 4, 5, 6, 9, 78] {
            let block: Vec<u16> = (0..(ROWS * cols) as u16).collect();
            let sent = interleave(&block, cols);
            assert_eq!(deinterleave(&sent, cols), block);
            // Two neighbours on the air must be far apart in the code.
            let gap = (i32::from(sent[0]) - i32::from(sent[cols])).unsigned_abs();
            assert!(gap > 1);
        }
    }

    #[test]
    fn scrambler_is_its_own_inverse_and_starts_as_jaero() {
        // First outputs: s[0]^s[14] with the start state 110100101011001.
        assert_eq!(scrambler_sequence(4)[0], 0);
        let mut seed = 5;
        let bits = random_bits(576, &mut seed);
        let mut x = bits.clone();
        scramble(&mut x);
        assert_ne!(x, bits);
        scramble(&mut x);
        assert_eq!(x, bits);
    }

    #[test]
    fn viterbi_block_decodes_at_low_snr() {
        let mut seed = 1;
        let bits = random_bits(2000, &mut seed);
        let coded = Encoder::new().encode(&bits);
        // Eb/N0 = 3 dB at rate 1/2: Es/N0 = 0 dB, sigma^2 = 0.5.
        let sigma = (0.5f32).sqrt();
        let soft: Vec<f32> = coded
            .iter()
            .map(|&c| (2.0 * f32::from(c) - 1.0) + sigma * noise(&mut seed))
            .collect();
        let raw = soft
            .iter()
            .zip(&coded)
            .filter(|(s, c)| (**s > 0.0) != (**c == 1))
            .count();
        assert!(raw > 200, "the channel must be making errors ({raw})");
        let out = Viterbi::new().decode_block(&soft);
        let wrong = out[..1990]
            .iter()
            .zip(&bits)
            .filter(|(a, b)| a != b)
            .count();
        assert!(wrong <= 4, "{wrong} errors after decoding, {raw} before");
        let (errs, total) = channel_errors(&out, &soft, true);
        assert_eq!(total, 4000);
        assert!((errs as i32 - raw as i32).abs() < 40);
    }

    #[test]
    fn viterbi_streaming_matches_with_delay() {
        let mut seed = 7;
        let bits = random_bits(1500, &mut seed);
        let soft: Vec<f32> = Encoder::new()
            .encode(&bits)
            .iter()
            .map(|&c| 2.0 * f32::from(c) - 1.0)
            .collect();
        let mut v = Viterbi::new();
        let mut out = Vec::new();
        for chunk in soft.chunks(74) {
            v.push(chunk, &mut out);
        }
        assert_eq!(out.len(), 1500 - Viterbi::delay());
        assert_eq!(out[..], bits[..out.len()]);
    }

    #[test]
    fn punctured_code_decodes() {
        let mut seed = 3;
        let bits = random_bits(1200, &mut seed);
        let sent = puncture_2_3(&Encoder::new().encode(&bits));
        assert_eq!(sent.len(), 1800);
        let soft: Vec<f32> = sent.iter().map(|&c| 2.0 * f32::from(c) - 1.0).collect();
        let mut full = Vec::new();
        depuncture_2_3(&soft, &mut full);
        assert_eq!(full.len(), 2400);
        let out = Viterbi::new().decode_block(&full);
        assert_eq!(out[..1180], bits[..1180]);
    }

    #[test]
    fn bit_packing_is_lsb_first() {
        assert_eq!(
            bytes_to_bits(&[0x01, 0x80]),
            [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]
        );
        assert_eq!(
            bits_to_bytes(&bytes_to_bits(&[0x12, 0xfe, 0x71])),
            [0x12, 0xfe, 0x71]
        );
    }
}
