// made by CasualArclamp
//! L-band correction carriers: what of them is in the clear.
//!
//! Next to the Aero channels on the Inmarsat satellite at 143.5 E sit
//! continuous BPSK carriers that are not Aero. They look like the GNSS
//! correction services that rent Inmarsat capacity (Trimble RTX, OmniSTAR,
//! Fugro, StarFire and the like): OmniSTAR gives "2400 baud" as 4876
//! symbols a second, which puts these 2438.095 and 1219.048 symbol/s
//! carriers in its "1200" and "600" classes. Nothing about their frame is
//! published. Everything below was worked out from Rory's recording
//! `areo phone assignmeant.wav` (2026-11-21); the notes and scripts are in
//! `research_notes/L-band 2438 baud carriers`.
//!
//! The payload is a subscription service and is encrypted (it is white
//! after the decoder: a Berlekamp-Massey run gives half the length), so
//! this module goes as far as the clear layer:
//!
//! * symbol rate 51200/21 or 25600/21 per second, BPSK, root raised cosine
//!   (roll-off about 0.4: the matched filter's Es/N0 peaks between 0.35
//!   and 0.5);
//! * a frame every 8192 symbols (3.36 s or 6.72 s);
//! * the frame opens with a 64-symbol unique word, `C2B5D127` sent twice,
//!   uncoded;
//! * from the unique word's last symbol on, 8128 symbols of the K=7 rate
//!   1/2 code (generators 133 and 171 octal, the Aero code's own pair and
//!   order), the encoder carried over from the frame before and flushed to
//!   zero at the end: 4064 bits;
//! * of those, 16 bits that are the same in every frame of a carrier and
//!   differ between carriers (the service or beam ID, so taken), then 7 bits
//!   that are all ones on the 2438 carriers and all zeros on the 1219 one,
//!   then the encrypted payload, then at least 8 zero tail bits.
//!
//! Another carrier there, 1200 symbol/s BPSK (also RRC) with the same code
//! but no fixed frame (packets with runs of zeros between them), is only
//! checked for code lock: [`StreamCheck`].

use crate::fec::{self, Viterbi};

/// Symbols from one unique word to the next.
pub const FRAME_SYMBOLS: usize = 8192;
/// The unique word, first symbol in the top bit (bit 1 = positive soft value).
pub const UW: u64 = 0xC2B5_D127_C2B5_D127;
pub const UW_SYMBOLS: usize = 64;
/// The coded block starts on the unique word's last symbol (measured: no
/// channel errors from there, 12 % one symbol either side).
pub const CODED_START: usize = 63;
pub const CODED_SYMBOLS: usize = 8128;
/// Decoded bits a frame: the ID, the 7-bit field, the payload, the tail.
pub const FRAME_BITS: usize = CODED_SYMBOLS / 2;
/// Where the payload starts and the tail begins, in decoded bits.
pub const PAYLOAD_START: usize = 23;
pub const TAIL_START: usize = FRAME_BITS - 8;

/// The symbol rates of the framed carriers.
pub const FRAMED_RATES: [f64; 2] = [51_200.0 / 21.0, 25_600.0 / 21.0];
/// Every rate the correction search looks for (the 1200 is unframed).
pub const ALL_RATES: [f64; 3] = [51_200.0 / 21.0, 25_600.0 / 21.0, 1200.0];
/// Pulse roll-off for the demodulator's root raised cosine. Tried 0 (an
/// unfiltered pulse) to 1.0 on `areo phone assignmeant.wav`: the measured
/// Es/N0 is best from 0.35 to 0.5 on all three rates (by 1.7 dB over an
/// unfiltered pulse on the 1200 carriers, whose constellation then split
/// each point in three; by 0.8 dB over 0.8 on the 2438 ones).
pub fn roll_off(_symbol_rate: f64) -> f32 {
    0.4
}
/// Whether a symbol rate is one of the framed kind.
pub fn framed(symbol_rate: f64) -> bool {
    FRAMED_RATES.iter().any(|r| (r - symbol_rate).abs() < 5.0)
}

/// Normalised soft correlation that finds a unique word (noise alone has a
/// spread of about 0.16; the word repeats after 32 symbols, which gives 0.5
/// there, so the best place near a crossing is taken).
const FIND: f32 = 0.7;
/// Correlation at the expected place that keeps lock.
const KEEP: f32 = 0.3;
/// Frames decoded from the expected place without seeing the word before
/// lock is given up.
const MISSES: u32 = 3;
/// A first frame worse than this is a false find.
const FALSE_LOCK_BER: f32 = 0.15;

