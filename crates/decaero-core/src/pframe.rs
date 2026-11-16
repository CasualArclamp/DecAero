// made by CasualArclamp
//! The P channel (ground to aircraft, continuous): frame sync, the frame
//! header, de-interleaving, decoding and the signal units, plus the encoder
//! the test modulator uses.
//!
//! On the air a frame is: unique word, 16 header bits, dummy bits (10500
//! bit/s only), then the coded and interleaved signal units. Layout from
//! JAERO `AeroL::setSettings` and `AeroL::Decode` (MIT, (C) Jonathan Olds).
//!
//! The decoder here works on soft bits (positive = 1) and is built for a weak
//! signal: it matches the unique word allowing errors, fly-wheels from one
//! frame to the next once it has lock, and gives the figures the GUI shows.

use crate::fec::{self, Encoder, Viterbi};
use std::collections::VecDeque;

/// The 32-bit unique word, first bit on the air in the top bit
/// (JAERO `preambledetector.setPreamble(3780831379, 32)`).
pub const UNIQUE_WORD: u32 = 0xE15A_E893;
/// Octets in a signal unit on the P, T and C channels (10 + CRC).
pub const SU_BYTES: usize = 12;

/// The three P channel formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PFormat {
    /// Channel rate in bit/s.
    pub rate: u32,
    /// Interleaver columns (rows are always 64).
    pub cols: usize,
    /// Interleaver blocks in a frame.
    pub blocks: usize,
    /// A-QPSK sends the unique word on both rails, so each bit comes twice.
    pub uw_doubled: bool,
    /// Bits between the header and the first interleaver block.
    pub dummy_bits: usize,
}

impl PFormat {
    pub const P600: Self = Self {
        rate: 600,
        cols: 6,
        blocks: 3,
        uw_doubled: false,
        dummy_bits: 0,
    };
    pub const P1200: Self = Self {
        rate: 1200,
        cols: 9,
        blocks: 2,
        uw_doubled: false,
        dummy_bits: 0,
    };
    pub const P10500: Self = Self {
        rate: 10500,
        cols: 78,
        blocks: 1,
        uw_doubled: true,
        dummy_bits: 178,
    };

    pub fn from_rate(rate: u32) -> Option<Self> {
        [Self::P600, Self::P1200, Self::P10500]
            .into_iter()
            .find(|f| f.rate == rate)
    }

    pub const fn uw_bits(&self) -> usize {
        if self.uw_doubled { 64 } else { 32 }
    }
    pub const fn coded_bits(&self) -> usize {
        fec::ROWS * self.cols * self.blocks
    }
    pub const fn info_bits(&self) -> usize {
        self.coded_bits() / 2
    }
    /// Signal units in a frame (6, or 26 at 10500 bit/s).
    pub const fn signal_units(&self) -> usize {
        self.info_bits() / (8 * SU_BYTES)
    }
    /// Everything after the unique word.
    pub const fn body_bits(&self) -> usize {
        16 + self.dummy_bits + self.coded_bits()
    }
    pub const fn frame_bits(&self) -> usize {
        self.uw_bits() + self.body_bits()
    }
    /// Seconds a frame lasts.
    pub fn frame_seconds(&self) -> f64 {
        self.frame_bits() as f64 / f64::from(self.rate)
    }

    /// The unique word as it appears in the serial bit stream.
    pub fn uw_pattern(&self) -> Vec<u8> {
        let word = (0..32).rev().map(|i| ((UNIQUE_WORD >> i) & 1) as u8);
        if self.uw_doubled {
            word.flat_map(|b| [b, b]).collect()
        } else {
            word.collect()
        }
    }
}

/// The 16 header bits (JAERO: `formatid`, `supfrmaker`, two frame counters).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Header {
    pub format_id: u8,
    pub superframe_marker: u8,
    pub frame_counter: u8,
    /// The counter is sent twice; false when the two copies differ.
    pub counter_agrees: bool,
}

impl Header {
    fn from_bits(word: u16) -> Self {
        let n = |shift: u32| ((word >> shift) & 0xf) as u8;
        Self {
            format_id: n(12),
            superframe_marker: n(8),
            frame_counter: n(4),
            counter_agrees: n(4) == n(0),
        }
    }
    fn to_bits(self) -> u16 {
        let c = u16::from(self.frame_counter & 0xf);
        (u16::from(self.format_id & 0xf) << 12)
            | (u16::from(self.superframe_marker & 0xf) << 8)
            | (c << 4)
            | c
    }
}

