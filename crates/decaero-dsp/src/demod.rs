// made by CasualArclamp
//! The Aero demodulator, for both modulations.
//!
//! A-BPSK (600 and 1200 bit/s) behaves as MSK and A-QPSK (8400 and 10500
//! bit/s) is offset QPSK. In both, the matched filter sampled once a *bit*
//! and turned back a quarter circle per bit leaves a real, antipodal
//! sequence on a fixed carrier phase. Everything here works on that
//! sequence, block by block and feed-forward, because loops that lean on
//! decisions fall over first on a weak signal:
//!
//! * timing: the block's samples are taken at eight trial offsets and the
//!   offset whose squares add up most coherently wins;
//! * carrier phase: half the angle of that sum of squares, interpolated
//!   from block to block;
//! * carrier frequency: the phase step between blocks, fed back slowly to
//!   the oscillator.
//!
//! The acquisition (`acquire::find`) hands over a carrier within a hertz or
//! so, which is all the feed-forward stages need.
//!
//! Output is soft bits (positive = 1) in the serial order of the frame:
//! for A-BPSK the product of neighbouring samples (JAERO `DiffDecode`), for
//! A-QPSK the two rails alternating, each with a sign the frame sync settles.

use num_complex::Complex32;
use std::f64::consts::{PI, TAU};

/// Which Aero modulation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Modulation {
    /// A-BPSK: MSK-like, half-sine matched filter, differential bits.
    ABpsk,
    /// A-QPSK: offset QPSK, root raised cosine with this roll-off
    /// (JAERO uses 1.0 at 10500 bit/s and 0.6 at 8400 bit/s).
    AQpsk { roll_off: f32 },
}

impl Modulation {
    /// The modulation an Aero bit rate uses.
    pub fn for_rate(bit_rate: u32) -> Self {
        match bit_rate {
            600 | 1200 => Self::ABpsk,
            8400 => Self::AQpsk { roll_off: 0.6 },
            _ => Self::AQpsk { roll_off: 1.0 },
        }
    }
}

/// Trial timing offsets per bit.
const TRIALS: usize = 8;

/// What the GUI shows about the carrier.
#[derive(Debug, Clone, Default)]
pub struct DemodStatus {
    /// 0 for noise, towards 1 for a clean signal; above about 0.1 is a carrier.
    pub coherence: f32,
    /// Es/N0 from the moments of the samples, in dB (an estimate; the FEC
    /// figures are the firmer ones).
    pub es_n0_db: f32,
    /// Carrier offset being tracked, relative to the samples' centre, in Hz.
    pub offset_hz: f64,
    /// Sample clock error against the nominal bit rate, in parts per million.
    pub clock_ppm: f64,
    /// Recent samples for the constellation: turned onto the real axis for
    /// A-BPSK, the two rails paired into four corners for A-QPSK.
    pub scatter: Vec<Complex32>,
}

pub struct Demod {
    modulation: Modulation,
    bit_rate: f64,
    rate: f64,
    /// Samples per bit.
    sps: f64,
    block: usize,
    // Oscillator taking out the carrier offset.
    nco_phase: f64,
    nco_hz: f64,
    // Matched filter.
    taps: Vec<f32>,
    tail: Vec<Complex32>,
    /// Filtered samples; `hist[0]` is absolute sample `base`.
    hist: Vec<Complex32>,
    base: usize,
    /// Absolute sample position of the next block's first bit.
    pos: f64,
    /// Bits demodulated so far (sets the quarter-circle turn).
    bit_index: u64,
    blocks_done: u64,
    /// The block waiting for the next phase measurement.
    held: Vec<Complex32>,
    held_index: u64,
    theta_before: Option<f64>,
    theta_held: f64,
    last_a: f32,
    timing_sum: f64,
    /// Averaged quarter-to-quarter turn of the squared sums.
    freq_vector: Complex32,
    status: DemodStatus,
    m2: f32,
    m4: f32,
}

