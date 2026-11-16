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
    let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
    let window: Vec<f32> = (0..n)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
        .collect();
    let mut power = vec![0f32; n];
    let mut buf = vec![Complex32::default(); n];
    // Half-overlapped segments, powers added.
    let mut start = 0;
    while start + n <= samples.len() {
        for ((b, &s), &w) in buf.iter_mut().zip(&samples[start..start + n]).zip(&window) {
            *b = s * s * w;
        }
        fft.process(&mut buf);
        for (p, b) in power.iter_mut().zip(&buf) {
            *p += b.norm_sqr();
        }
        start += n / 2;
    }
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