/// One decoded frame.
#[derive(Debug, Clone)]
pub struct Frame {
    /// The 16 bits that name the carrier.
    pub id: u16,
    /// The 7 bits after it (0x7F on the 2438 carriers, 0 on the 1219).
    pub field: u8,
    /// All [`FRAME_BITS`] decoded bits.
    pub bits: Vec<u8>,
    /// Channel bits the decoder put right, and channel bits compared.
    pub channel_errors: u32,
    pub channel_bits: u32,
    /// Unique word symbols received wrong (64 compared).
    pub uw_errors: u32,
    /// The carrier phase came out upside down (the word was inverted).
    pub inverted: bool,
    /// Decoded where the frame should be, though the word was not seen there.
    pub flywheel: bool,
}

impl Frame {
    /// The payload as bytes, from bit 23 (`n` bytes at most).
    pub fn payload_head(&self, n: usize) -> Vec<u8> {
        let end = (PAYLOAD_START + 8 * n).min(TAIL_START);
        fec::bits_to_bytes(&self.bits[PAYLOAD_START..end])
    }
    /// The tail bits are all zero (the encoder's flush; a sanity check).
    pub fn tail_clear(&self) -> bool {
        self.bits[TAIL_START..].iter().all(|&b| b == 0)
    }
    pub fn ber(&self) -> f32 {
        self.channel_errors as f32 / self.channel_bits.max(1) as f32
    }
}