pub(crate) fn rrc(sps: f64, alpha: f64, span: usize) -> Vec<f32> {
    let n = (span as f64 * sps).round() as usize | 1;
    let mid = (n / 2) as f64;
    (0..n)
        .map(|i| {
            let t = (i as f64 - mid) / sps;
            let v = if t.abs() < 1e-9 {
                1.0 - alpha + 4.0 * alpha / PI
            } else if alpha > 0.0 && (t.abs() - 1.0 / (4.0 * alpha)).abs() < 1e-9 {
                alpha / 2f64.sqrt()
                    * ((1.0 + 2.0 / PI) * (PI / (4.0 * alpha)).sin()
                        + (1.0 - 2.0 / PI) * (PI / (4.0 * alpha)).cos())
            } else {
                ((PI * t * (1.0 - alpha)).sin() + 4.0 * alpha * t * (PI * t * (1.0 + alpha)).cos())
                    / (PI * t * (1.0 - (4.0 * alpha * t).powi(2)))
            };
            v as f32
        })
        .collect()
}

impl Demod {
    /// `rate` is the sample rate of the complex baseband handed to
    /// [`Demod::process`] (six to ten samples a bit suits it), `offset_hz`
    /// the carrier's offset from its centre as acquisition found it.
    pub fn new(modulation: Modulation, bit_rate: u32, rate: f64, offset_hz: f64) -> Self {
        let bit_rate = f64::from(bit_rate);
        let sps = rate / bit_rate;
        let mut taps = match modulation {
            // Half a sine over two bits: MSK's own pulse.
            Modulation::ABpsk => {
                let n = (2.0 * sps).round() as usize;
                (0..n)
                    .map(|i| (PI * (i as f64 + 0.5) / n as f64).sin() as f32)
                    .collect()
            }
            // A symbol on one rail lasts two bits.
            Modulation::AQpsk { roll_off } => rrc(2.0 * sps, f64::from(roll_off), 8),
        };
        let energy = taps.iter().map(|t| t * t).sum::<f32>().sqrt();
        for t in &mut taps {
            *t /= energy;
        }
        // A block is about a tenth of a second at the slow rates and 25 to
        // 60 ms at the fast ones: long enough to measure phase in noise,
        // short enough to follow a drifting carrier. (Tried 128 to 1024 on
        // weak 8400 and 10500 bit/s recordings: the error rate barely moves,
        // but the frequency loop wanders at 8400 below 512.)
        let block = match bit_rate as u32 {
            0..1000 => 64,
            1000..5000 => 128,
            5000..9000 => 512,
            _ => 256,
        };
        Self {
            modulation,
            bit_rate,
            rate,
            sps,
            block,
            nco_phase: 0.0,
            nco_hz: offset_hz,
            tail: vec![Complex32::default(); taps.len() - 1],
            taps,
            hist: Vec::new(),
            base: 0,
            pos: 2.0 * sps,
            bit_index: 0,
            blocks_done: 0,
            held: Vec::new(),
            held_index: 0,
            theta_before: None,
            theta_held: 0.0,
            last_a: 0.0,
            timing_sum: 0.0,
            freq_vector: Complex32::default(),
            status: DemodStatus {
                offset_hz,
                ..DemodStatus::default()
            },
            m2: 0.0,
            m4: 0.0,
        }
    }

    /// Move the carrier oscillator to `offset_hz` and forget what the
    /// frequency loop had gathered. For a carrier found again by other means
    /// after the loop has been thrown further than it can pull back from
    /// (it reads only a few hertz either way at the slow rates).
    pub fn retune(&mut self, offset_hz: f64) {
        self.nco_hz = offset_hz;
        self.freq_vector = Complex32::default();
        self.status.offset_hz = offset_hz;
    }

    pub fn status(&self) -> &DemodStatus {
        &self.status
    }
    pub fn modulation(&self) -> Modulation {
        self.modulation
    }

    /// The filtered signal at a fractional sample position (linear between
    /// samples: at eight samples a bit the error is far below the noise).
    fn at(&self, pos: f64) -> Complex32 {
        let i = pos.floor();
        let f = (pos - i) as f32;
        let k = i as usize - self.base;
        self.hist[k] * (1.0 - f) + self.hist[k + 1] * f
    }

    /// A quarter circle back per bit.
    fn turn(index: u64) -> Complex32 {
        match index & 3 {
            0 => Complex32::new(1.0, 0.0),
            1 => Complex32::new(0.0, -1.0),
            2 => Complex32::new(-1.0, 0.0),
            _ => Complex32::new(0.0, 1.0),
        }
    }

