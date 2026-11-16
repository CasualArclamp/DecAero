// made by CasualArclamp
//! AMBE 4800x3600 speech decoder: the voice codec of the Inmarsat Aero
//! C channel at 8400 bit/s (20 ms frames of 96 bits, 8 kHz audio out).
//!
//! This is a Rust port of two C projects, kept close to the originals so the
//! two can be compared line by line:
//!
//! * `ambe4800x3600.c` from libaeroambe (MIT, (C) 2018 libaeroambe team):
//!   the frame layout, error protection and parameter decoding;
//! * `mbelib.c` and `ecc.c` from mbelib (ISC, (C) 2010 mbelib Author): the
//!   multi-band excitation synthesis, Golay and Hamming decoders.
//!
//! See `NOTICE` at the repository root for the licences and the patent
//! notice. Differences from the C code are marked "not in the C code".
//!
//! Rust notes: `Decoder` owns all state, so one decoder per call; arrays are
//! one element longer than in C where the C code reads one past the end.

// The loops follow the numbered equations of the C code (1-based indices
// into several arrays at once), so they stay as index loops.
#![allow(clippy::needless_range_loop, clippy::chunks_exact_to_as_chunks)]

mod tables;

use std::f32::consts::{E, PI, TAU};
use tables::*;

/// Samples in one decoded frame (20 ms at 8 kHz).
pub const FRAME_SAMPLES: usize = 160;
/// Bytes in one coded frame as the C channel carries it (96 bits).
pub const FRAME_BYTES: usize = 12;
/// Audio sample rate of the decoder output.
pub const SAMPLE_RATE: u32 = 8000;

/// Where each of the 96 received bits goes: row (`RW`) and bit (`RX`) of the
/// six code words. From libaeroambe `aeroambe.h`.
#[rustfmt::skip]
const RX: [u8; 96] = [
    23, 11, 14, 2, 5, 8, 9, 11, 22, 10, 13, 1, 4, 7, 8, 10, 21, 9, 12, 0, 3, 6, 7, 9, 20, 8, 11, 14, 2, 5, 6,
    8, 19, 7, 10, 13, 1, 4, 5, 7, 18, 6, 9, 12, 0, 3, 4, 6, 17, 5, 8, 11, 14, 2, 3, 5, 16, 4, 7, 10, 13, 1, 2,
    4, 15, 3, 6, 9, 12, 0, 1, 3, 14, 2, 5, 8, 11, 12, 0, 2, 13, 1, 4, 7, 10, 11, 13, 1, 12, 0, 3, 6, 9, 10, 12, 0,
];
#[rustfmt::skip]
const RW: [u8; 96] = [
    0, 0, 1, 1, 2, 3, 4, 5, 0, 0, 1, 1, 2, 3, 4, 5, 0, 0, 1, 1, 2, 3, 4, 5, 0, 0, 1, 2, 2, 3, 4, 5, 0, 0, 1, 2,
    2, 3, 4, 5, 0, 0, 1, 2, 2, 3, 4, 5, 0, 0, 1, 2, 3, 3, 4, 5, 0, 0, 1, 2, 3, 3, 4, 5, 0, 0, 1, 2, 3, 3, 4, 5,
    0, 0, 1, 2, 3, 4, 4, 5, 0, 0, 1, 2, 3, 4, 5, 5, 0, 0, 1, 2, 3, 4, 5, 5,
];

/// What a frame turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// Speech, synthesised from this frame's parameters.
    Voice,
    /// Too many bit errors: the previous frame's parameters were used again.
    Repeat,
    /// A silence frame, or too many repeats in a row: silence out.
    Silence,
    /// A tone frame (not synthesised): silence out.
    Tone,
}

/// Per-frame figures for the decoder statistics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameInfo {
    pub kind: FrameKind,
    /// Bits the Golay (23,12) code corrected in the first code word.
    pub golay_errors: u32,
    /// Golay errors plus the Hamming (15,11) words that needed correcting.
    pub total_errors: u32,
}

