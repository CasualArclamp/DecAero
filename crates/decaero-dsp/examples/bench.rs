// made by CasualArclamp
//! Speed of the two hot loops, to measure before and after a change:
//! `cargo run --release -p decaero-dsp --example bench`

use decaero_dsp::ddc::Ddc;
use decaero_dsp::demod::{Demod, Modulation};
use num_complex::Complex32;
use std::time::Instant;

fn main() {
    let mut seed = 1u32;
    let mut next = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / 8_388_608.0 - 1.0
    };
    let wide: Vec<Complex32> = (0..6_250_000)
        .map(|_| Complex32::new(next(), next()))
        .collect();
    println!("vector unit: {}", decaero_dsp::simd::vector_level());

    // The down-converter: ten seconds of a 625 kS/s input for one channel.
    let mut out = Vec::new();
    let mut best = f64::MAX;
    for _ in 0..5 {
        let mut ddc = Ddc::new(625_000.0, 50_000.0, 130);
        out.clear();
        let t = Instant::now();
        for block in wide.chunks(31_250) {
            ddc.process(block, &mut out);
        }
        best = best.min(t.elapsed().as_secs_f64());
    }
    let sum: Complex32 = out.iter().sum();
    println!(
        "down-converter: {:6.1} M samples/s  ({:.2}% of a core per channel at 625 kS/s)  check {:.4}",
        wide.len() as f64 / best / 1e6,
        best / 10.0 * 100.0,
        sum.norm()
    );

    // The demodulator at 8 samples a bit: a minute of 8400 bit/s.
    let narrow = &wide[..8400 * 8 * 60];
    let mut best = f64::MAX;
    let mut soft = Vec::new();
    for _ in 0..5 {
        let mut demod = Demod::new(Modulation::for_rate(8400), 8400, 67_200.0, 0.0);
        soft.clear();
        let t = Instant::now();
        for piece in narrow.chunks(512) {
            demod.process(piece, &mut soft);
        }
        best = best.min(t.elapsed().as_secs_f64());
    }
    println!(
        "demodulator:    {:6.1} M samples/s  ({:.2}% of a core for an 8400 bit/s channel)  check {:.4}",
        narrow.len() as f64 / best / 1e6,
        best / 60.0 * 100.0,
        soft.iter().sum::<f32>()
    );
}
