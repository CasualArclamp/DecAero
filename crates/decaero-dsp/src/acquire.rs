// made by CasualArclamp
//! Finding an Aero carrier and naming its mode without any loop.
//!
//! Both Aero modulations are offset schemes (A-BPSK behaves as MSK, A-QPSK is
//! offset QPSK), and squaring either gives two spectral lines at
//! `2 fc - Rb/2` and `2 fc + Rb/2`. Their spacing is the bit rate and their
//! mean is twice the carrier. Averaging the squared spectrum over seconds
//! pulls those lines out of a signal far too weak to see a constellation,
//! which is what the voice-follow mode leans on.

use num_complex::Complex32;
use rustfft::FftPlanner;

/// What the squared spectrum says about a channel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Found {
    /// The bit rate whose line pair was found.
    pub bit_rate: u32,
    /// Carrier offset from the centre of the samples given, in Hz.
    pub offset_hz: f64,
    /// The weaker line over the median of the squared spectrum (a ratio; 8
    /// and above is a safe find).
    pub strength: f32,
}

/// The squared spectrum, built up a segment at a time so a long look can be
/// averaged without keeping the samples.
pub struct LineSpectrum {
    rate: f64,
    n: usize,
    fft: std::sync::Arc<dyn rustfft::Fft<f32>>,
    window: Vec<f32>,
    buf: Vec<Complex32>,
    power: Vec<f32>,
    segments: f32,
}

impl LineSpectrum {
    /// `n` samples a segment (a power of two); bins are `rate / n` Hz wide.
    pub fn new(rate: f64, n: usize) -> Self {
        Self {
            rate,
            n,
            fft: FftPlanner::<f32>::new().plan_fft_forward(n),
            window: (0..n)
                .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
                .collect(),
            buf: vec![Complex32::default(); n],
            power: vec![0.0; n],
            segments: 0.0,
        }
    }

    pub fn segment_len(&self) -> usize {
        self.n
    }
    /// Segments averaged so far (less than a whole number once old ones fade).
    pub fn segments(&self) -> f32 {
        self.segments
    }

    /// Let what is already there count for `keep` (0 to 1) of what it did:
    /// makes the average slide.
    pub fn fade(&mut self, keep: f32) {
        for p in &mut self.power {
            *p *= keep;
        }
        self.segments *= keep;
    }

    /// Add one segment of exactly `segment_len` samples.
    pub fn add(&mut self, samples: &[Complex32]) {
        for ((b, &s), &w) in self.buf.iter_mut().zip(samples).zip(&self.window) {
            *b = s * s * w;
        }
        self.fft.process(&mut self.buf);
        for (p, b) in self.power.iter_mut().zip(&self.buf) {
            *p += b.norm_sqr();
        }
        self.segments += 1.0;
    }

    /// Look for the line pair of each rate in `rates`, with the carrier
    /// within `search_hz` of the centre. Strongest first, one find a rate.
    pub fn find(&self, rates: &[u32], search_hz: f64) -> Vec<Found> {
        self.peaks(rates, search_hz, 1)
    }

    /// Every line pair within `search_hz` of the centre, not just the best of
    /// each rate: the peaks of the line-pair score, a few a rate, strongest
    /// first. Two voice carriers 5 kHz apart can both be within reach.
    ///
    /// Peaks of one rate are at least 0.4 of the bit rate apart: closer than
    /// that, both of a candidate's lines would sit in the continuous part of
    /// a strong carrier's squared spectrum, and the peak is that carrier's.
    pub fn find_all(&self, rates: &[u32], search_hz: f64) -> Vec<Found> {
        self.peaks(rates, search_hz, 4)
    }

    /// Up to `per_rate` peaks of each rate's line-pair score, strongest first.
    fn peaks(&self, rates: &[u32], search_hz: f64, per_rate: usize) -> Vec<Found> {
        let (n, rate, power) = (self.n, self.rate, &self.power);
        let mut sorted = power.clone();
        sorted.sort_by(f32::total_cmp);
        let median = sorted[n / 2].max(f32::MIN_POSITIVE);
        let bin_hz = rate / n as f64;
        let at = |hz: f64| -> f32 {
            // The squared spectrum repeats every `rate`; take the strongest of
            // the three bins around the frequency.
            let k = (hz / bin_hz).round() as i64;
            (-1..=1)
                .map(|d| power[(k + d).rem_euclid(n as i64) as usize])
                .fold(0.0, f32::max)
        };
        let mut found = Vec::new();
        for &bit_rate in rates {
            let rb = f64::from(bit_rate);
            if rb >= rate {
                continue; // the two lines would fold onto each other
            }
            let steps = (2.0 * search_hz / bin_hz).ceil() as i64;
            // Each candidate for 2 fc scores its weaker line.
            let mut scores: Vec<(f32, f64)> = (-steps..=steps)
                .map(|k| {
                    let twice = k as f64 * bin_hz;
                    (at(twice - rb / 2.0).min(at(twice + rb / 2.0)), twice / 2.0)
                })
                .collect();
            // The strongest, then the strongest clear of it, and so on.
            let apart = (0.4 * rb).max(200.0);
            let mut peaks: Vec<(f32, f64)> = Vec::new();
            while peaks.len() < per_rate {
                let Some(&best) = scores.iter().max_by(|a, b| a.0.total_cmp(&b.0)) else {
                    break;
                };
                if best.0 < 0.0 {
                    break;
                }
                peaks.push(best);
                for s in &mut scores {
                    if (s.1 - best.1).abs() < apart {
                        s.0 = -1.0;
                    }
                }
            }
            found.extend(peaks.into_iter().map(|(weaker, offset_hz)| Found {
                bit_rate,
                offset_hz,
                strength: (weaker / median).sqrt(),
            }));
        }
        found.sort_by(|a, b| b.strength.total_cmp(&a.strength));
        found
    }
}