/// The model parameters of one frame (`mbe_parms` in mbelib).
#[derive(Clone)]
struct Parms {
    w0: f32,
    l: usize,
    vl: [bool; 58],
    ml: [f32; 58],
    log2_ml: [f32; 58],
    phi: [f32; 58],
    psi: [f32; 58],
    gamma: f32,
    repeat: u32,
}

impl Parms {
    /// `mbe_initMbeParms`.
    fn init() -> Self {
        Self {
            w0: 0.09378,
            l: 30,
            vl: [false; 58],
            ml: [0.0; 58],
            log2_ml: [0.0; 58],
            phi: [0.0; 58],
            psi: [PI / 2.0; 58],
            gamma: 0.0,
            repeat: 0,
        }
    }
}

/// One voice decoder; keep one per call so the frame-to-frame prediction and
/// phases belong to that call alone.
pub struct Decoder {
    cur: Parms,
    prev: Parms,
    prev_enhanced: Parms,
    /// The C code uses `rand()`; a small generator of our own keeps the
    /// output the same on every platform.
    rng: u32,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Self {
            cur: Parms::init(),
            prev: Parms::init(),
            prev_enhanced: Parms::init(),
            rng: 0x1234_5678,
        }
    }

    /// Forget the call so far (`mbe_initMbeParms`).
    pub fn reset(&mut self) {
        self.cur = Parms::init();
        self.prev = Parms::init();
        self.prev_enhanced = Parms::init();
    }

    /// Decode one 96-bit frame, bytes as the C channel carries them (first
    /// bit in the least significant bit of byte 0), to 160 samples at 8 kHz.
    pub fn decode(
        &mut self,
        frame: &[u8; FRAME_BYTES],
        out: &mut [i16; FRAME_SAMPLES],
    ) -> FrameInfo {
        // Unpack and de-interleave (AeroAMBE::to_decode_slot).
        let mut fr = [[0u8; 24]; 6];
        for i in 0..96 {
            let bit = (frame[i / 8] >> (i % 8)) & 1;
            fr[RW[i] as usize][RX[i] as usize] = bit;
        }
        // mbe_processAmbe4800x3600Framef
        let golay_errors = ecc_c0(&mut fr);
        demodulate(&mut fr);
        let mut d = [0u8; 72];
        let total_errors = golay_errors + ecc_data(&fr, &mut d);

        let mut audio = [0f32; FRAME_SAMPLES];
        let kind = self.process(&d, total_errors, &mut audio);
        // mbe_floattoshort
        for (o, a) in out.iter_mut().zip(audio) {
            *o = (7.0 * a).clamp(-32760.0, 32760.0) as i16;
        }
        FrameInfo {
            kind,
            golay_errors,
            total_errors,
        }
    }

    /// `mbe_processAmbe3600Dataf`.
    fn process(&mut self, d: &[u8; 72], errs2: u32, audio: &mut [f32; FRAME_SAMPLES]) -> FrameKind {
        let bad = decode_parms(d, &mut self.cur, &mut self.prev);
        let mut kind = FrameKind::Voice;
        match bad {
            Bad::Silence | Bad::Tone => self.cur.repeat = 0,
            Bad::No if errs2 > 3 => {
                // mbe_useLastMbeParms
                let repeat = self.prev.repeat;
                self.cur = self.prev.clone();
                self.cur.repeat = repeat + 1;
                kind = FrameKind::Repeat;
            }
            Bad::No => self.cur.repeat = 0,
        }
        if bad == Bad::No && self.cur.repeat <= 3 {
            self.prev = self.cur.clone(); // mbe_moveMbeParms (cur, prev)
            spectral_amp_enhance(&mut self.cur);
            synthesize(audio, &mut self.cur, &mut self.prev_enhanced, &mut self.rng);
            self.prev_enhanced = self.cur.clone();
            kind
        } else {
            audio.fill(0.0);
            self.reset();
            match bad {
                Bad::Tone => FrameKind::Tone,
                _ => FrameKind::Silence,
            }
        }
    }
}

