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
    /// within `search_hz` of the centre. Strongest first.
    pub fn find(&self, rates: &[u32], search_hz: f64) -> Vec<Found> {
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
            let mut best: Option<(f32, f64)> = None;
            for k in -steps..=steps {
                let twice = k as f64 * bin_hz; // candidate for 2 fc
                let weaker = at(twice - rb / 2.0).min(at(twice + rb / 2.0));
                if best.is_none_or(|(b, _)| weaker > b) {
                    best = Some((weaker, twice / 2.0));
                }
            }
            if let Some((weaker, offset_hz)) = best {
                found.push(Found {
                    bit_rate,
                    offset_hz,
                    strength: (weaker / median).sqrt(),
                });
            }
        }
        found.sort_by(|a, b| b.strength.total_cmp(&a.strength));
        found
    }
}

/// Look for the line pair of each rate in `rates`. `samples` are complex
/// baseband at `rate` Hz; the carrier may sit anywhere within `search_hz` of
/// the centre. Returns the finds, strongest first.
pub fn find(samples: &[Complex32], rate: f64, rates: &[u32], search_hz: f64) -> Vec<Found> {
    // FFT length: about 0.3 Hz bins, but no longer than the data.
    let mut n = 1usize << 12;
    while (n * 2) <= samples.len() && rate / (n as f64) > 0.3 {
        n *= 2;
    }
    if samples.len() < n {
        return Vec::new();
    }
    let mut spectrum = LineSpectrum::new(rate, n);
    // Half-overlapped segments, powers added.
    let mut start = 0;
    while start + n <= samples.len() {
        spectrum.add(&samples[start..start + n]);
        start += n / 2;
    }
    spectrum.find(rates, search_hz)
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