/// Look for the line pair of each rate in `rates`. `samples` are complex
/// baseband at `rate` Hz; the carrier may sit anywhere within `search_hz` of
/// the centre. Returns the finds, strongest first.
pub fn find(samples: &[Complex32], rate: f64, rates: &[u32], search_hz: f64) -> Vec<Found> {
    squared_spectrum(samples, rate).map_or_else(Vec::new, |s| s.find(rates, search_hz))
}

/// [`find`], but every line pair found (see [`LineSpectrum::find_all`]).
pub fn find_all(samples: &[Complex32], rate: f64, rates: &[u32], search_hz: f64) -> Vec<Found> {
    squared_spectrum(samples, rate).map_or_else(Vec::new, |s| s.find_all(rates, search_hz))
}

/// The squared spectrum of all of `samples`, or `None` if they are too few.
fn squared_spectrum(samples: &[Complex32], rate: f64) -> Option<LineSpectrum> {
    // FFT length: about 0.3 Hz bins, but no longer than the data.
    let mut n = 1usize << 12;
    while (n * 2) <= samples.len() && rate / (n as f64) > 0.3 {
        n *= 2;
    }
    if samples.len() < n {
        return None;
    }
    let mut spectrum = LineSpectrum::new(rate, n);
    // Half-overlapped segments, powers added.
    let mut start = 0;
    while start + n <= samples.len() {
        spectrum.add(&samples[start..start + n]);
        start += n / 2;
    }
    Some(spectrum)
}

/// What [`find_bpsk`] says about a plain BPSK carrier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FoundBpsk {
    /// The candidate symbol rate whose clock line was found.
    pub symbol_rate: f64,
    /// Carrier offset from the centre of the samples given, in Hz.
    pub offset_hz: f64,
    /// The squared signal's carrier line over the median of its spectrum
    /// (as an amplitude ratio, like [`Found::strength`]).
    pub carrier_strength: f32,
    /// The envelope's clock line over the median of its spectrum.
    pub clock_strength: f32,
}