#[derive(PartialEq, Clone, Copy)]
enum Bad {
    No,
    Silence,
    Tone,
}

/// `mbe_golay2312`: corrects the 12 data bits (`io[11..23]`) in place and
/// returns how many changed.
fn golay2312(io: &mut [u8; 23]) -> u32 {
    let mut block: u32 = 0;
    for i in (0..23).rev() {
        block = (block << 1) | u32::from(io[i]);
    }
    // mbe_checkGolayBlock
    let mut expected = 0;
    for (i, g) in GOLAY_GENERATOR.iter().enumerate() {
        if block & (0x40_0000 >> i) != 0 {
            expected ^= g;
        }
    }
    let syndrome = (expected ^ (block & 0x7ff)) as usize;
    let data = (block >> 11) ^ GOLAY_MATRIX[syndrome];
    let mut errs = 0;
    for i in 0..12 {
        let bit = ((data >> i) & 1) as u8;
        if io[11 + i] != bit {
            errs += 1;
        }
        io[11 + i] = bit;
    }
    errs
}

/// `mbe_hamming1511`: corrects one error in place; returns 1 if it did.
fn hamming1511(io: &mut [u8; 15]) -> u32 {
    let mut block: u32 = 0;
    for i in (0..15).rev() {
        block = (block << 1) | u32::from(io[i]);
    }
    let mut syndrome = 0usize;
    for g in HAMMING_GENERATOR {
        syndrome = (syndrome << 1) | ((block & g).count_ones() & 1) as usize;
    }
    let mut errs = 0;
    if syndrome > 0 {
        errs = 1;
        // Not in the C code: mbelib flips bit `syndrome - 1`
        // (`hammingMatrix`), which is the wrong bit for 11 of the 15
        // positions with this generator. The bit to flip is the one whose
        // column of the check matrix equals the syndrome.
        for bit in 0..15 {
            let column = HAMMING_GENERATOR
                .iter()
                .fold(0, |acc, g| (acc << 1) | ((g >> bit) & 1) as usize);
            if column == syndrome {
                block ^= 1 << bit;
                break;
            }
        }
    }
    for (i, b) in io.iter_mut().enumerate() {
        *b = ((block >> i) & 1) as u8;
    }
    errs
}

/// `mbe_eccAmbe4800x3600C0`.
fn ecc_c0(fr: &mut [[u8; 24]; 6]) -> u32 {
    let mut w = [0u8; 23];
    w.copy_from_slice(&fr[0][1..24]);
    let errs = golay2312(&mut w);
    fr[0][1..24].copy_from_slice(&w);
    errs
}

/// `mbe_demodulateAmbe4800x3600Data`: the three Hamming words are whitened
/// with a sequence seeded by the first code word's data bits.
fn demodulate(fr: &mut [[u8; 24]; 6]) {
    let mut seed: u16 = 0;
    for i in (12..24).rev() {
        seed = (seed << 1) | u16::from(fr[0][i]);
    }
    let mut pr = seed.wrapping_mul(16);
    for row in fr.iter_mut().take(4).skip(1) {
        for j in (0..15).rev() {
            pr = pr.wrapping_mul(173).wrapping_add(13849);
            row[j] ^= (pr >> 15) as u8;
        }
    }
}