/// Finds the frames in a stream of soft symbols (positive = 1) and decodes them.
pub struct FrameDecoder {
    /// Soft symbols kept; `buf[0]` is absolute symbol `base`.
    buf: Vec<f32>,
    base: u64,
    /// Absolute position of the next unique word, once locked.
    next: Option<u64>,
    missed: u32,
    inverted: bool,
    /// Frames since the last lock was taken.
    since_lock: u64,
    uw: [f32; UW_SYMBOLS],
    viterbi: Viterbi,
    pub frames: u64,
    pub uw_wrong: u64,
    pub uw_bits: u64,
    pub lock_losses: u64,
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            base: 0,
            next: None,
            missed: 0,
            inverted: false,
            since_lock: 0,
            uw: std::array::from_fn(|i| if (UW >> (63 - i)) & 1 == 1 { 1.0 } else { -1.0 }),
            viterbi: Viterbi::new(),
            frames: 0,
            uw_wrong: 0,
            uw_bits: 0,
            lock_losses: 0,
        }
    }

    /// Holding frame lock (the unique word seen at its place lately).
    pub fn locked(&self) -> bool {
        self.next.is_some() && self.missed == 0
    }

    /// Normalised correlation with the unique word at `buf[i]`, signed.
    fn correlate(&self, i: usize) -> f32 {
        let w = &self.buf[i..i + UW_SYMBOLS];
        let dot: f32 = w.iter().zip(&self.uw).map(|(s, u)| s * u).sum();
        let mag: f32 = w.iter().map(|s| s.abs()).sum();
        if mag > 0.0 { dot / mag } else { 0.0 }
    }

    /// Soft symbols in; frames that are now complete appended to `out`.
    pub fn push(&mut self, soft: &[f32], out: &mut Vec<Frame>) {
        self.buf.extend_from_slice(soft);
        loop {
            match self.next {
                None => {
                    if !self.search() {
                        return;
                    }
                }
                Some(p) => {
                    // The frame and two symbols of slack either side.
                    let end = p + (CODED_START + CODED_SYMBOLS) as u64 + 2;
                    if end > self.base + self.buf.len() as u64 {
                        return;
                    }
                    self.frame_at(p, out);
                }
            }
        }
    }

    /// Look for a unique word in what is kept. True if one was found.
    fn search(&mut self) -> bool {
        let len = self.buf.len();
        if len < UW_SYMBOLS + 40 {
            return false;
        }
        let last = len - UW_SYMBOLS - 40;
        let hit = (0..=last).find(|&i| self.correlate(i).abs() >= FIND);
        let Some(i) = hit else {
            // Keep the tail that could still start a word.
            let cut = last + 1;
            self.buf.drain(..cut);
            self.base += cut as u64;
            return false;
        };
        // The best place just after the crossing (the word repeats after 32).
        let best = (i..i + 40)
            .max_by(|&a, &b| self.correlate(a).abs().total_cmp(&self.correlate(b).abs()))
            .unwrap_or(i);
        self.buf.drain(..best);
        self.base += best as u64;
        self.next = Some(self.base);
        self.missed = 0;
        self.since_lock = 0;
        true
    }

    fn frame_at(&mut self, p: u64, out: &mut Vec<Frame>) {
        // The word may have slipped a symbol or two: take the best place.
        let rel = (p - self.base) as usize;
        let (best, c) = (rel.saturating_sub(2)..=rel + 2)
            .map(|i| (i, self.correlate(i)))
            .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
            .unwrap_or((rel, 0.0));
        let seen = c.abs() >= KEEP;
        let at = if seen {
            self.missed = 0;
            self.inverted = c < 0.0;
            best
        } else {
            self.missed += 1;
            rel
        };
        if self.missed > MISSES {
            self.lose_lock(at);
            return;
        }
        let sign = if self.inverted { -1.0 } else { 1.0 };
        let uw_errors = self.buf[at..at + UW_SYMBOLS]
            .iter()
            .zip(&self.uw)
            .filter(|&(s, u)| s * sign * u < 0.0)
            .count() as u32;
        let coded: Vec<f32> = self.buf[at + CODED_START..at + CODED_START + CODED_SYMBOLS]
            .iter()
            .map(|s| s * sign)
            .collect();
        let bits = self.viterbi.decode_terminated(&coded);
        let (channel_errors, channel_bits) = fec::channel_errors(&bits, &coded, false);
        let frame = Frame {
            id: bits[..16]
                .iter()
                .fold(0u16, |a, &b| (a << 1) | u16::from(b)),
            field: bits[16..PAYLOAD_START]
                .iter()
                .fold(0u8, |a, &b| (a << 1) | b),
            bits,
            channel_errors,
            channel_bits,
            uw_errors,
            inverted: self.inverted,
            flywheel: !seen,
        };
        if self.since_lock == 0 && frame.ber() > FALSE_LOCK_BER {
            // Not a frame after all: look again past this place.
            self.next = None;
            self.buf.drain(..at + 1);
            self.base += at as u64 + 1;
            return;
        }
        self.since_lock += 1;
        self.frames += 1;
        self.uw_wrong += u64::from(uw_errors);
        self.uw_bits += UW_SYMBOLS as u64;
        out.push(frame);
        let next = self.base + (at + FRAME_SYMBOLS) as u64;
        self.next = Some(next);
        // Keep from two symbols before the next word.
        let cut = at + FRAME_SYMBOLS - 2;
        self.buf.drain(..cut);
        self.base += cut as u64;
    }

    fn lose_lock(&mut self, at: usize) {
        self.next = None;
        self.missed = 0;
        self.lock_losses += 1;
        self.buf.drain(..at);
        self.base += at as u64;
    }
}

/// Code lock on a carrier with the K=7 code but no frame known: pieces of
/// the stream are decoded in all four ways the pairs could be cut and
/// ordered, and the best tells the channel error rate.
pub struct StreamCheck {
    pending: Vec<f32>,
    viterbi: Viterbi,
    /// The cut and order that fitted last (0 to 3), kept while it still fits.
    way: usize,
}

/// Symbols in a piece, and decoded bits at each end not counted (the
/// decoder is unsure there).
const PIECE: usize = 2400;
const EDGE: usize = 24;

/// One piece's result.
#[derive(Debug, Clone, Copy)]
pub struct StreamPiece {
    pub channel_errors: u32,
    pub channel_bits: u32,
}

impl Default for StreamCheck {
    fn default() -> Self {
        Self::new()
    }
}