    /// Complex baseband in, soft bits (positive = 1) appended to `out`.
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<f32>) {
        // Carrier offset out, then the matched filter.
        let step = -TAU * self.nco_hz / self.rate;
        let n_taps = self.taps.len();
        let mut work = std::mem::take(&mut self.tail);
        work.reserve(input.len());
        for &x in input {
            let (s, c) = self.nco_phase.sin_cos();
            self.nco_phase = (self.nco_phase + step).rem_euclid(TAU);
            work.push(x * Complex32::new(c as f32, s as f32));
        }
        // Every output the input reaches, a block at a time (AVX2 or
        // AVX-512 where the CPU has them). The taps are symmetric, so their
        // order does not matter.
        crate::simd::fir_cr(&work, &self.taps, &mut self.hist);
        self.tail = work.split_off(work.len() - (n_taps - 1));

        // Whole blocks while the filtered samples reach.
        loop {
            let need = self.pos + (self.block as f64 + 0.5) * self.sps + 2.0;
            if (need as usize) >= self.base + self.hist.len() {
                break;
            }
            self.do_block(out);
        }
        // Drop what is behind us.
        let keep_from = (self.pos - self.sps).floor().max(self.base as f64) as usize;
        if keep_from - self.base > 8192 {
            self.hist.drain(..keep_from - self.base);
            self.base = keep_from;
        }
    }

    fn do_block(&mut self, out: &mut Vec<f32>) {
        let b = self.block;
        // Sum of squares at each trial offset, kept per quarter of the block
        // so a carrier a few hertz off (which turns the sum within a block)
        // can still be timed and measured; the samples at offset 0 kept.
        let mut quarters = [[Complex32::default(); TRIALS]; 4];
        let mut centre = Vec::with_capacity(b);
        let mut energy = 0f32;
        for n in 0..b {
            let turn = Self::turn(self.bit_index + n as u64);
            let q = n * 4 / b;
            for (k, sum) in quarters[q].iter_mut().enumerate() {
                let offset = (k as f64 - (TRIALS / 2) as f64) / TRIALS as f64;
                let y = self.at(self.pos + (n as f64 + offset) * self.sps) * turn;
                *sum += y * y;
                if k == TRIALS / 2 {
                    centre.push(y);
                    energy += y.norm_sqr();
                }
            }
        }
        // Timing: where the quarters' sums are largest, to a fraction of a
        // trial step.
        let mags: Vec<f32> = (0..TRIALS)
            .map(|k| quarters.iter().map(|q| q[k].norm()).sum())
            .collect();
        let peak = (0..TRIALS)
            .max_by(|&a, &c| mags[a].total_cmp(&mags[c]))
            .unwrap_or(TRIALS / 2);
        let (l, m, r) = (
            mags[(peak + TRIALS - 1) % TRIALS],
            mags[peak],
            mags[(peak + 1) % TRIALS],
        );
        let frac = if (l - 2.0 * m + r).abs() > 1e-12 {
            0.5 * (l - r) / (l - 2.0 * m + r)
        } else {
            0.0
        };
        let mut error = (peak as f64 + f64::from(frac) - (TRIALS / 2) as f64) / TRIALS as f64;
        if error >= 0.5 {
            error -= 1.0;
        }
        // Frequency: how far the squared sum turns from one quarter to the
        // next. Averaged as a vector over a few blocks, it holds up in noise
        // and reads offsets up to a quarter of the bit rate over the block
        // length without wrapping.
        let c = TRIALS / 2;
        let turned: Complex32 = (0..3)
            .map(|q| quarters[q + 1][c] * quarters[q][c].conj())
            .sum();
        self.freq_vector = self.freq_vector * 0.7 + turned;
        let sums: [Complex32; TRIALS] =
            std::array::from_fn(|k| quarters.iter().map(|q| q[k]).sum());
        // Jump straight there at the start, then follow gently.
        let gain = if self.blocks_done < 3 { 1.0 } else { 0.15 };
        let correction = gain * error * self.sps;
        self.timing_sum = 0.98 * self.timing_sum + 0.02 * correction / (b as f64 * self.sps) * 1e6;

        // Carrier phase of this block (known to within half a circle).
        let m_centre = sums[TRIALS / 2];
        let raw = 0.5 * f64::from(m_centre.im).atan2(f64::from(m_centre.re));
        let theta = if self.blocks_done == 0 {
            raw
        } else {
            let mut d = raw - self.theta_held;
            d -= PI * (d / PI).round();
            self.theta_held + d
        };
        let coherence = if energy > 0.0 {
            m_centre.norm() / energy
        } else {
            0.0
        };
        self.status.coherence = 0.8 * self.status.coherence + 0.2 * coherence;

        if self.blocks_done > 0 {
            // The held block now has a phase on both sides: give its bits.
            let step_after = theta - self.theta_held;
            let step_before = self
                .theta_before
                .map_or(step_after, |t| self.theta_held - t);
            let held = std::mem::take(&mut self.held);
            let mut scatter = Vec::with_capacity(held.len());
            let (mut m2, mut m4) = (0f32, 0f32);
            let mut a = Vec::with_capacity(held.len());
            for (i, y) in held.iter().enumerate() {
                let x = (i as f64 + 0.5) / held.len() as f64 - 0.5;
                let th = self.theta_held + x * if x < 0.0 { step_before } else { step_after };
                let v = y * Complex32::new(th.cos() as f32, -th.sin() as f32);
                m2 += v.re * v.re;
                m4 += v.re.powi(4);
                a.push(v.re);
                scatter.push(v);
            }
            let n = a.len() as f32;
            self.m2 = if self.m2 == 0.0 {
                m2 / n
            } else {
                0.9 * self.m2 + 0.1 * m2 / n
            };
            self.m4 = if self.m4 == 0.0 {
                m4 / n
            } else {
                0.9 * self.m4 + 0.1 * m4 / n
            };
            // Antipodal signal in Gaussian noise: A^4 = (3 M2^2 - M4) / 2.
            let a2 = ((3.0 * self.m2 * self.m2 - self.m4) / 2.0).max(0.0).sqrt();
            let noise = (self.m2 - a2).max(self.m2 * 1e-3);
            self.status.es_n0_db = 10.0 * (a2 / (2.0 * noise)).max(1e-3).log10();
            let scale = 1.0 / self.m2.sqrt().max(1e-12);
            for s in &mut scatter {
                *s *= scale;
            }
            // Offset QPSK is shown with the offset taken out: the decision
            // of one rail against the next decision of the other, which is
            // the four corners of plain QPSK. (Drawn as it is sampled, the
            // other rail is caught half way between its decisions and
            // smears into three lumps.)
            if matches!(self.modulation, Modulation::AQpsk { .. }) {
                scatter = scatter
                    .chunks_exact(2)
                    .map(|pair| Complex32::new(pair[0].re, pair[1].re))
                    .collect();
            }
            self.status.scatter = scatter;
            for (i, &v) in a.iter().enumerate() {
                let v = v * scale;
                match self.modulation {
                    Modulation::ABpsk => {
                        // JAERO `DiffDecode`: the bit is whether neighbours
                        // agree, as sure as the less sure of the two.
                        let mag = v.abs().min(self.last_a.abs());
                        out.push(if v * self.last_a >= 0.0 { mag } else { -mag });
                        self.last_a = v;
                    }
                    Modulation::AQpsk { .. } => {
                        // The turn leaves I0, Q0, -I1, -Q1, ...: undo the signs.
                        let sign = if (self.held_index + i as u64) & 2 == 0 {
                            1.0
                        } else {
                            -1.0
                        };
                        out.push(v * sign);
                    }
                }
            }
            // Feed the frequency error back slowly. The oscillator's phase
            // stays continuous, so nothing jumps.
            let per_quarter =
                0.5 * f64::from(self.freq_vector.im).atan2(f64::from(self.freq_vector.re));
            let hz = per_quarter / (TAU * (b / 4) as f64 / self.bit_rate);
            self.nco_hz += 0.1 * hz;
            self.status.offset_hz = self.nco_hz;
            self.status.clock_ppm = self.timing_sum;
            self.theta_before = Some(self.theta_held);
        }
        self.theta_held = theta;
        self.held = centre;
        self.held_index = self.bit_index;
        self.bit_index += b as u64;
        self.blocks_done += 1;
        self.pos += b as f64 * self.sps + correction;
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

    /// An MSK modulator: each bit moves the phase a quarter circle up or down.
    fn msk(bits: &[u8], sps: usize, offset_cycles_per_sample: f64) -> Vec<Complex32> {
        let mut phase = 0.3f64;
        let mut out = Vec::with_capacity(bits.len() * sps);
        for &b in bits {
            let step = if b == 1 { 1.0 } else { -1.0 } * (PI / 2.0) / sps as f64;
            for _ in 0..sps {
                phase += step + TAU * offset_cycles_per_sample;
                out.push(Complex32::new(phase.cos() as f32, phase.sin() as f32));
            }
        }
        out
    }

    /// The demodulator must give MSK's bits back (as a whole either right or
    /// inverted), with a carrier offset and noise at 4 dB Es/N0.
    #[test]
    fn msk_bits_come_back_through_noise() {
        let mut seed = 77;
        let bits: Vec<u8> = (0..6000).map(|_| (lcg(&mut seed) >> 31) as u8).collect();
        let sps = 8;
        let rate = 4800.0;
        // 0.8 Hz off where acquisition would have put it.
        let mut signal = msk(&bits, sps, 0.8 / rate);
        // Es/N0 4 dB with unit-power signal: noise variance per dimension
        // sigma^2 = sps / (2 Es/N0).
        let esn0 = 10f32.powf(0.4);
        let sigma = (sps as f32 / (2.0 * esn0)).sqrt();
        for s in &mut signal {
            *s += Complex32::new(sigma * gauss(&mut seed), sigma * gauss(&mut seed));
        }
        let mut demod = Demod::new(Modulation::ABpsk, 600, rate, 0.0);
        let mut soft = Vec::new();
        for chunk in signal.chunks(1000) {
            demod.process(chunk, &mut soft);
        }
        assert!(soft.len() > 5500);
        // Find the lag and sense that line the bits up, past the start-up.
        let hard: Vec<u8> = soft.iter().map(|&s| u8::from(s > 0.0)).collect();
        let mut best = (usize::MAX, 0usize);
        for lag in 0..8 {
            let wrong = (300..5000).filter(|&i| hard[i] != bits[i + lag]).count();
            let wrong = wrong.min(4700 - wrong);
            if wrong < best.0 {
                best = (wrong, lag);
            }
        }
        // Differential detection at 4 dB makes roughly 3 % errors.
        assert!(
            best.0 < 4700 * 6 / 100,
            "{} errors of 4700 at lag {}",
            best.0,
            best.1
        );
        let st = demod.status();
        assert!(st.coherence > 0.3, "coherence {}", st.coherence);
        assert!(
            (st.es_n0_db - 4.0).abs() < 2.0,
            "Es/N0 estimate {}",
            st.es_n0_db
        );
        assert!((st.offset_hz - 0.8).abs() < 0.5, "offset {}", st.offset_hz);
    }

    /// Offset QPSK is shown as plain QPSK's four corners, not as two
    /// columns with the other rail smeared between its decisions.
    #[test]
    fn offset_qpsk_is_shown_as_four_corners() {
        // MSK is offset QPSK with half-sine pulses: near enough to the root
        // raised cosine for the corners to stand clear.
        let mut seed = 9;
        let bits: Vec<u8> = (0..4000).map(|_| (lcg(&mut seed) >> 31) as u8).collect();
        let signal = msk(&bits, 8, 0.0);
        let mut demod = Demod::new(Modulation::AQpsk { roll_off: 1.0 }, 600, 4800.0, 0.0);
        let mut soft = Vec::new();
        for chunk in signal.chunks(1000) {
            demod.process(chunk, &mut soft);
        }
        let scatter = &demod.status().scatter;
        assert!(scatter.len() >= 16, "{}", scatter.len());
        let corner = |v: f32| (0.5..1.6).contains(&v.abs());
        let good = scatter
            .iter()
            .filter(|p| corner(p.re) && corner(p.im))
            .count();
        assert!(
            good * 10 >= scatter.len() * 9,
            "{good} of {}",
            scatter.len()
        );
        // All four are used.
        for (re, im) in [(1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)] {
            assert!(scatter.iter().any(|p| p.re * re > 0.0 && p.im * im > 0.0));
        }
    }

    #[test]
    fn noise_has_no_coherence() {
        let mut seed = 5;
        let noise: Vec<Complex32> = (0..40_000)
            .map(|_| Complex32::new(gauss(&mut seed), gauss(&mut seed)))
            .collect();
        let mut demod = Demod::new(Modulation::ABpsk, 600, 4800.0, 0.0);
        let mut soft = Vec::new();
        demod.process(&noise, &mut soft);
        assert!(
            demod.status().coherence < 0.25,
            "{}",
            demod.status().coherence
        );
    }
}