/// One signal unit as received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalUnit {
    pub bytes: [u8; SU_BYTES],
    pub crc_ok: bool,
}

/// One decoded frame with its quality figures.
#[derive(Debug, Clone, Default)]
pub struct PFrame {
    pub header: Header,
    pub units: Vec<SignalUnit>,
    /// Unique word bits that were wrong (of 32 or 64).
    pub uw_errors: u32,
    /// Channel bit errors the Viterbi decoder corrected in this frame.
    pub channel_errors: u32,
    /// Coded bits compared to count them.
    pub channel_bits: u32,
}

impl PFrame {
    pub fn crc_good(&self) -> usize {
        self.units.iter().filter(|u| u.crc_ok).count()
    }
    /// Bit error rate before decoding, as measured by re-encoding.
    pub fn pre_fec_ber(&self) -> f32 {
        if self.channel_bits == 0 {
            0.0
        } else {
            self.channel_errors as f32 / self.channel_bits as f32
        }
    }
}

/// Builds the serial bit stream of a P channel (for the test modulator).
pub struct PEncoder {
    fmt: PFormat,
    conv: Encoder,
    counter: u8,
}

impl PEncoder {
    pub fn new(fmt: PFormat) -> Self {
        Self {
            fmt,
            conv: Encoder::new(),
            counter: 0,
        }
    }