/// `mbe_eccAmbe4800x3600Data`: the 72 voice bits out of the six words.
fn ecc_data(fr: &[[u8; 24]; 6], d: &mut [u8; 72]) -> u32 {
    let mut n = 0;
    let mut push = |b: u8| {
        d[n] = b;
        n += 1;
    };
    for j in (12..24).rev() {
        push(fr[0][j]);
    }
    let mut errs = 0;
    for row in &fr[1..4] {
        let mut h = [0u8; 15];
        h.copy_from_slice(&row[..15]);
        errs += hamming1511(&mut h);
        for j in (4..15).rev() {
            push(h[j]);
        }
    }
    for j in (0..13).rev() {
        push(fr[4][j]);
    }
    for j in (0..14).rev() {
        push(fr[5][j]);
    }
    errs
}

/// A value of `bm` bits, most significant first, out of one row of `bb`.
fn field(row: &[u8; 12], bm: usize) -> usize {
    (0..bm)
        .rev()
        .fold(0, |acc, j| (acc << 1) | usize::from(row[j]))
}

/// The dequantiser shared by the gain and the higher-order coefficients.
fn dequant(value: usize, bm: usize) -> f32 {
    let x = if bm <= 4 {
        FX_LOOKUP[LOOKUP_OFFSET[bm] + value]
    } else {
        let x = value as f32 - 2f32.powi(bm as i32 - 1);
        if x < 0.0 { x - 0.5 } else { x + 0.5 }
    };
    // Not in the C code: bm = 0 reads quantstep[-1] there.
    x * DQ_QUANT_STEP[bm.saturating_sub(1)]
}

