// made by CasualArclamp
//! A finer spectrum of the part of the band on screen.
//!
//! The band's own transform has bins of some tens of hertz. Zoomed in past
//! that, the view is mixed to zero and decimated, and a transform is taken
//! over a stretch of it that grows with the zoom (up to a second), so the
//! resolution follows the zoom smoothly. The
//! stretch is windowed at its own length and padded with zeros to a power of
//! two, so nothing jumps when the length passes one.
//!
//! The band's own rows carry on beside these: a `Zoom` being started or moved
//! never takes anything off the screen, it only adds detail once it has the
//! samples for it.

use decaero_dsp::ddc::Ddc;
use num_complex::Complex32;
use rustfft::FftPlanner;

/// The longest stretch transformed, seconds (so the finest bins are 1 Hz).
const LONGEST_S: f64 = 1.0;
/// The decimated rate is this many times the band covered: the CIC decimator
/// is flat to 0.7 dB over the middle quarter and what folds into it is 50 dB
/// down.
const OVERSAMPLE: f64 = 4.0;

/// One row (or the average) of the finer spectrum: `db[i]` is the level at
/// `lo_hz + i * step_hz` from the centre of the input.
#[derive(Debug, Clone)]
pub struct ZoomRow {
    pub lo_hz: f64,
    pub step_hz: f64,
    pub db: Vec<f32>,
}

pub struct Zoom {
    in_rate: f64,
    decim: usize,
    ddc: Ddc,
    centre_hz: f64,
    /// Width of the band covered, around `centre_hz`.
    cover_hz: f64,
    /// Decimated samples, the newest last.
    samples: Vec<Complex32>,
    planner: FftPlanner<f32>,
    window: Vec<f32>,
    buf: Vec<Complex32>,
    /// The running average, with the transform size it was taken at.
    average: Option<(usize, ZoomRow)>,
}

impl Zoom {
    /// A zoom on `lo_hz..hi_hz` (from the centre of the input), covering
    /// twice that so the view can be moved and widened a little without
    /// starting again. `None` when the input is too narrow to gain anything.
    pub fn new(in_rate: f64, lo_hz: f64, hi_hz: f64) -> Option<Self> {
        let width = hi_hz - lo_hz;
        let decim = (in_rate / (OVERSAMPLE * 2.0 * width)).floor() as usize;
        if decim < 2 {
            return None;
        }
        let centre_hz = (lo_hz + hi_hz) / 2.0;
        Some(Self {
            in_rate,
            decim,
            ddc: Ddc::new(in_rate, centre_hz, decim),
            centre_hz,
            cover_hz: in_rate / decim as f64 / OVERSAMPLE,
            samples: Vec::new(),
            planner: FftPlanner::new(),
            window: Vec::new(),
            buf: Vec::new(),
            average: None,
        })
    }

    fn out_rate(&self) -> f64 {
        self.in_rate / self.decim as f64
    }

    /// Does this zoom still serve the view `lo_hz..hi_hz`? It must cover it,
    /// and not be so much wider that the transform is mostly off screen.
    pub fn fits(&self, lo_hz: f64, hi_hz: f64) -> bool {
        lo_hz >= self.centre_hz - self.cover_hz / 2.0
            && hi_hz <= self.centre_hz + self.cover_hz / 2.0
            && hi_hz - lo_hz >= self.cover_hz / 4.0
    }

    /// A block of the input.
    pub fn feed(&mut self, block: &[Complex32]) {
        self.ddc.process(block, &mut self.samples);
        let keep = (self.out_rate() * (LONGEST_S + 0.1)) as usize;
        if self.samples.len() > 2 * keep {
            let cut = self.samples.len() - keep;
            self.samples.drain(..cut);
        }
    }

