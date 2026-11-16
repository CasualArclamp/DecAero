// made by CasualArclamp
//! One channel's down-converter: an oscillator, then a three-stage CIC
//! decimator (three running sums over the decimation factor). It costs a few
//! additions per input sample, so twenty channels off one wide recording are
//! cheap, and its nulls sit on the frequencies that would alias back in.

use num_complex::Complex32;

/// Scale to integers before the CIC so its running sums are exact.
const SCALE: f32 = 32768.0;

pub struct Ddc {
    in_rate: f64,
    decim: usize,
    phase: f64,
    step: f64,
    /// The three integrators (wrapping arithmetic is what makes a CIC work).
    integ: [[i64; 2]; 3],
    /// The three combs' previous values at the output rate.
    comb: [[i64; 2]; 3],
    count: usize,
    gain: f32,
}

impl Ddc {
    /// Shift `offset_hz` to zero and decimate by `decim`.
    pub fn new(in_rate: f64, offset_hz: f64, decim: usize) -> Self {
        let decim = decim.max(1);
        Self {
            in_rate,
            decim,
            phase: 0.0,
            step: -std::f64::consts::TAU * offset_hz / in_rate,
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
    }

    pub fn process(&mut self, input: &[Complex32], out: &mut Vec<Complex32>) {
        for &x in input {
            let (s, c) = self.phase.sin_cos();
            self.phase += self.step;
            if self.phase > std::f64::consts::PI {
                self.phase -= std::f64::consts::TAU;
            } else if self.phase < -std::f64::consts::PI {
                self.phase += std::f64::consts::TAU;
            }
            let (c, s) = (c as f32, s as f32);
            let mut v = [
                ((x.re * c - x.im * s) * SCALE) as i64,
                ((x.re * s + x.im * c) * SCALE) as i64,
            ];
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
