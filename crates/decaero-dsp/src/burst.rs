// made by CasualArclamp
//! Bursts (the R and T channels): find one in a channel's samples, then
//! demodulate the whole of it at once.
//!
//! A burst is too short for loops to settle (an R burst at 10500 bit/s lasts
//! about 40 ms), but once it is all in memory nothing has to settle: the
//! carrier is read off the burst's own squared spectrum, one timing offset is
//! chosen for the whole burst, and the phase is measured in short blocks and
//! interpolated between them. The same quarter-circle-per-bit view as the
//! continuous demodulator (`demod.rs`) is used, so the bits come out in the
//! same form for the frame decoder.

use crate::demod::{Modulation, rrc};
use crate::simd;
use num_complex::Complex32;
use rustfft::FftPlanner;
use std::collections::VecDeque;
use std::f64::consts::{PI, TAU};

/// One demodulated burst.
#[derive(Debug, Clone)]
pub struct BurstBits {
    /// Soft bits (positive = 1), preamble and all: the frame decoder finds
    /// the unique word.
    pub soft: Vec<f32>,
    /// Carrier offset from the centre of the samples, Hz.
    pub offset_hz: f64,
    pub es_n0_db: f32,
    /// How long the burst lasted, seconds.
    pub seconds: f64,
    /// For the constellation: samples turned onto the real axis for A-BPSK,
    /// the two rails paired into four corners for A-QPSK.
    pub scatter: Vec<Complex32>,
}

fn matched_taps(modulation: Modulation, sps: f64) -> Vec<f32> {
    let mut taps: Vec<f32> = match modulation {
        Modulation::ABpsk => {
            let n = (2.0 * sps).round() as usize;
            (0..n)
                .map(|i| (PI * (i as f64 + 0.5) / n as f64).sin() as f32)
                .collect()
        }
        Modulation::AQpsk { roll_off } => rrc(2.0 * sps, f64::from(roll_off), 8),
        // Bursts are Aero only; plain BPSK is here for completeness.
        Modulation::Bpsk { roll_off } => rrc(sps, f64::from(roll_off.max(0.1)), 8),
    };
    let energy = taps.iter().map(|t| t * t).sum::<f32>().sqrt();
    for t in &mut taps {
        *t /= energy;
    }
    taps
}

fn turn(index: usize) -> Complex32 {
    match index & 3 {
        0 => Complex32::new(1.0, 0.0),
        1 => Complex32::new(0.0, -1.0),
        2 => Complex32::new(-1.0, 0.0),
        _ => Complex32::new(0.0, 1.0),
    }
}

