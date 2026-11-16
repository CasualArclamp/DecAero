// made by CasualArclamp
//! The deep scan: finds carriers too weak to stand out of the spectrum.
//!
//! A short windowed FFT, hopped a quarter of its length, cuts the band into
//! overlapping channels about 15 kHz wide at one go (far cheaper than a
//! down-converter each). Every channel's output is squared and its spectrum
//! averaged; an Aero carrier leaves its line pair there (see
//! `decaero_dsp::acquire`) even a couple of dB under the noise in its own
//! bandwidth. That is what lets voice follow pick up a call the waterfall
//! barely hints at.

use crate::channel::Event;
use decaero_dsp::acquire::LineSpectrum;
use num_complex::Complex32;
use rustfft::FftPlanner;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};

/// Samples in a segment of a channel's squared spectrum (about 1.7 s).
const SEGMENT: usize = 1 << 16;
/// Segments between looks, and how much of the old average each look keeps:
/// the average covers roughly the last fifteen seconds.
const SEGMENTS_PER_LOOK: u32 = 3;
const KEEP: f32 = 0.6;
/// Line strength (over the median of the squared spectrum) to report, once
/// several segments are averaged, and for the first look.
const MIN_STRENGTH: f32 = 3.6;
const FIRST_STRENGTH: f32 = 5.0;
/// Channels scanned on each side of the centre. A radio running at several
/// MS/s has hundreds of channels and each keeps a spectrum in memory: the
/// scan then covers the middle 1.5 MHz or so, which is more than the Aero
/// channels of one beam take.
const EACH_SIDE: usize = 80;

/// Length of the filter bank's transform for an input at `in_rate`.
fn bank_size(in_rate: f64) -> usize {
    // Channels about 9.8 kHz apart: each takes in a 10500 bit/s carrier
    // wherever it sits between two of them.
    ((in_rate / 9766.0).round() as usize)
        .next_power_of_two()
        .max(8)
}

/// A strong carrier leaks a little into every channel of the bank, and there
/// it is seen again, moved by a whole number of these steps (half a channel's
/// sample rate, since the squaring doubles every frequency). A find that far
/// from a carrier already known, at the same bit rate, is that carrier.
pub fn alias_step(in_rate: f64) -> f64 {
    let n = bank_size(in_rate);
    in_rate / (n / 4) as f64 / 2.0
}

pub fn run(
    in_rate: f64,
    blocks: Receiver<Arc<Vec<Complex32>>>,
    events: Sender<Event>,
    stop: Arc<AtomicBool>,
) {
    let n = bank_size(in_rate);
    let hop = n / 4;
    let out_rate = in_rate / hop as f64;
    let spacing = in_rate / n as f64;
    let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
    let window: Vec<f32> = (0..n)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
        .collect();
    // Channel k is k steps above the centre, or n - k below it.
    let scanned: Vec<usize> = (0..n).filter(|&k| k.min(n - k) <= EACH_SIDE).collect();
    let mut pending: Vec<Complex32> = Vec::new();
    let mut frame = vec![Complex32::default(); n];
    let mut hops = 0u64;
    let mut segments: Vec<Vec<Complex32>> = (0..scanned.len())
        .map(|_| Vec::with_capacity(SEGMENT))
        .collect();
    let mut spectra: Vec<LineSpectrum> = (0..scanned.len())
        .map(|_| LineSpectrum::new(out_rate, SEGMENT))
        .collect();
    let mut since_look = 0;
    let mut first = true;

    while let Ok(block) = blocks.recv() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        pending.extend_from_slice(&block);
        let mut at = 0;
        while at + n <= pending.len() {
            for ((f, &x), &w) in frame.iter_mut().zip(&pending[at..at + n]).zip(&window) {
                *f = x * w;
            }
            fft.process(&mut frame);
            // A tone on channel k turns k quarter circles from one hop to
            // the next: take that out so each channel is at baseband.
            for (seg, &k) in segments.iter_mut().zip(&scanned) {
                let v = frame[k];
                let turned = match (k as u64 * hops) & 3 {
                    0 => v,
                    1 => Complex32::new(v.im, -v.re),
                    2 => -v,
                    _ => Complex32::new(-v.im, v.re),
                };
                seg.push(turned);
            }
            hops += 1;
            at += hop;
        }
        pending.drain(..at);
        if segments[0].len() < SEGMENT {
            continue;
        }
        for (spectrum, seg) in spectra.iter_mut().zip(&mut segments) {
            spectrum.add(seg);
            seg.clear();
        }
        since_look += 1;
        if since_look < SEGMENTS_PER_LOOK {
            continue;
        }
        since_look = 0;
        let threshold = if first { FIRST_STRENGTH } else { MIN_STRENGTH };
        first = false;
        for (spectrum, &k) in spectra.iter_mut().zip(&scanned) {
            let centre = if k < n / 2 {
                k as f64
            } else {
                k as f64 - n as f64
            } * spacing;
            let found = spectrum.find(&[8400, 10500, 1200, 600], spacing / 2.0 + 500.0);
            spectrum.fade(KEEP);
            if std::env::var("AERO_SCAN_TRACE").is_ok()
                && found.first().is_some_and(|f| f.strength > 2.8)
            {
                eprintln!(
                    "scan ch {k:2} centre {centre:9.0} segs {:.1} {:?}",
                    spectrum.segments(),
                    &found[..2.min(found.len())]
                );
            }
            // The strongest pair, if it stands clear of the others (a strong
            // carrier leaves weaker ghosts at the other rates' spacings).
            let Some(best) = found.first() else { continue };
            let clear = found
                .get(1)
                .is_none_or(|second| best.strength > 1.6 * second.strength);
            // The slow rates are narrow and strong enough for the spectrum
            // survey; here they are mostly ghosts between strong neighbours,
            // so they must stand out more.
            let need = if best.bit_rate <= 1200 {
                1.7 * threshold
            } else {
                threshold
            };
            if best.strength >= need && clear {
                let _ = events.send(Event::Found {
                    offset_hz: centre + best.offset_hz,
                    bit_rate: best.bit_rate,
                    strength: best.strength,
                });
            }
        }
    }
}