/// Find a plain BPSK carrier and name its symbol rate, again without a loop.
///
/// Squaring plain BPSK takes the modulation off and leaves one line at
/// `2 fc` (not the pair an Aero carrier gives, which is how the two are told
/// apart). The envelope `|x|^2` of a band-limited BPSK signal carries a line
/// at the symbol rate. `symbol_rates` are the candidates; of those whose line
/// stands within 6 dB of the strongest, the slowest is taken, because a
/// carrier's envelope can also show a line at twice its own rate.
pub fn find_bpsk(
    samples: &[Complex32],
    rate: f64,
    symbol_rates: &[f64],
    search_hz: f64,
) -> Option<FoundBpsk> {
    let mut n = 1usize << 12;
    while (n * 2) <= samples.len() && rate / (n as f64) > 0.3 {
        n *= 2;
    }
    if samples.len() < n {
        return None;
    }
    let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
    let window: Vec<f32> = (0..n)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
        .collect();
    let mut squared = vec![0f32; n];
    let mut envelope = vec![0f32; n];
    let mut buf = vec![Complex32::default(); n];
    let mut start = 0;
    while start + n <= samples.len() {
        let seg = &samples[start..start + n];
        for ((b, &s), &w) in buf.iter_mut().zip(seg).zip(&window) {
            *b = s * s * w;
        }
        fft.process(&mut buf);
        for (p, b) in squared.iter_mut().zip(&buf) {
            *p += b.norm_sqr();
        }
        let mean = seg.iter().map(|s| s.norm_sqr()).sum::<f32>() / n as f32;
        for ((b, &s), &w) in buf.iter_mut().zip(seg).zip(&window) {
            *b = Complex32::new((s.norm_sqr() - mean) * w, 0.0);
        }
        fft.process(&mut buf);
        for (p, b) in envelope.iter_mut().zip(&buf) {
            *p += b.norm_sqr();
        }
        start += n / 2;
    }
    let median = |v: &[f32]| {
        let mut sorted = v.to_vec();
        sorted.sort_by(f32::total_cmp);
        sorted[sorted.len() / 2].max(f32::MIN_POSITIVE)
    };
    let bin_hz = rate / n as f64;
    // The carrier: the strongest squared bin within reach.
    let steps = (2.0 * search_hz / bin_hz).ceil() as i64;
    let (mut peak, mut at) = (0f32, 0i64);
    for k in -steps..=steps {
        let p = squared[k.rem_euclid(n as i64) as usize];
        if p > peak {
            (peak, at) = (p, k);
        }
    }
    // A fraction of a bin from the neighbours (parabola through three bins).
    let bin = |k: i64| squared[k.rem_euclid(n as i64) as usize];
    let (l, m, r) = (bin(at - 1), bin(at), bin(at + 1));
    let frac = if (l - 2.0 * m + r).abs() > 0.0 {
        0.5 * (l - r) / (l - 2.0 * m + r)
    } else {
        0.0
    };
    let offset_hz = (at as f64 + f64::from(frac)) * bin_hz / 2.0;
    let carrier_strength = (peak / median(&squared)).sqrt();
    // The clock line of each candidate (positive frequencies only).
    let env_median = median(&envelope[1..n / 2]);
    let lines: Vec<(f64, f32)> = symbol_rates
        .iter()
        .filter(|&&rs| rs < rate / 2.0)
        .map(|&rs| {
            let k = (rs / bin_hz).round() as usize;
            let p = (k.saturating_sub(1)..=k + 1)
                .map(|i| envelope[i.min(n - 1)])
                .fold(0.0, f32::max);
            (rs, (p / env_median).sqrt())
        })
        .collect();
    let strongest = lines.iter().map(|l| l.1).fold(0.0, f32::max);
    let (symbol_rate, clock_strength) = lines
        .iter()
        .filter(|l| l.1 >= 0.5 * strongest)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .copied()?;
    Some(FoundBpsk {
        symbol_rate,
        offset_hz,
        carrier_strength,
        clock_strength,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::{PI, TAU};

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

    /// MSK (offset QPSK with half-sine pulses) at `offset_hz`, amplitude
    /// `amp`, random bits from `seed`.
    fn msk(
        n: usize,
        sps: usize,
        rate: f64,
        offset_hz: f64,
        amp: f32,
        mut seed: u32,
    ) -> Vec<Complex32> {
        let mut phase = 0.3f64;
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            let sign = if lcg(&mut seed) >> 31 == 1 { 1.0 } else { -1.0 };
            for _ in 0..sps {
                phase += sign * (PI / 2.0) / sps as f64 + TAU * offset_hz / rate;
                out.push(Complex32::new(phase.cos() as f32, phase.sin() as f32) * amp);
            }
        }
        out.truncate(n);
        out
    }

    /// Two voice carriers 5 kHz apart, both within reach of one look (as in
    /// a deep scan channel): `find` names only the stronger, `find_all`
    /// both, and nothing between them.
    #[test]
    fn two_voice_carriers_in_one_reach_are_both_found() {
        let (sps, bit_rate) = (6usize, 8400u32);
        let rate = f64::from(bit_rate) * sps as f64;
        let n = (3.0 * rate) as usize;
        let strong = msk(n, sps, rate, -2500.0, 2.0, 11);
        let weak = msk(n, sps, rate, 2500.0, 1.0, 22);
        let mut seed = 5;
        let x: Vec<Complex32> = strong
            .iter()
            .zip(&weak)
            .map(|(a, b)| a + b + Complex32::new(gauss(&mut seed), gauss(&mut seed)) * 1.5)
            .collect();
        let best = find(&x, rate, &[bit_rate], 5000.0);
        assert_eq!(best.len(), 1);
        assert!((best[0].offset_hz + 2500.0).abs() < 5.0, "{best:?}");
        let all = find_all(&x, rate, &[bit_rate], 5000.0);
        let near = |hz: f64| all.iter().find(|f| (f.offset_hz - hz).abs() < 5.0);
        assert!(near(-2500.0).is_some_and(|f| f.strength > 8.0), "{all:?}");
        assert!(near(2500.0).is_some_and(|f| f.strength > 8.0), "{all:?}");
        // The rest are noise, well under both.
        let third = all
            .iter()
            .filter(|f| (f.offset_hz.abs() - 2500.0).abs() > 5.0)
            .map(|f| f.strength)
            .fold(0.0, f32::max);
        assert!(third < 4.0, "{all:?}");
    }
}
