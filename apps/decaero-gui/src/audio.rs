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

/// The card's callback: linear interpolation from 8 kHz, which is enough for
/// a 4 kHz voice codec, with a short wait for a cushion so it does not
/// stutter at the start of a call.
fn callback<T>(
    shared: Arc<Shared>,
    channels: usize,
    rate: u32,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo) + Send + 'static
where
    T: cpal::SizedSample + cpal::FromSample<f32>,
{
    let step = 8000.0 / rate as f32;
    let (mut prev, mut next, mut frac) = (0f32, 0f32, 0f32);
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
                prev = next;
                next = match tap.as_ref().filter(|_| primed).and_then(AudioTap::pop) {
                    Some(s) => f32::from(s) / 32768.0,
                    None => {
                        primed = false;
                        0.0
                    }
                };
            }
            let v = (prev + (next - prev) * frac) * volume;
            for s in frame {
                *s = T::from_sample(v);
            }
        }
    }
}