impl StreamCheck {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            viterbi: Viterbi::new(),
            way: 0,
        }
    }

    fn try_way(&mut self, piece: &[f32], way: usize) -> StreamPiece {
        let start = way & 1;
        let mut soft: Vec<f32> = piece[start..start + PIECE - 2].to_vec();
        if way & 2 != 0 {
            for pair in soft.chunks_exact_mut(2) {
                pair.swap(0, 1);
            }
        }
        let bits = self.viterbi.decode_free(&soft);
        let mut enc = fec::Encoder::new();
        let (mut errors, mut total) = (0, 0);
        for (i, &b) in bits.iter().enumerate() {
            let coded = enc.push(b);
            if i < EDGE || i + EDGE >= bits.len() {
                continue;
            }
            for (k, c) in coded.into_iter().enumerate() {
                total += 1;
                errors += u32::from((soft[2 * i + k] > 0.0) != (c == 1));
            }
        }
        StreamPiece {
            channel_errors: errors,
            channel_bits: total,
        }
    }

    pub fn push(&mut self, soft: &[f32], out: &mut Vec<StreamPiece>) {
        self.pending.extend_from_slice(soft);
        while self.pending.len() >= PIECE {
            let piece: Vec<f32> = self.pending.drain(..PIECE).collect();
            let mut best = self.try_way(&piece, self.way);
            if best.channel_errors * 10 > best.channel_bits {
                let tried = self.way;
                for way in (0..4).filter(|&w| w != tried) {
                    let r = self.try_way(&piece, way);
                    if r.channel_errors < best.channel_errors {
                        best = r;
                        self.way = way;
                    }
                }
            }
            out.push(best);
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

    /// Build a stream of frames as the carriers send them: the word, then
    /// the coded block starting on the word's last symbol.
    fn stream(frames: usize, id: u16, seed: &mut u32) -> Vec<f32> {
        let mut enc = fec::Encoder::new();
        let mut symbols: Vec<u8> = Vec::new();
        for _ in 0..frames {
            let mut bits: Vec<u8> = (0..16).rev().map(|k| ((id >> k) & 1) as u8).collect();
            bits.extend([1u8; 7]);
            while bits.len() < TAIL_START {
                bits.push((lcg(seed) >> 31) as u8);
            }
            bits.extend([0u8; 8]);
            let coded = enc.encode(&bits);
            // The word's first 63 symbols; its 64th is the first coded one.
            // The symbol after the block was a 1 in every frame recorded.
            symbols.extend((0..63).map(|i| ((UW >> (63 - i)) & 1) as u8));
            symbols.extend(&coded);
            symbols.push(1);
        }
        symbols
            .into_iter()
            .map(|b| if b == 1 { 1.0 } else { -1.0 })
            .collect()
    }

    #[test]
    fn frames_come_out_with_their_id() {
        let mut seed = 3;
        // Lead-in of noise-like symbols so the word has to be found.
        let mut soft: Vec<f32> = (0..3000)
            .map(|_| if lcg(&mut seed) >> 31 == 1 { 0.7 } else { -0.7 })
            .collect();
        soft.extend(stream(4, 0xE932, &mut seed));
        let mut dec = FrameDecoder::new();
        let mut out = Vec::new();
        for chunk in soft.chunks(777) {
            dec.push(chunk, &mut out);
        }
        assert!(out.len() >= 3, "{} frames", out.len());
        for f in &out {
            assert_eq!(f.id, 0xE932);
            assert_eq!(f.field, 0x7F);
            assert!(f.tail_clear());
            assert!(!f.inverted);
        }
    }

    #[test]
    fn inverted_carrier_is_read_the_right_way_up() {
        let mut seed = 11;
        let soft: Vec<f32> = stream(3, 0x14CE, &mut seed).iter().map(|s| -s).collect();
        let mut dec = FrameDecoder::new();
        let mut out = Vec::new();
        dec.push(&soft, &mut out);
        assert!(!out.is_empty());
        assert!(out.iter().all(|f| f.id == 0x14CE && f.inverted));
    }

    #[test]
    fn stream_check_finds_the_cut() {
        let mut seed = 5;
        let bits: Vec<u8> = (0..3000).map(|_| (lcg(&mut seed) >> 31) as u8).collect();
        let coded = fec::Encoder::new().encode(&bits);
        // Start on an odd symbol, pairs swapped.
        let mut soft: Vec<f32> = Vec::new();
        for pair in coded.chunks_exact(2) {
            soft.push(if pair[1] == 1 { 1.0 } else { -1.0 });
            soft.push(if pair[0] == 1 { 1.0 } else { -1.0 });
        }
        let mut check = StreamCheck::new();
        let mut out = Vec::new();
        check.push(&soft[1..], &mut out);
        assert!(!out.is_empty());
        let last = out.last().unwrap();
        assert_eq!(last.channel_errors, 0, "{last:?}");
    }
}