/// Demodulate one burst held whole in `samples` (complex baseband at `rate`,
/// about eight samples a bit, the carrier within `search_hz` of the centre).
/// `None` when the samples do not hold a carrier of this bit rate.
pub fn demodulate(
    samples: &[Complex32],
    rate: f64,
    bit_rate: u32,
    search_hz: f64,
) -> Option<BurstBits> {
    let modulation = Modulation::for_rate(bit_rate);
    let rb = f64::from(bit_rate);
    let sps = rate / rb;
    if samples.len() < (40.0 * sps) as usize {
        return None;
    }
    // Carrier: the squared burst has lines at 2 fc - Rb/2 and 2 fc + Rb/2.
    let n = (samples.len() * 2).next_power_of_two().max(4096);
    let mut buf = vec![Complex32::default(); n];
    for (b, &s) in buf.iter_mut().zip(samples) {
        *b = s * s;
    }
    FftPlanner::<f32>::new()
        .plan_fft_forward(n)
        .process(&mut buf);
    let power: Vec<f32> = buf.iter().map(|b| b.norm_sqr()).collect();
    let bin_hz = rate / n as f64;
    let at = |hz: f64| power[((hz / bin_hz).round() as i64).rem_euclid(n as i64) as usize];
    let steps = (2.0 * search_hz / bin_hz).ceil() as i64;
    let (mut best, mut best_twice) = (0f32, 0f64);
    for k in -steps..=steps {
        let twice = k as f64 * bin_hz;
        let weaker = at(twice - rb / 2.0).min(at(twice + rb / 2.0));
        if weaker > best {
            best = weaker;
            best_twice = twice;
        }
    }
    let mut sorted = power.clone();
    sorted.sort_by(f32::total_cmp);
    // A burst's lines stand above the rest of its squared spectrum (less so
    // for a short one). What slips through here by chance is thrown out by
    // the unique word and the CRC.
    if std::env::var("AERO_BURST_TRACE").is_ok() {
        eprintln!(
            "  lines {:.0} x median at {:+.0} Hz, {:.3} s",
            best / sorted[n / 2].max(f32::MIN_POSITIVE),
            best_twice / 2.0,
            samples.len() as f64 / rate
        );
    }
    if best < 8.0 * sorted[n / 2].max(f32::MIN_POSITIVE) {
        return None;
    }
    let offset_hz = best_twice / 2.0;

    // Carrier out, then the matched filter.
    let step = -TAU * offset_hz / rate;
    let taps = matched_taps(modulation, sps);
    // Silence either side, so the filter's run-in costs none of a short burst.
    let pad = taps.len();
    let mut mixed = vec![Complex32::default(); pad];
    mixed.extend(samples.iter().enumerate().map(|(i, &x)| {
        let (s, c) = (step * i as f64).sin_cos();
        x * Complex32::new(c as f32, s as f32)
    }));
    mixed.extend(std::iter::repeat_n(Complex32::default(), pad));
    let mut filtered = Vec::with_capacity(mixed.len());
    simd::fir_cr(&mixed, &taps, &mut filtered);
    let bits = ((filtered.len() as f64 - 2.0) / sps) as usize - 1;
    let block = if bit_rate > 5000 { 64 } else { 32 };
    if bits < 2 * block {
        return None;
    }
    let sample_at = |pos: f64| -> Complex32 {
        let i = pos.floor();
        let f = (pos - i) as f32;
        let k = (i as usize).min(filtered.len() - 2);
        filtered[k] * (1.0 - f) + filtered[k + 1] * f
    };

    // One timing offset for the whole burst: where the blocks' squared sums
    // are largest.
    const TRIALS: usize = 16;
    let metric: Vec<f32> = (0..TRIALS)
        .map(|k| {
            let tau = k as f64 / TRIALS as f64;
            let mut total = 0f32;
            for b in 0..bits / block {
                let mut sum = Complex32::default();
                for n in b * block..(b + 1) * block {
                    let y = sample_at((n as f64 + tau) * sps) * turn(n);
                    sum += y * y;
                }
                total += sum.norm();
            }
            total
        })
        .collect();
    let peak = (0..TRIALS)
        .max_by(|&a, &b| metric[a].total_cmp(&metric[b]))
        .unwrap_or(0);
    let (l, m, r) = (
        metric[(peak + TRIALS - 1) % TRIALS],
        metric[peak],
        metric[(peak + 1) % TRIALS],
    );
    let frac = if (l - 2.0 * m + r).abs() > 1e-12 {
        0.5 * (l - r) / (l - 2.0 * m + r)
    } else {
        0.0
    };
    let tau = (peak as f64 + f64::from(frac)).rem_euclid(TRIALS as f64) / TRIALS as f64;

    // Phase in blocks, each known to within half a circle and carried on
    // from its neighbour, then interpolated bit by bit.
    let y: Vec<Complex32> = (0..bits)
        .map(|n| sample_at((n as f64 + tau) * sps) * turn(n))
        .collect();
    let blocks = bits / block;
    let mut theta = Vec::with_capacity(blocks);
    for b in 0..blocks {
        let sum: Complex32 = y[b * block..(b + 1) * block].iter().map(|v| v * v).sum();
        let raw = 0.5 * f64::from(sum.im).atan2(f64::from(sum.re));
        let t = match theta.last() {
            None => raw,
            Some(&prev) => {
                let d: f64 = raw - prev;
                prev + d - PI * (d / PI).round()
            }
        };
        theta.push(t);
    }
    let phase_at = |n: usize| -> f64 {
        // Position in blocks, measured from the first block's centre.
        let x = (n as f64 + 0.5) / block as f64 - 0.5;
        let i = (x.floor().max(0.0) as usize).min(blocks.saturating_sub(2));
        if blocks < 2 {
            return theta[0];
        }
        theta[i] + (x - i as f64) * (theta[i + 1] - theta[i])
    };
    let rotated: Vec<Complex32> = y
        .iter()
        .enumerate()
        .map(|(n, v)| {
            let th = phase_at(n);
            v * Complex32::new(th.cos() as f32, -th.sin() as f32)
        })
        .collect();
    // Level and Es/N0 from the blocks that are the burst itself: its ends
    // are ramp and noise, weaker than the body.
    let level = |c: &[Complex32]| c.iter().map(|v| v.re * v.re).sum::<f32>() / c.len() as f32;
    let mut levels: Vec<f32> = rotated.chunks(block).map(level).collect();
    levels.sort_by(f32::total_cmp);
    let m2 = levels[levels.len() / 2].max(1e-20);
    let body: Vec<f32> = rotated
        .chunks(block)
        .filter(|c| level(c) > 0.5 * m2)
        .flat_map(|c| c.iter().map(|v| v.re))
        .collect();
    let count = body.len().max(1) as f32;
    let m2s = body.iter().map(|a| a * a).sum::<f32>() / count;
    let m4 = body.iter().map(|a| a.powi(4)).sum::<f32>() / count;
    // Antipodal signal in Gaussian noise: A^4 = (3 M2^2 - M4) / 2.
    let a2 = ((3.0 * m2s * m2s - m4) / 2.0).max(0.0).sqrt();
    let noise = (m2s - a2).max(m2s * 1e-3);
    let es_n0_db = 10.0 * (a2 / (2.0 * noise)).max(1e-3).log10();
    let scale = 1.0 / m2.sqrt();

    let mut soft = Vec::with_capacity(bits);
    let mut last = 0f32;
    for (n, v) in rotated.iter().enumerate() {
        let a = v.re * scale;
        match modulation {
            Modulation::ABpsk => {
                let mag = a.abs().min(last.abs());
                soft.push(if a * last >= 0.0 { mag } else { -mag });
                last = a;
            }
            Modulation::AQpsk { .. } => soft.push(if n & 2 == 0 { a } else { -a }),
            Modulation::Bpsk { .. } => soft.push(a),
        }
    }
    Some(BurstBits {
        soft,
        offset_hz,
        es_n0_db,
        seconds: samples.len() as f64 / rate,
        // Offset QPSK is shown with the offset taken out, as the continuous
        // channels are (`demod.rs`): the four corners of plain QPSK.
        scatter: match modulation {
            Modulation::ABpsk | Modulation::Bpsk { .. } => {
                rotated.iter().map(|v| v * scale).collect()
            }
            Modulation::AQpsk { .. } => rotated
                .chunks_exact(2)
                .map(|pair| Complex32::new(pair[0].re, pair[1].re) * scale)
                .collect(),
        },
    })
}