    /// The spectrum of the `seconds` up to `back_s` ago. `base_s` is the
    /// length of the band's own transform: a stretch no longer than that
    /// shows nothing new, so `None` until more has been gathered.
    pub fn row(&mut self, back_s: f64, seconds: f64, base_s: f64) -> Option<ZoomRow> {
        let rate = self.out_rate();
        let end = self
            .samples
            .len()
            .checked_sub((back_s * rate).round() as usize)?;
        let len = ((seconds.min(LONGEST_S) * rate).round() as usize).min(end);
        if len < 64 || (len as f64) < base_s * rate * 1.25 {
            return None;
        }
        let n = len.next_power_of_two();
        if self.window.len() != len {
            self.window = (0..len)
                .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / len as f32).cos())
                .collect();
        }
        self.buf.clear();
        self.buf.extend(
            self.samples[end - len..end]
                .iter()
                .zip(&self.window)
                .map(|(s, w)| s * w),
        );
        self.buf.resize(n, Complex32::default());
        self.planner.plan_fft_forward(n).process(&mut self.buf);

        // Levels as densities on the band's own scale: noise and modulated
        // carriers keep their colours whatever the resolution, and only a
        // steady tone stands higher the finer it is resolved.
        let scale = (1.0 / (0.375 * len as f64 * rate * base_s)) as f32;
        let step_hz = rate / n as f64;
        // The middle quarter of the transform is the band covered.
        let half = (n / (2 * OVERSAMPLE as usize)) as isize;
        let db: Vec<f32> = (-half..half)
            .map(|k| {
                let bin = k.rem_euclid(n as isize) as usize;
                // Undo the decimator's droop (three running sums: sinc cubed).
                let x = std::f64::consts::PI * k as f64 * step_hz / self.in_rate;
                let droop = if k == 0 {
                    1.0
                } else {
                    ((x * self.decim as f64).sin() / (self.decim as f64 * x.sin())).powi(6)
                };
                10.0 * (self.buf[bin].norm_sqr() * scale / droop as f32)
                    .max(1e-20)
                    .log10()
            })
            .collect();
        let row = ZoomRow {
            lo_hz: self.centre_hz - half as f64 * step_hz,
            step_hz,
            db,
        };
        match &mut self.average {
            Some((size, average)) if *size == n => {
                for (a, r) in average.db.iter_mut().zip(&row.db) {
                    *a += 0.027 * (r - *a);
                }
            }
            _ => self.average = Some((n, row.clone())),
        }
        Some(row)
    }

    /// The running average of the rows, for the spectrum trace.
    pub fn average(&self) -> Option<&ZoomRow> {
        self.average.as_ref().map(|(_, row)| row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tone and noise at 625 kS/s.
    fn signal(tone_hz: f64, seconds: f64) -> Vec<Complex32> {
        let rate = 625_000.0;
        let mut seed = 1u32;
        let mut noise = move || {
            // Twelve uniform draws make a fair bell curve.
            let mut sum = 0.0f32;
            for _ in 0..12 {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                sum += (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5;
            }
            sum
        };
        (0..(rate * seconds) as usize)
            .map(|i| {
                let (s, c) = (std::f64::consts::TAU * tone_hz * i as f64 / rate).sin_cos();
                Complex32::new(c as f32 * 0.05 + noise(), s as f32 * 0.05 + noise())
            })
            .collect()
    }

    #[test]
    fn resolves_finer_and_keeps_the_noise_level() {
        let rate = 625_000.0;
        let base_s = 32_768.0 / rate;
        // Two tones 6 Hz apart: one lump to the band's 19 Hz bins.
        let mut input = signal(-116_503.0, 1.3);
        for (i, x) in input.iter_mut().enumerate() {
            let (s, c) = (std::f64::consts::TAU * -116_497.0 * i as f64 / rate).sin_cos();
            *x += Complex32::new(c as f32, s as f32) * 0.05;
        }
        let mut zoom = Zoom::new(rate, -117_000.0, -116_000.0).unwrap();
        assert!(zoom.fits(-117_000.0, -116_000.0));
        assert!(zoom.fits(-117_300.0, -116_200.0));
        assert!(!zoom.fits(-116_200.0, -115_000.0) && !zoom.fits(-116_600.0, -116_400.0));
        // Nothing until there is more to transform than the band's own row.
        zoom.feed(&input[..20_000]);
        assert!(zoom.row(0.0, 1.0, base_s).is_none());
        for block in input[20_000..].chunks(31_250) {
            zoom.feed(block);
        }
        let row = zoom.row(0.0, 1.0, base_s).unwrap();
        assert!(row.step_hz < 1.0, "{}", row.step_hz);
        let at = |hz: f64| row.db[((hz - row.lo_hz) / row.step_hz).round() as usize];
        let (a, b, between) = (at(-116_503.0), at(-116_497.0), at(-116_500.0));
        assert!(
            a > between + 15.0 && b > between + 15.0,
            "{a} {between} {b}"
        );
        // The noise sits where the band's own transform has it: a power of
        // 2 (two parts of variance 1) over 32768 bins is -42.1 dB a bin;
        // averaging decibels reads 2.5 dB under that.
        let far: Vec<f32> = row
            .db
            .iter()
            .enumerate()
            .filter(|(i, _)| (row.lo_hz + *i as f64 * row.step_hz + 116_500.0).abs() > 100.0)
            .map(|(_, v)| *v)
            .collect();
        let mean = far.iter().sum::<f32>() / far.len() as f32;
        assert!((mean + 44.6).abs() < 0.7, "{mean}");
        // The same at both ends of the band covered: the droop is undone.
        let edge = far[..200].iter().sum::<f32>() / 200.0;
        let middle = far[far.len() / 2 - 100..far.len() / 2 + 100]
            .iter()
            .sum::<f32>()
            / 200.0;
        assert!((edge - middle).abs() < 1.2, "{edge} {middle}");
        assert!(zoom.average().is_some());
    }

    #[test]
    fn no_zoom_on_a_narrow_input() {
        assert!(Zoom::new(48_000.0, 0.0, 10_000.0).is_none());
        assert!(Zoom::new(48_000.0, 1000.0, 2000.0).is_some());
    }
}