/// `mbe_decodeAmbe3600Parms`.
fn decode_parms(d: &[u8; 72], cur: &mut Parms, prev: &mut Parms) -> Bad {
    cur.repeat = prev.repeat;

    // Fundamental frequency.
    let mut b0 = 0usize;
    for (i, &bit) in d[..6].iter().enumerate() {
        b0 |= usize::from(bit) << (6 - i);
    }
    b0 |= usize::from(d[71]);
    if b0 >> 1 == 0x35 {
        return Bad::Tone;
    }
    if b0 >> 1 > 0x34 {
        cur.w0 = TAU / 32.0;
        cur.l = 14;
        cur.vl[1..=14].fill(false);
        return Bad::Silence;
    }
    cur.w0 = 4.0 * PI / (2.0 * b0 as f32 + 39.5);
    cur.l = ((0.9254 * (10 + (b0 >> 1)) as f32) as usize).clamp(9, 56);
    let l_cur = cur.l;
    let l9 = l_cur - 9;
    let unvc = 0.2046 / cur.w0.sqrt();

    // Bits into b1.. (the C array is not cleared; here it is).
    let mut bb = [[0u8; 12]; 60];
    for (i, pair) in BIT_ORDER[l9].chunks_exact(2).enumerate() {
        bb[pair[0] as usize][pair[1] as usize] = d[6 + i];
    }

    // Voicing: eight decisions spread over the harmonics.
    let c = l_cur / 8;
    let (mut j, mut k) = (0, 7usize);
    for i in 1..=l_cur {
        cur.vl[i] = bb[1][k] != 0;
        if j == c {
            j = 0;
            k = k.saturating_sub(1);
        } else {
            j += 1;
        }
    }

    let rho: f32 = if l_cur <= 15 {
        0.3
    } else if l_cur <= 24 {
        0.03 * l_cur as f32 - 0.05
    } else {
        0.8
    };

    // Gain.
    let b2 = field(&bb[2], 6);
    cur.gamma = 0.42 * B2_AERO[b2] + 0.5 * prev.gamma;

    // The seven block-average DCT coefficients.
    let mut gm = [0f32; 9];
    for i in 0..7 {
        let bm = BIT_ALLOC[l9][i] as usize;
        let x = dequant(field(&bb[i + 3], bm), bm);
        gm[i + 2] = x * DQ_PRBA_MUL[i] + DQ_PRBA_ADD[i] * (1.0 - rho);
    }
    let mut ri = [0f32; 9];
    for (i, r) in ri.iter_mut().enumerate().skip(1) {
        for m in 1..=8 {
            let am = if m == 1 { 1.0 } else { 2.0 };
            *r += am * gm[m] * (PI * (m - 1) as f32 * (i as f32 - 0.5) / 8.0).cos();
        }
    }

    // Higher-order coefficients, block by block, then an inverse DCT each.
    let mut cik = [[0f32; 18]; 9];
    let mut m = 7;
    for i in 1..=8 {
        cik[i][1] = ri[i];
        let len = LMPRBL[l9][i - 1] as usize;
        for k in 2..=len {
            let bm = BIT_ALLOC[l9][m] as usize;
            m += 1;
            cik[i][k] = if bm == 0 {
                0.0
            } else {
                dequant(field(&bb[2 + m], bm), bm) * DQ_HOC_MUL[i - 1][k - 2]
                    + DQ_HOC_ADD[i - 1][k - 2] * (1.0 - rho)
            };
        }
    }
    let mut tl = [0f32; 58];
    let mut l = 1;
    for i in 1..=8 {
        let ji = LMPRBL[l9][i - 1] as usize;
        for j in 1..=ji {
            let mut sum = 0.0;
            for k in 1..=ji {
                let ak = if k == 1 { 1.0 } else { 2.0 };
                sum += ak * cik[i][k] * (PI * (k - 1) as f32 * (j as f32 - 0.5) / ji as f32).cos();
            }
            if l < tl.len() {
                tl[l] = sum;
            }
            l += 1;
        }
    }

    // Prediction from the previous frame's spectrum.
    if l_cur > prev.l {
        for l in prev.l + 1..=l_cur {
            prev.ml[l] = prev.ml[prev.l];
            prev.log2_ml[l] = prev.log2_ml[prev.l];
        }
    }
    prev.log2_ml[0] = prev.log2_ml[1];
    prev.ml[0] = prev.ml[1];

    let mut intkl = [0usize; 58];
    let mut deltal = [0f32; 58];
    let mut sum43 = 0.0;
    for l in 1..=l_cur {
        let flokl = (prev.l as f32 / l_cur as f32) * l as f32;
        intkl[l] = flokl as usize;
        deltal[l] = flokl - intkl[l] as f32;
        sum43 +=
            (1.0 - deltal[l]) * prev.log2_ml[intkl[l]] + deltal[l] * prev.log2_ml[intkl[l] + 1];
    }
    sum43 *= 0.65 / l_cur as f32;
    let sum42 = tl[1..=l_cur].iter().sum::<f32>() / l_cur as f32;
    let big_gamma = cur.gamma - 0.5 * (l_cur as f32).log2() - sum42;
    for l in 1..=l_cur {
        let c1 = 0.65 * (1.0 - deltal[l]) * prev.log2_ml[intkl[l]];
        let c2 = 0.65 * deltal[l] * prev.log2_ml[intkl[l] + 1];
        cur.log2_ml[l] = tl[l] + c1 + c2 - sum43 + big_gamma;
        let mag = (0.693 * cur.log2_ml[l]).exp();
        cur.ml[l] = if cur.vl[l] { mag } else { unvc * mag };
    }
    Bad::No
}

/// `mbe_spectralAmpEnhance`.
fn spectral_amp_enhance(cur: &mut Parms) {
    let (mut rm0, mut rm1) = (0f32, 0f32);
    for l in 1..=cur.l {
        let p = cur.ml[l] * cur.ml[l];
        rm0 += p;
        rm1 += p * (cur.w0 * l as f32).cos();
    }
    let (r2m0, r2m1) = (rm0 * rm0, rm1 * rm1);
    for l in 1..=cur.l {
        if cur.ml[l] != 0.0 {
            let wl = cur.ml[l].sqrt()
                * ((0.96 * PI * ((r2m0 + r2m1) - 2.0 * rm0 * rm1 * (cur.w0 * l as f32).cos()))
                    / (cur.w0 * rm0 * (r2m0 - r2m1)))
                    .powf(0.25);
            if 8 * l <= cur.l {
            } else if wl > 1.2 {
                cur.ml[l] *= 1.2;
            } else if wl < 0.5 {
                cur.ml[l] *= 0.5;
            } else if wl.is_finite() {
                // Not in the C code: a NaN weight (flat spectrum) is skipped.
                cur.ml[l] *= wl;
            }
        }
    }
    let sum: f32 = cur.ml[1..=cur.l].iter().map(|m| m * m).sum();
    let gamma = if sum == 0.0 { 1.0 } else { (rm0 / sum).sqrt() };
    for m in &mut cur.ml[1..=cur.l] {
        *m *= gamma;
    }
}