/// Watches a channel's samples for bursts and hands each one out demodulated.
pub struct BurstReceiver {
    rate: f64,
    bit_rate: u32,
    search_hz: f64,
    /// Running sum over half a bit: a wide enough filter for an aircraft's
    /// Doppler, narrow enough to lift a burst out of the noise.
    window: VecDeque<Complex32>,
    sum: Complex32,
    half_bit: usize,
    /// Smoothed power and the noise floor it is compared with.
    power: f32,
    floor: f32,
    smooth: f32,
    fall: f32,
    rise: f32,
    /// Samples before a burst starts, so its first bits are not lost.
    before: VecDeque<Complex32>,
    keep_before: usize,
    capture: Option<Vec<Complex32>>,
    above: usize,
    below: usize,
    start_after: usize,
    end_after: usize,
    max_len: usize,
    warm: usize,
    /// Bursts of energy seen, whether or not they turned out to be ours.
    pub seen: u64,
}

impl BurstReceiver {
    /// `rate` is the sample rate handed to [`BurstReceiver::process`], about
    /// eight samples a bit.
    pub fn new(rate: f64, bit_rate: u32) -> Self {
        let sps = rate / f64::from(bit_rate);
        let bits = |n: f64| (n * sps) as usize;
        Self {
            rate,
            bit_rate,
            // Aircraft Doppler and oscillator error: a few hundred hertz at
            // the slow rates, more room at the fast one.
            search_hz: (0.4 * f64::from(bit_rate)).max(450.0),
            window: VecDeque::new(),
            sum: Complex32::default(),
            half_bit: bits(0.5).max(1),
            power: 0.0,
            floor: f32::MAX,
            smooth: (1.0 / (16.0 * sps)) as f32,
            // The floor comes down in a fifth of a second and up in two.
            fall: (1.0 / (0.2 * rate)) as f32,
            rise: (1.0 / (2.0 * rate)) as f32,
            before: VecDeque::new(),
            keep_before: bits(96.0),
            capture: None,
            above: 0,
            below: 0,
            start_after: bits(8.0),
            end_after: bits(24.0),
            // The longest T burst reckoned with, and some preamble.
            max_len: bits(64.0 * (5.0 + 3.0 * 31.0) + 600.0),
            warm: bits(64.0),
            seen: 0,
        }
    }

