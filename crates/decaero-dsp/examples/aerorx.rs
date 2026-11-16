// made by CasualArclamp
//! Bench receiver: find every Aero carrier in a recording and decode it.
//!
//! `cargo run --release -p decaero-dsp --example aerorx -- FILE.wav [--units]`
//!
//! It reads the whole file, looks at the averaged spectrum for carriers,
//! names the bit rate of each from its squared-spectrum lines, and runs the
//! demodulator and frame decoder on it. `--units` prints the signal units.

use decaero_core::pframe::{PDecoder, PFormat};
use decaero_dsp::acquire;
use decaero_dsp::ddc::Ddc;
use decaero_dsp::demod::{Demod, Modulation};
use num_complex::Complex32;
use rustfft::FftPlanner;

/// Carriers in the averaged spectrum: (centre Hz, width Hz, peak dB over floor).
fn carriers(samples: &[Complex32], rate: f64) -> Vec<(f64, f64, f32)> {
    let n = 1usize << 14;
    let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
    let mut power = vec![0f32; n];
    let mut buf = vec![Complex32::default(); n];
    let segments = (samples.len() / n).clamp(1, 400);
    let hop = (samples.len() - n) / segments;
    for s in 0..segments {
        for (i, b) in buf.iter_mut().enumerate() {
            let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos();
            *b = samples[s * hop + i] * w;
        }
        fft.process(&mut buf);
        for (p, b) in power.iter_mut().zip(&buf) {
            *p += b.norm_sqr();
        }
    }
    // Lowest frequency first, in dB, lightly smoothed.
    let db: Vec<f32> = (0..n)
        .map(|i| 10.0 * power[(i + n / 2) % n].max(1e-20).log10())
        .collect();
    let smooth: Vec<f32> = (0..n)
        .map(|i| {
            (0..5)
                .map(|k| db[(i + k).saturating_sub(2).min(n - 1)])
                .sum::<f32>()
                / 5.0
        })
        .collect();
    let mut sorted = smooth.clone();
    sorted.sort_by(f32::total_cmp);
    let floor = sorted[n * 3 / 10];
    let above = |i: usize| i < n && smooth[i] > floor + 4.0;
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if above(i) {
            let mut j = i;
            while above(j) || (j..j + 4).any(above) {
                j += 1;
            }
            let width = (j - i) as f64 * rate / n as f64;
            let centre = ((i + j) as f64 / 2.0 - (n / 2) as f64) * rate / n as f64;
            let peak = smooth[i..j].iter().copied().fold(f32::MIN, f32::max) - floor;
            if width > 250.0 {
                out.push((centre, width, peak));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .expect("a WAV file");
    let show_units = args.iter().any(|a| a == "--units");
    let rec = decaero_dsp::wav::read(std::path::Path::new(path)).expect("read the recording");
    let rate = rec.sample_rate;
    println!(
        "{:.1} s at {:.0} S/s{}",
        rec.samples.len() as f64 / rate,
        rate,
        if rec.real { " (real IF)" } else { "" }
    );
    for (centre, width, peak) in carriers(&rec.samples, rate) {
        if rec.real && centre < 0.0 {
            continue; // the mirror image of a real signal
        }
        // What the width allows; the squared-spectrum lines decide.
        let rates: &[u32] = if width < 2200.0 {
            &[600, 1200]
        } else {
            &[8400, 10500, 1200]
        };
        let top = f64::from(*rates.iter().max().unwrap_or(&600));
        let mut ddc = Ddc::new(rate, centre, Ddc::decimation_for(rate, top, 6.0));
        let mut wide = Vec::new();
        ddc.process(&rec.samples, &mut wide);
        let found = acquire::find(&wide, ddc.out_rate(), rates, width.max(600.0));
        let Some(best) = found.first().filter(|f| f.strength >= 6.0) else {
            println!(
                "{centre:>10.0} Hz  width {width:>5.0}  peak {peak:>4.1} dB  no Aero line pair"
            );
            continue;
        };
        let carrier = centre + best.offset_hz;
        print!(
            "{carrier:>10.1} Hz  width {width:>5.0}  peak {peak:>4.1} dB  {:>5} bit/s  lines x{:<5.1}",
            best.bit_rate, best.strength
        );
        let Some(format) = PFormat::from_rate(best.bit_rate) else {
            println!("  C channel (voice): decoder not written yet");
            continue;
        };
        let bit_rate = f64::from(best.bit_rate);
        let mut ddc = Ddc::new(rate, carrier, Ddc::decimation_for(rate, bit_rate, 8.0));
        let mut narrow = Vec::new();
        ddc.process(&rec.samples, &mut narrow);
        let mut demod = Demod::new(
            Modulation::for_rate(best.bit_rate),
            best.bit_rate,
            ddc.out_rate(),
            0.0,
        );
        let mut soft = Vec::new();
        // In pieces, as a live receiver would, so the frequency loop closes.
        for (i, piece) in narrow.chunks(512).enumerate() {
            demod.process(piece, &mut soft);
            if std::env::var("AERORX_TRACE").is_ok() && i % 20 == 0 {
                let s = demod.status();
                eprintln!(
                    "t {:5.1} off {:+7.2} coh {:.2} esn0 {:4.1} ppm {:+.0}",
                    (i * 512) as f64 / ddc.out_rate(),
                    s.offset_hz,
                    s.coherence,
                    s.es_n0_db,
                    s.clock_ppm
                );
            }
        }
        let mut decoder = PDecoder::new(format);
        let mut frames = Vec::new();
        decoder.push(&soft, &mut frames);
        let st = decoder.stats();
        let ds = demod.status();
        println!(
            "  Es/N0 {:>4.1} dB  offset {:+.1} Hz  frames {:>3}  CRC {}/{}  pre-FEC BER {:.1e}  UW {}/{} wrong",
            ds.es_n0_db,
            ds.offset_hz,
            st.frames,
            st.crc_good,
            st.crc_good + st.crc_bad,
            st.channel_errors as f64 / st.channel_bits.max(1) as f64,
            st.uw_bits_wrong,
            st.uw_bits
        );
        if show_units {
            for f in &frames {
                for u in f
                    .units
                    .iter()
                    .filter(|u| u.crc_ok && !matches!(u.bytes[0], 0x01 | 0x26 | 0x0A))
                {
                    let hex: Vec<String> =
                        u.bytes[..10].iter().map(|b| format!("{b:02X}")).collect();
                    println!("      {}", hex.join(" "));
                }
            }
        }
    }
}