    /// One frame carrying `payloads` (10 octets each, the CRC is added);
    /// missing units are sent as fill-in units (type 0x01).
    pub fn frame(&mut self, payloads: &[[u8; SU_BYTES - 2]]) -> Vec<u8> {
        let fmt = self.fmt;
        let mut info = Vec::with_capacity(fmt.info_bits());
        for i in 0..fmt.signal_units() {
            let payload = payloads
                .get(i)
                .copied()
                .unwrap_or([0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            info.extend(fec::bytes_to_bits(&fec::with_crc(&payload)));
        }
        fec::scramble(&mut info);
        // The convolutional encoder runs on from frame to frame.
        let coded = self.conv.encode(&info);

        let mut bits = fmt.uw_pattern();
        let header = Header {
            format_id: 1,
            superframe_marker: 0,
            frame_counter: self.counter,
            counter_agrees: true,
        };
        self.counter = (self.counter + 1) & 0xf;
        bits.extend((0..16).rev().map(|i| ((header.to_bits() >> i) & 1) as u8));
        bits.extend((0..fmt.dummy_bits).map(|i| (i & 1) as u8));
        for block in coded.chunks_exact(fec::ROWS * fmt.cols) {
            bits.extend(fec::interleave(block, fmt.cols));
        }
        debug_assert_eq!(bits.len(), fmt.frame_bits());
        bits
    }
}

/// Unique-word errors allowed when looking for a signal: about one false
/// start per several frames of noise, each of which is thrown out by the
/// confirmation below.
const fn acquire_tolerance(uw_bits: usize) -> u32 {
    (uw_bits as u32 * 5) / 32
}

/// Frames with no good CRC and a poor unique word before lock is given up.
const LOSE_AFTER: u32 = 3;

/// Running totals for the GUI.
#[derive(Debug, Clone, Copy, Default)]
pub struct PStats {
    pub frames: u64,
    pub uw_bits_wrong: u64,
    pub uw_bits: u64,
    pub crc_good: u64,
    pub crc_bad: u64,
    pub channel_errors: u64,
    pub channel_bits: u64,
    pub lock_losses: u64,
}

struct Pending {
    header: Header,
    uw_errors: u32,
}

/// The P channel decoder: soft bits in, frames out.
pub struct PDecoder {
    fmt: PFormat,
    uw: u64,
    uw_mask: u64,
    /// The last 64 hard decisions.
    recent: u64,
    locked: bool,
    confirmed: bool,
    /// Per rail (bit parity): true when that rail comes out upside down.
    /// A-BPSK has one rail, so both entries agree.
    inverted: [bool; 2],
    /// Bits taken in so far (its low bit is the rail of the newest bit).
    count: u64,
    /// Soft bits of the frame being collected (after the unique word).
    body: Vec<f32>,
    /// Bits of the next unique word seen so far while locked.
    uw_seen: usize,
    bad_frames: u32,
    viterbi: Viterbi,
    reencoder: Encoder,
    /// Bits re-encoded since acquisition.
    reencoded: u32,
    /// De-interleaved soft bits not yet matched with decoded bits.
    soft: VecDeque<f32>,
    decoded: Vec<u8>,
    pending: VecDeque<Pending>,
    /// Errors in the unique word that opened the frame being collected.
    pending_uw_errors: u32,
    /// The body of the current frame has been decoded; its closing unique word is arriving.
    body_done: bool,
    /// Good CRCs among the frames the last body completed.
    last_good: usize,
    stats: PStats,
}

impl PDecoder {
    pub fn new(fmt: PFormat) -> Self {
        let pattern = fmt.uw_pattern();
        let uw = pattern
            .iter()
            .fold(0u64, |acc, &b| (acc << 1) | u64::from(b));
        let uw_mask = if fmt.uw_bits() == 64 {
            u64::MAX
        } else {
            (1u64 << fmt.uw_bits()) - 1
        };
        Self {
            fmt,
            uw,
            uw_mask,
            recent: 0,
            locked: false,
            confirmed: false,
            inverted: [false; 2],
            count: 0,
            body: Vec::with_capacity(fmt.body_bits()),
            uw_seen: 0,
            bad_frames: 0,
            viterbi: Viterbi::new(),
            reencoder: Encoder::new(),
            reencoded: 0,
            soft: VecDeque::new(),
            decoded: Vec::new(),
            pending: VecDeque::new(),
            pending_uw_errors: 0,
            body_done: false,
            last_good: 0,
            stats: PStats::default(),
        }
    }

    pub fn format(&self) -> PFormat {
        self.fmt
    }
    /// Frame sync held and confirmed by a second unique word or a good CRC.
    pub fn locked(&self) -> bool {
        self.locked && self.confirmed
    }
    pub fn stats(&self) -> PStats {
        self.stats
    }

    /// Unique-word bits that differ, for the rail of the newest bit and for
    /// the other rail, and how many bits each rail has. With one rail the
    /// two figures are the same.
    fn uw_mismatch(&self) -> ([u32; 2], u32) {
        let diff = (self.recent ^ self.uw) & self.uw_mask;
        if self.fmt.uw_doubled {
            let newest = 0x5555_5555_5555_5555u64;
            (
                [(diff & newest).count_ones(), (diff & !newest).count_ones()],
                32,
            )
        } else {
            ([diff.count_ones(); 2], 32)
        }
    }

    fn drop_lock(&mut self) {
        if self.locked && self.confirmed {
            self.stats.lock_losses += 1;
        }
        self.locked = false;
        self.confirmed = false;
        self.body.clear();
        self.body_done = false;
        self.uw_seen = 0;
        self.bad_frames = 0;
        self.viterbi.reset();
        self.reencoder = Encoder::new();
        self.reencoded = 0;
        self.soft.clear();
        self.decoded.clear();
        self.pending.clear();
    }

    /// Feed soft bits (positive = 1); decoded frames are appended to `out`.
    pub fn push(&mut self, soft_bits: &[f32], out: &mut Vec<PFrame>) {
        let rails = if self.fmt.uw_doubled { 2 } else { 1 };
        for &raw in soft_bits {
            self.recent = (self.recent << 1) | u64::from(raw > 0.0);
            self.count += 1;
            let rail = (self.count & 1) as usize;
            if !self.locked {
                let (miss, n) = self.uw_mismatch();
                let tol = acquire_tolerance(32);
                if miss.iter().all(|&m| m <= tol || m >= n - tol) {
                    self.locked = true;
                    self.confirmed = false;
                    self.inverted[rail] = miss[0] > tol;
                    self.inverted[1 - rail] = miss[1] > tol;
                    self.bad_frames = 0;
                    self.pending_uw_errors = miss[..rails].iter().map(|&m| m.min(n - m)).sum();
                }
                continue;
            }
            let s = if self.inverted[rail] { -raw } else { raw };
            if !self.body_done {
                self.body.push(s);
                if self.body.len() == self.fmt.body_bits() {
                    // Decode at once rather than after the next unique word:
                    // the units reach the screen a unique word sooner.
                    self.last_good = self.finish_frame(out);
                    self.body_done = true;
                }
                continue;
            }
            // The next frame's unique word is arriving.
            self.uw_seen += 1;
            if self.uw_seen < self.fmt.uw_bits() {
                continue;
            }
            self.uw_seen = 0;
            let (per_rail, n) = self.uw_mismatch();
            let mut miss = 0;
            for (k, &m) in per_rail[..rails].iter().enumerate() {
                let r = if k == 0 { rail } else { 1 - rail };
                let mut m = if self.inverted[r] { n - m } else { m };
                if m > n * 3 / 4 {
                    // The carrier slipped: carry on with this rail turned over.
                    self.inverted[r] = !self.inverted[r];
                    if rails == 1 {
                        self.inverted[1 - r] = self.inverted[r];
                    }
                    m = n - m;
                }
                miss += m;
            }
            let uw_bits = n * rails as u32;
            self.body_done = false;
            let good = self.last_good;
            let uw_fine = miss <= uw_bits / 4;
            if uw_fine || good > 0 {
                self.confirmed = true;
                self.bad_frames = 0;
            } else {
                self.bad_frames += 1;
                if !self.confirmed || self.bad_frames >= LOSE_AFTER {
                    self.drop_lock();
                    continue;
                }
            }
            self.pending_uw_errors = miss;
        }
    }

    /// Decode the collected frame body; returns how many CRCs were good in
    /// the frames that completed.
    fn finish_frame(&mut self, out: &mut Vec<PFrame>) -> usize {
        let fmt = self.fmt;
        let body = std::mem::take(&mut self.body);
        let word = body[..16]
            .iter()
            .fold(0u16, |acc, &s| (acc << 1) | u16::from(s > 0.0));
        self.pending.push_back(Pending {
            header: Header::from_bits(word),
            uw_errors: self.pending_uw_errors,
        });
        for block in body[16 + fmt.dummy_bits..].chunks_exact(fec::ROWS * fmt.cols) {
            let plain = fec::deinterleave(block, fmt.cols);
            self.viterbi.push(&plain, &mut self.decoded);
            self.soft.extend(plain);
        }
        self.body = body;
        self.body.clear();

        // The decoder answers a little late, so a frame's bits are complete
        // only part of the way into the next one.
        let mut good = 0;
        while self.decoded.len() >= fmt.info_bits() {
            let Some(p) = self.pending.pop_front() else {
                break;
            };
            let mut info: Vec<u8> = self.decoded.drain(..fmt.info_bits()).collect();
            let (mut errors, mut total) = (0, 0);
            for &bit in &info {
                // The first bits after acquisition are re-encoded from a
                // register that is not yet full: do not count them.
                let warm = self.reencoded >= 6;
                self.reencoded = self.reencoded.saturating_add(1);
                for c in self.reencoder.push(bit) {
                    if let Some(s) = self.soft.pop_front()
                        && warm
                        && s != 0.0
                    {
                        total += 1;
                        errors += u32::from((s > 0.0) != (c == 1));
                    }
                }
            }
            fec::scramble(&mut info);
            let units: Vec<SignalUnit> = fec::bits_to_bytes(&info)
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
            let frame = PFrame {
                header: p.header,
                units,
                uw_errors: p.uw_errors,
                channel_errors: errors,
                channel_bits: total,
            };
            good += frame.crc_good();
            self.stats.frames += 1;
            self.stats.uw_bits_wrong += u64::from(frame.uw_errors);
            self.stats.uw_bits += fmt.uw_bits() as u64;
            self.stats.crc_good += frame.crc_good() as u64;
            self.stats.crc_bad += (frame.units.len() - frame.crc_good()) as u64;
            self.stats.channel_errors += u64::from(errors);
            self.stats.channel_bits += u64::from(total);
            out.push(frame);
        }
        good
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
    fn payload(seed: &mut u32) -> [u8; 10] {
        let mut p = [0u8; 10];
        p[0] = 0x71;
        for b in &mut p[1..] {
            *b = (lcg(seed) >> 24) as u8;
        }
        p
    }

    /// Frames of random units through a noisy channel at `ebn0_db`, starting
    /// part-way into a frame. Returns (units sent, units decoded right, stats).
    fn run(fmt: PFormat, frames: usize, ebn0_db: f32, invert: bool) -> (usize, usize, PStats) {
        let mut seed = 42 + fmt.rate;
        let mut enc = PEncoder::new(fmt);
        let mut sent = Vec::new();
        let mut bits = Vec::new();
        for _ in 0..frames {
            let units: Vec<[u8; 10]> = (0..fmt.signal_units())
                .map(|_| payload(&mut seed))
                .collect();
            bits.extend(enc.frame(&units));
            sent.extend(units);
        }
        // Rate 1/2: Es/N0 is 3 dB under Eb/N0. Unit signal, noise sigma^2 = 1 / (2 Es/N0).
        let esn0 = 10f32.powf((ebn0_db - 3.01) / 10.0);
        let sigma = (1.0 / (2.0 * esn0)).sqrt();
        let sign = if invert { -1.0 } else { 1.0 };
        let soft: Vec<f32> = bits[301..]
            .iter()
            .map(|&b| sign * (2.0 * f32::from(b) - 1.0) + sigma * gauss(&mut seed))
            .collect();
        let mut dec = PDecoder::new(fmt);
        let mut out = Vec::new();
        for chunk in soft.chunks(997) {
            dec.push(chunk, &mut out);
        }
        let got: std::collections::HashSet<[u8; 10]> = out
            .iter()
            .flat_map(|f| &f.units)
            .filter(|u| u.crc_ok)
            .map(|u| {
                let mut p = [0u8; 10];
                p.copy_from_slice(&u.bytes[..10]);
                p
            })
            .collect();
        let right = sent.iter().filter(|p| got.contains(*p)).count();
        (sent.len(), right, dec.stats())
    }

    #[test]
    fn formats_last_as_long_as_they_should() {
        assert_eq!(PFormat::P600.frame_bits(), 1200);
        assert_eq!(PFormat::P1200.frame_bits(), 1200);
        assert_eq!(PFormat::P10500.frame_bits(), 5250);
        assert_eq!(PFormat::P600.signal_units(), 6);
        assert_eq!(PFormat::P10500.signal_units(), 26);
        assert!((PFormat::P600.frame_seconds() - 2.0).abs() < 1e-9);
        assert!((PFormat::P10500.frame_seconds() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn clean_signal_decodes_every_unit_after_sync() {
        for fmt in [PFormat::P600, PFormat::P1200, PFormat::P10500] {
            let (sent, right, stats) = run(fmt, 8, 30.0, false);
            // The first frame is lost to the late start and the last one is
            // still inside the Viterbi decoder.
            assert_eq!(right, sent - 2 * fmt.signal_units(), "{fmt:?}");
            assert_eq!(stats.crc_bad, 0);
            assert_eq!(stats.channel_errors, 0);
            assert_eq!(stats.uw_bits_wrong, 0);
        }
    }

    #[test]
    fn inverted_signal_decodes() {
        let (sent, right, _) = run(PFormat::P1200, 6, 30.0, true);
        assert_eq!(right, sent - 12);
    }

    /// The sensitivity claim in DESIGN.md: frames hold at 3 dB Eb/N0 with the
    /// channel making about 8 % bit errors.
    #[test]
    fn weak_signal_holds_at_3_db() {
        for fmt in [PFormat::P600, PFormat::P10500] {
            let frames = if fmt.rate == 600 { 60 } else { 40 };
            let (sent, right, stats) = run(fmt, frames, 3.0, false);
            let ber = stats.channel_errors as f32 / stats.channel_bits as f32;
            assert!((0.05..0.11).contains(&ber), "pre-FEC BER {ber}");
            let share = right as f32 / sent as f32;
            assert!(
                share > 0.90,
                "{fmt:?}: {right} of {sent} units at 3 dB, {stats:?}"
            );
            assert_eq!(stats.lock_losses, 0);
        }
    }

    #[test]
    fn noise_alone_never_locks() {
        let mut seed = 9;
        let mut dec = PDecoder::new(PFormat::P600);
        let mut out = Vec::new();
        let noise: Vec<f32> = (0..200_000).map(|_| gauss(&mut seed)).collect();
        dec.push(&noise, &mut out);
        assert!(!dec.locked());
        assert!(out.iter().all(|f| f.crc_good() == 0));
    }
}