    /// True while a burst is being collected.
    pub fn busy(&self) -> bool {
        self.capture.is_some()
    }

    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<BurstBits>) {
        for &x in input {
            self.window.push_back(x);
            self.sum += x;
            if self.window.len() > self.half_bit
                && let Some(old) = self.window.pop_front()
            {
                self.sum -= old;
            }
            self.power += self.smooth * (self.sum.norm_sqr() - self.power);
            if self.warm > 0 {
                self.warm -= 1;
                if self.warm == 0 {
                    self.floor = self.power;
                }
                continue;
            }
            match &mut self.capture {
                None => {
                    self.before.push_back(x);
                    if self.before.len() > self.keep_before {
                        self.before.pop_front();
                    }
                    // The floor is the average noise power: it follows the
                    // power down quickly and up slowly, so a burst that
                    // slips under the start threshold barely moves it.
                    let gain = if self.power < self.floor {
                        self.fall
                    } else {
                        self.rise
                    };
                    self.floor += gain * (self.power - self.floor);
                    self.above = if self.power > 1.8 * self.floor {
                        self.above + 1
                    } else {
                        0
                    };
                    if self.above >= self.start_after {
                        self.capture = Some(self.before.drain(..).collect());
                        self.above = 0;
                        self.below = 0;
                    }
                }
                Some(burst) => {
                    burst.push(x);
                    self.below = if self.power < 1.3 * self.floor {
                        self.below + 1
                    } else {
                        0
                    };
                    let over = burst.len() >= self.max_len;
                    if self.below >= self.end_after || over {
                        let burst = self.capture.take().unwrap_or_default();
                        self.seen += 1;
                        if std::env::var("AERO_BURST_TRACE").is_ok() {
                            eprintln!(
                                "energy for {:.3} s (over {over})",
                                burst.len() as f64 / self.rate
                            );
                        }
                        if over {
                            // Not a burst: a carrier that stays. Let the
                            // floor come up to it.
                            self.floor = self.power;
                        } else if let Some(bits) =
                            demodulate(&burst, self.rate, self.bit_rate, self.search_hz)
                        {
                            out.push(bits);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use decaero_core::pframe::UNIQUE_WORD;
    use decaero_core::rtframe::{self, Burst};

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

    /// MSK whose *differentially decoded* bits are `bits`: the phase turns a
    /// quarter circle up when a bit equals the one before it... which is how
    /// A-BPSK reaches the demodulator (it gives the product of neighbours).
    fn msk(bits: &[u8], sps: usize, offset_hz: f64, rate: f64) -> Vec<Complex32> {
        let mut phase = 1.1f64;
        let mut out = Vec::new();
        for &b in bits {
            let step = if b == 1 { 1.0 } else { -1.0 } * (PI / 2.0) / sps as f64;
            for _ in 0..sps {
                phase += step + TAU * offset_hz / rate;
                out.push(Complex32::new(phase.cos() as f32, phase.sin() as f32));
            }
        }
        out
    }

    /// An R burst at 1200 bit/s, 180 Hz off, in noise at 6 dB Es/N0, between
    /// stretches of noise alone: found, demodulated and decoded.
    #[test]
    fn an_r_burst_in_noise_is_received() {
        let (rate, sps) = (9600.0, 8usize);
        let mut seed = 21;
        let mut unit = [0u8; rtframe::R_BYTES];
        for b in &mut unit {
            *b = (lcg(&mut seed) >> 24) as u8;
        }
        let mut bits: Vec<u8> = (0..120).map(|i| ((i / 2) & 1) as u8).collect();
        bits.extend((0..32).rev().map(|i| ((UNIQUE_WORD >> i) & 1) as u8));
        bits.extend(rtframe::encode_r(&unit, false));
        bits.extend([0, 1, 1, 0, 1, 0, 0, 1]);
        let burst = msk(&bits, sps, 180.0, rate);
        let sigma = (sps as f32 / (2.0 * 10f32.powf(0.6))).sqrt();
        let mut signal = vec![Complex32::default(); 30_000];
        signal.extend(burst);
        signal.extend(vec![Complex32::default(); 20_000]);
        for s in &mut signal {
            *s += Complex32::new(sigma * gauss(&mut seed), sigma * gauss(&mut seed));
        }
        let mut rx = BurstReceiver::new(rate, 1200);
        let mut out = Vec::new();
        for chunk in signal.chunks(777) {
            rx.process(chunk, &mut out);
        }
        assert_eq!(out.len(), 1, "one burst");
        let b = &out[0];
        assert!((b.offset_hz - 180.0).abs() < 15.0, "offset {}", b.offset_hz);
        assert!((b.es_n0_db - 6.0).abs() < 3.0, "Es/N0 {}", b.es_n0_db);
        let d = rtframe::decode_burst(&b.soft, false).expect("the burst decodes");
        assert_eq!(d.burst, Burst::R { unit });
    }

    #[test]
    fn noise_alone_gives_no_burst() {
        let mut seed = 4;
        let noise: Vec<Complex32> = (0..200_000)
            .map(|_| Complex32::new(gauss(&mut seed), gauss(&mut seed)))
            .collect();
        let mut rx = BurstReceiver::new(9600.0, 1200);
        let mut out = Vec::new();
        rx.process(&noise, &mut out);
        assert!(out.is_empty());
    }
}
