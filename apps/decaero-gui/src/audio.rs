// made by CasualArclamp
//! Sound out: the listened call's 8 kHz audio brought up to the sound card's
//! rate. The card's callback pulls samples from the engine's tap; when there
//! are none it plays silence.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use decaero_engine::AudioTap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

struct Shared {
    tap: Mutex<Option<AudioTap>>,
    /// Volume as the bits of an f32, so the callback reads it without a lock.
    volume: AtomicU32,
}

pub struct Output {
    shared: Arc<Shared>,
    /// Kept alive: dropping the stream stops the sound.
    _stream: cpal::Stream,
}

impl Output {
    pub fn start() -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no sound output device")?;
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let (rate, channels, format) = (
            config.sample_rate(),
            config.channels() as usize,
            config.sample_format(),
        );
        let shared = Arc::new(Shared {
            tap: Mutex::new(None),
            volume: AtomicU32::new(0.7f32.to_bits()),
        });
        let config: cpal::StreamConfig = config.into();
        let err = |e| eprintln!("audio output: {e}");
        let stream = match format {
            cpal::SampleFormat::F32 => device.build_output_stream(
                config,
                callback::<f32>(shared.clone(), channels, rate),
                err,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_output_stream(
                config,
                callback::<i16>(shared.clone(), channels, rate),
                err,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_output_stream(
                config,
                callback::<u16>(shared.clone(), channels, rate),
                err,
                None,
            ),
            f => return Err(format!("sound device format {f} is not handled")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Self {
            shared,
            _stream: stream,
        })
    }

    pub fn attach(&mut self, tap: AudioTap) {
        *self.shared.tap.lock().unwrap() = Some(tap);
    }

    pub fn set_volume(&self, v: f32) {
        self.shared.volume.store(v.to_bits(), Ordering::Relaxed);
    }
}

/// Taps of the interpolation filter (8 kHz samples it reaches over).
const TAPS: usize = 96;
/// Positions between two 8 kHz samples the filter is tabulated at.
const PHASES: usize = 128;

/// 8 kHz up to the card's rate through a sharp low-pass: a Kaiser-windowed
/// sinc of 96 taps, flat to about 3.6 kHz and 60 dB down from about
/// 3.95 kHz, so nothing of the 8 kHz sampling's images (the speech mirrored
/// about 4 kHz, which linear interpolation lets through 8 to 13 dB down and
/// is heard as a metallic edge) reaches the card. Tabulated at 128 places
/// between two samples, the two nearest blended.
struct Upsampler {
    /// `(PHASES + 1) × TAPS` filter values.
    table: Vec<f32>,
    /// The last `TAPS` samples, twice over so a run of them is one slice.
    past: [f32; 2 * TAPS],
    at: usize,
}

impl Upsampler {
    fn new() -> Self {
        // Kaiser window, beta 5.65 (60 dB); the modified Bessel function by
        // its series.
        let bessel = |x: f64| {
            let (mut sum, mut term) = (1.0, 1.0);
            for k in 1..30 {
                term *= (x / (2.0 * k as f64)).powi(2);
                sum += term;
            }
            sum
        };
        let beta = 5.65;
        // Cutoff (the −6 dB point) at 3.8 kHz: 0.475 of the 8 kHz rate.
        let fc = 0.475;
        let half = TAPS as f64 / 2.0;
        let mut table = vec![0f32; (PHASES + 1) * TAPS];
        for phase in 0..=PHASES {
            let frac = phase as f64 / PHASES as f64;
            let row = &mut table[phase * TAPS..(phase + 1) * TAPS];
            let mut sum = 0.0;
            for (k, v) in row.iter_mut().enumerate() {
                // Tap k weighs the sample k places after the oldest; the
                // output instant is `frac` past the sample at half − 1.
                let t = k as f64 - (half - 1.0) - frac;
                let x = std::f64::consts::PI * 2.0 * fc * t;
                let sinc = if x.abs() < 1e-9 { 1.0 } else { x.sin() / x };
                let r = (t / half).clamp(-1.0, 1.0);
                let w = bessel(beta * (1.0 - r * r).sqrt()) / bessel(beta);
                let h = 2.0 * fc * sinc * w;
                *v = h as f32;
                sum += h;
            }
            // Unity gain at DC at every place.
            for v in row.iter_mut() {
                *v /= sum as f32;
            }
        }
        Self {
            table,
            past: [0.0; 2 * TAPS],
            at: 0,
        }
    }

    /// The next 8 kHz sample.
    fn push(&mut self, sample: f32) {
        self.past[self.at] = sample;
        self.past[self.at + TAPS] = sample;
        self.at = (self.at + 1) % TAPS;
    }

    /// The sound `frac` (0 to 1) of a sample past the middle of what has
    /// been pushed: 6 ms behind the newest.
    fn at(&self, frac: f32) -> f32 {
        let place = frac.clamp(0.0, 1.0) * PHASES as f32;
        let p = (place as usize).min(PHASES - 1);
        let blend = place - p as f32;
        let a = &self.table[p * TAPS..(p + 1) * TAPS];
        let b = &self.table[(p + 1) * TAPS..(p + 2) * TAPS];
        let x = &self.past[self.at..self.at + TAPS];
        let mut sum = 0f32;
        for k in 0..TAPS {
            sum += (a[k] + blend * (b[k] - a[k])) * x[k];
        }
        sum
    }
}

/// The card's callback: 8 kHz up to the card's rate through [`Upsampler`],
/// with a short wait for a cushion so it does not stutter at the start of
/// a call.
fn callback<T>(
    shared: Arc<Shared>,
    channels: usize,
    rate: u32,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) + Send + 'static
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let step = 8000.0 / rate as f32;
    let mut up = Upsampler::new();
    let mut frac = 0f32;
    let mut primed = false;
    move |data: &mut [T], _| {
        let volume = f32::from_bits(shared.volume.load(Ordering::Relaxed)).powi(2);
        let tap = shared.tap.lock().ok().and_then(|t| t.clone());
        if let Some(t) = &tap {
            // A quarter of a second in hand before starting.
            if !primed && t.buffered() >= 2000 {
                primed = true;
            }
        }
        for frame in data.chunks_mut(channels.max(1)) {
            frac += step;
            while frac >= 1.0 {
                frac -= 1.0;
                up.push(
                    match tap.as_ref().filter(|_| primed).and_then(AudioTap::pop) {
                        Some(s) => f32::from(s) / 32768.0,
                        None => {
                            primed = false;
                            0.0
                        }
                    },
                );
            }
            let v = up.at(frac) * volume;
            for s in frame {
                *s = T::from_sample(v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tone at `hz` sampled at 8 kHz, brought up to 48 kHz: its level
    /// there, and the level of its image at 8000 − hz, both against 1.
    fn tone(hz: f64) -> (f64, f64) {
        let mut up = Upsampler::new();
        let mut out = Vec::new();
        for n in 0..4000 {
            up.push((std::f64::consts::TAU * hz * n as f64 / 8000.0).sin() as f32);
            for k in 0..6 {
                out.push(f64::from(up.at(k as f32 / 6.0)));
            }
        }
        // Past the filter's start, a whole number of both tones' cycles.
        let part = &out[1200..1200 + 19_200];
        let level = |f: f64| {
            let (mut re, mut im) = (0.0, 0.0);
            for (n, v) in part.iter().enumerate() {
                let p = std::f64::consts::TAU * f * n as f64 / 48_000.0;
                re += v * p.cos();
                im += v * p.sin();
            }
            2.0 * re.hypot(im) / part.len() as f64
        };
        (level(hz), level(8000.0 - hz))
    }

    #[test]
    fn speech_passes_and_its_images_do_not() {
        for hz in [300.0, 1000.0, 2500.0, 3400.0] {
            let (kept, image) = tone(hz);
            assert!((kept - 1.0).abs() < 0.03, "{hz} Hz comes out at {kept}");
            assert!(image < 0.002, "{hz} Hz leaves an image at {image}");
        }
        // Just under 4 kHz the filter is closing: the image is well down.
        let (_, image) = tone(3900.0);
        assert!(image < 0.05, "3900 Hz leaves an image at {image}");
    }
}