/// A float in [0, 1) (the C code's `mbe_rand`).
fn rand01(rng: &mut u32) -> f32 {
    *rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    (*rng >> 8) as f32 / 16_777_216.0
}

fn rand_phase(rng: &mut u32) -> f32 {
    rand01(rng) * TAU - PI
}

/// `mbe_synthesizeSpeechf` with `uvquality` = 1, as libaeroambe calls it.
fn synthesize(out: &mut [f32; FRAME_SAMPLES], cur: &mut Parms, prev: &mut Parms, rng: &mut u32) {
    const N: usize = FRAME_SAMPLES;
    let uvthreshold = 2700.0 * PI / 4000.0;
    let uvsine = 1.359_140_9 * E;
    let uvrand = 2.0;
    let qfactor = 1.0 / E;

    let num_uv = (1..=cur.l).filter(|&l| !cur.vl[l]).count() as f32;
    let (cw0, pw0) = (cur.w0, prev.w0);
    out.fill(0.0);

    // eq 128, 129
    let maxl = cur.l.max(prev.l);
    if cur.l > prev.l {
        for l in prev.l + 1..=maxl {
            prev.ml[l] = 0.0;
            prev.vl[l] = true;
        }
    } else {
        for l in cur.l + 1..=maxl {
            cur.ml[l] = 0.0;
            cur.vl[l] = true;
        }
    }

    // eq 139, 140. Not in the C code: the phase is kept within one turn, so
    // it does not lose precision over a long call.
    for l in 1..=56 {
        cur.psi[l] = (prev.psi[l] + (pw0 + cw0) * ((l * N) as f32 / 2.0)).rem_euclid(TAU);
        cur.phi[l] = if l <= cur.l / 4 {
            cur.psi[l]
        } else {
            cur.psi[l] + num_uv * rand_phase(rng) / cur.l as f32
        };
    }

    // One unvoiced band: a cosine at the band centre with a random phase,
    // plus noise above the threshold frequency.
    let unvoiced = |n: usize, w0: f32, l: usize, phase: f32, rng: &mut u32| -> f32 {
        let w0l = w0 * l as f32;
        let mut c = (w0 * n as f32 * l as f32 + phase).cos();
        if w0l > uvthreshold {
            c += (w0l - uvthreshold) * uvrand * rand01(rng);
        }
        c * uvsine * qfactor
    };

    for l in 1..=maxl {
        let (cw0l, pw0l) = (cw0 * l as f32, pw0 * l as f32);
        match (cur.vl[l], prev.vl[l]) {
            (false, true) => {
                let ph = rand_phase(rng);
                for (n, s) in out.iter_mut().enumerate() {
                    let c1 = WS[n + N] * prev.ml[l] * (pw0l * n as f32 + prev.phi[l]).cos();
                    let c3 = unvoiced(n, cw0, l, ph, rng) * WS[n] * cur.ml[l];
                    *s += c1 + c3;
                }
            }
            (true, false) => {
                let ph = rand_phase(rng);
                for (n, s) in out.iter_mut().enumerate() {
                    let c1 = WS[n] * cur.ml[l] * (cw0l * (n as f32 - N as f32) + cur.phi[l]).cos();
                    let c3 = unvoiced(n, pw0, l, ph, rng) * WS[n + N] * prev.ml[l];
                    *s += c1 + c3;
                }
            }
            (true, true) => {
                for (n, s) in out.iter_mut().enumerate() {
                    let c1 = WS[n + N] * prev.ml[l] * (pw0l * n as f32 + prev.phi[l]).cos();
                    let c2 = WS[n] * cur.ml[l] * (cw0l * (n as f32 - N as f32) + cur.phi[l]).cos();
                    *s += c1 + c2;
                }
            }
            (false, false) => {
                let (ph, ph2) = (rand_phase(rng), rand_phase(rng));
                for (n, s) in out.iter_mut().enumerate() {
                    let c3 = unvoiced(n, pw0, l, ph, rng) * WS[n + N] * prev.ml[l];
                    let c4 = unvoiced(n, cw0, l, ph2, rng) * WS[n] * cur.ml[l];
                    *s += c3 + c4;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a Golay word the way the decoder reads it and check that up to
    /// three flipped bits are put right.
    #[test]
    fn golay_corrects_three_errors() {
        for data in [0u32, 0xfff, 0xa5a, 0x123, 0x801] {
            let mut parity = 0;
            for (i, g) in GOLAY_GENERATOR.iter().enumerate() {
                if data & (0x800 >> i) != 0 {
                    parity ^= g;
                }
            }
            let block = (data << 11) | parity;
            let mut w = [0u8; 23];
            for (i, b) in w.iter_mut().enumerate() {
                *b = ((block >> i) & 1) as u8;
            }
            let clean = w;
            assert_eq!(golay2312(&mut w), 0);
            assert_eq!(w, clean);
            w[22] ^= 1;
            w[15] ^= 1;
            w[3] ^= 1;
            assert_eq!(
                golay2312(&mut w),
                2,
                "two of the three errors were in data bits"
            );
            assert_eq!(w[11..], clean[11..]);
        }
    }

    #[test]
    fn hamming_corrects_one_error() {
        // Every valid word has syndrome 0: find them by search, then flip.
        let mut found = 0;
        for v in 0u32..(1 << 15) {
            if HAMMING_GENERATOR
                .iter()
                .all(|g| (v & g).count_ones() % 2 == 0)
            {
                found += 1;
                if found % 97 != 0 {
                    continue;
                }
                let mut w = [0u8; 15];
                for (i, b) in w.iter_mut().enumerate() {
                    *b = ((v >> i) & 1) as u8;
                }
                let clean = w;
                for flip in 0..15 {
                    let mut e = clean;
                    e[flip] ^= 1;
                    assert_eq!(hamming1511(&mut e), 1);
                    assert_eq!(e, clean);
                }
            }
        }
        assert_eq!(found, 1 << 11);
    }

    /// The bit maps must each place 96 bits in 96 different cells.
    #[test]
    fn frame_map_is_one_to_one() {
        let mut seen = std::collections::HashSet::new();
        for i in 0..96 {
            assert!(seen.insert((RW[i], RX[i])));
        }
        for row in &BIT_ORDER {
            let mut cells = std::collections::HashSet::new();
            for p in row.chunks_exact(2) {
                assert!(p[1] < 12);
                assert!(cells.insert((p[0], p[1])), "bit order repeats a cell");
            }
        }
    }

    /// Random frames must never panic or produce a run-away output, and a
    /// silence frame must be silent.
    #[test]
    fn random_frames_are_safe() {
        let mut dec = Decoder::new();
        let mut rng = 99u32;
        let mut out = [0i16; FRAME_SAMPLES];
        let mut voiced = 0;
        for _ in 0..4000 {
            let mut f = [0u8; FRAME_BYTES];
            for b in &mut f {
                rng = rng.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *b = (rng >> 24) as u8;
            }
            if dec.decode(&f, &mut out).kind == FrameKind::Voice {
                voiced += 1;
            }
        }
        assert!(voiced > 0);
    }
}
