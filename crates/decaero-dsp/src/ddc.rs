// made by CasualArclamp
//! One channel's down-converter: an oscillator, then a three-stage CIC
//! decimator (three running sums over the decimation factor). It costs a few
//! additions per input sample, so twenty channels off one wide recording are
//! cheap, and its nulls sit on the frequencies that would alias back in.

use crate::simd::{self, MIX};
use num_complex::Complex32;

/// Scale to integers before the CIC so its running sums are exact.
const SCALE: f32 = 32768.0;

pub struct Ddc {
    in_rate: f64,
    decim: usize,
    phase: f64,
    step: f64,
    /// The oscillator's turn after 0, 1, 2 ... samples: the multiply is done
    /// a table's length at a time (see `simd::mix`), with the oscillator's
    /// own position taken afresh in double precision for each.
    table: [Complex32; MIX],
    /// Input left over from the last call, less than a table's length.
    pending: Vec<Complex32>,
    /// The three integrators (wrapping arithmetic is what makes a CIC work).
    integ: [[i64; 2]; 3],
    /// The three combs' previous values at the output rate.
    comb: [[i64; 2]; 3],
    count: usize,
    gain: f32,
}

fn table_for(step: f64) -> [Complex32; MIX] {
    std::array::from_fn(|k| {
        let (s, c) = (step * k as f64).sin_cos();
        Complex32::new(c as f32, s as f32)
    })
}

impl Ddc {
    /// Shift `offset_hz` to zero and decimate by `decim`.
    pub fn new(in_rate: f64, offset_hz: f64, decim: usize) -> Self {
        let decim = decim.max(1);
        let step = -std::f64::consts::TAU * offset_hz / in_rate;
        Self {
            in_rate,
            decim,
            phase: 0.0,
            step,
            table: table_for(step),
            pending: Vec::with_capacity(MIX),
            integ: [[0; 2]; 3],
            comb: [[0; 2]; 3],
            count: 0,
            gain: 1.0 / (SCALE * (decim as f32).powi(3)),
        }
    }

    /// The decimation that gives about `samples_per_bit` at `bit_rate`.
    pub fn decimation_for(in_rate: f64, bit_rate: f64, samples_per_bit: f64) -> usize {
        ((in_rate / (bit_rate * samples_per_bit)).round() as usize).max(1)
    }

    pub fn out_rate(&self) -> f64 {
        self.in_rate / self.decim as f64
    }

    pub fn set_offset(&mut self, offset_hz: f64) {
        self.step = -std::f64::consts::TAU * offset_hz / self.in_rate;
        self.table = table_for(self.step);
    }

    /// One table's length of input: mix, then the CIC.
    fn chunk(&mut self, x: &[Complex32; MIX], out: &mut Vec<Complex32>) {
        let (s, c) = self.phase.sin_cos();
        self.phase = (self.phase + self.step * MIX as f64).rem_euclid(std::f64::consts::TAU);
        let mut mixed = [Complex32::default(); MIX];
        simd::mix(
            x,
            Complex32::new(c as f32, s as f32),
            &self.table,
            &mut mixed,
        );
        for m in mixed {
            let mut v = [(m.re * SCALE) as i64, (m.im * SCALE) as i64];
            for stage in &mut self.integ {
                stage[0] = stage[0].wrapping_add(v[0]);
                stage[1] = stage[1].wrapping_add(v[1]);
                v = *stage;
            }
            self.count += 1;
            if self.count == self.decim {
                self.count = 0;
                for stage in &mut self.comb {
                    let prev = *stage;
                    *stage = v;
                    v = [v[0].wrapping_sub(prev[0]), v[1].wrapping_sub(prev[1])];
                }
                out.push(Complex32::new(
                    v[0] as f32 * self.gain,
                    v[1] as f32 * self.gain,
                ));
            }
        }
    }

    /// Input in, decimated baseband appended to `out`. Up to a table's
    /// length of input (63 samples) waits for the next call.
    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        let mut input = input;
        if !self.pending.is_empty() {
            let take = (MIX - self.pending.len()).min(input.len());
            self.pending.extend_from_slice(&input[..take]);
            input = &input[take..];
            if self.pending.len() < MIX {
                return;
            }
            let block: [Complex32; MIX] = self.pending[..].try_into().expect("a full table");
            self.pending.clear();
            self.chunk(&block, out);
        }
        let (blocks, rest) = input.as_chunks::<MIX>();
        for block in blocks {
            self.chunk(block, out);
        }
        self.pending.extend_from_slice(rest);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shifts_a_tone_to_zero_and_keeps_its_level() {
        let fs = 625_000.0;
        let tone: Vec<Complex32> = (0..200_000)
            .map(|n| {
                let p = std::f64::consts::TAU * 50_000.0 * n as f64 / fs;
                Complex32::new(p.cos() as f32, p.sin() as f32)
            })
            .collect();
        let mut ddc = Ddc::new(fs, 50_000.0, 130);
        let mut out = Vec::new();
        ddc.process(&tone, &mut out);
        let tail = &out[out.len() / 2..];
        let mean = tail.iter().sum::<Complex32>() / tail.len() as f32;
        assert!((mean.norm() - 1.0).abs() < 0.01, "level {}", mean.norm());
        // A tone one channel (5 kHz) away is pushed well down.
        let mut ddc = Ddc::new(fs, 55_000.0, 130);
        out.clear();
        ddc.process(&tone, &mut out);
        let power = out[out.len() / 2..]
            .iter()
            .map(|v| v.norm_sqr())
            .sum::<f32>()
            / (out.len() / 2) as f32;
        assert!(power < 1e-3, "leak {power}");
    }
}
