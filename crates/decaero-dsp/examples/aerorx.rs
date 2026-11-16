// made by CasualArclamp
//! Bench receiver: find every Aero carrier in a recording and decode it.
//!
//! `cargo run --release -p decaero-dsp --example aerorx -- FILE.wav [--units]`
//!
//! It reads the whole file, looks at the averaged spectrum for carriers,
//! names the bit rate of each from its squared-spectrum lines, and runs the
//! demodulator and frame decoder on it. `--units` prints the signal units.

use decaero_core::cframe::CDecoder;
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

/// An 8400 bit/s C channel: demodulate, decode the frames, turn the voice
/// frames into audio and save it under `out/` as a WAV (8 kHz, 16 bit).
fn voice_call(samples: &[Complex32], rate: f64, carrier: f64, path: &str) {
    let mut ddc = Ddc::new(rate, carrier, Ddc::decimation_for(rate, 8400.0, 8.0));
    let mut narrow = Vec::new();
    ddc.process(samples, &mut narrow);
    let mut demod = Demod::new(Modulation::for_rate(8400), 8400, ddc.out_rate(), 0.0);
    let mut soft = Vec::new();
    for piece in narrow.chunks(512) {
        demod.process(piece, &mut soft);
    }
    let mut decoder = CDecoder::new();
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
    let mut voice = decaero_ambe::Decoder::new();
    let mut pcm: Vec<i16> = Vec::new();
    let mut kinds = [0usize; 4];
    let mut errs = [0usize; 8];
    let mut timeline = String::new();
    for f in &frames {
        for u in f.units.iter().filter(|u| u.crc_ok && u.bytes[0] != 0x01) {
            let hex: Vec<String> = u.bytes[..10].iter().map(|b| format!("{b:02X}")).collect();
            println!("      sub-band {}", hex.join(" "));
        }
        for v in &f.voice {
            let mut out = [0i16; decaero_ambe::FRAME_SAMPLES];
            let info = voice.decode(v, &mut out);
            kinds[info.kind as usize] += 1;
            timeline.push(['V', 'r', '.', 'T'][info.kind as usize]);
            errs[(info.total_errors as usize).min(7)] += 1;
            pcm.extend_from_slice(&out);
        }
    }
    if std::env::var("AERORX_TRACE").is_ok() {
        // One letter per 20 ms: V voiced, r repeated, . silence, T tone.
        for line in timeline.as_bytes().chunks(100) {
            println!("      {}", String::from_utf8_lossy(line));
        }
    }
    println!("      code-word corrections per voice frame (0..7+): {errs:?}");
    println!(
        "      voice frames: {} voiced, {} repeated, {} silent, {} tone",
        kinds[0], kinds[1], kinds[2], kinds[3]
    );
    if pcm.is_empty() {
        return;
    }
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("call")
        .trim()
        .to_string();
    let name = format!("out/{stem}_{carrier:.0}Hz.wav");
    let _ = std::fs::create_dir_all("out");
    let data = pcm.len() as u32 * 2;
    let mut wav = Vec::with_capacity(44 + data as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&8000u32.to_le_bytes());
    wav.extend_from_slice(&16000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data.to_le_bytes());
    for s in &pcm {
        wav.extend_from_slice(&s.to_le_bytes());
    }
    match std::fs::write(&name, wav) {
        Ok(()) => println!("      audio: {name} ({:.1} s)", pcm.len() as f64 / 8000.0),
        Err(e) => println!("      could not write {name}: {e}"),
    }
}

/// `--burst RATE --at HZ`: watch one channel for R and T bursts.
fn bursts(samples: &[Complex32], rate: f64, carrier: f64, bit_rate: u32) {
    use decaero_core::rtframe::{self, Burst};
    let mut ddc = Ddc::new(
        rate,
        carrier,
        Ddc::decimation_for(rate, f64::from(bit_rate), 8.0),
    );
    let mut narrow = Vec::new();
    ddc.process(samples, &mut narrow);
    let mut rx = decaero_dsp::burst::BurstReceiver::new(ddc.out_rate(), bit_rate);
    let mut found = Vec::new();
    for piece in narrow.chunks(512) {
        rx.process(piece, &mut found);
    }
    println!(
        "{} bursts at {bit_rate} bit/s near {carrier:.0} Hz",
        found.len()
    );
    for b in &found {
        print!(
            "  {:5.2} s  {:+7.1} Hz  Es/N0 {:4.1} dB  ",
            b.seconds, b.offset_hz, b.es_n0_db
        );
        match rtframe::decode_burst(&b.soft, bit_rate > 5000) {
            None => println!("not decoded ({} bits)", b.soft.len()),
            Some(d) => {
                let quality = format!(
                    "pre-FEC BER {:.1e}  UW {} wrong",
                    d.pre_fec_ber(),
                    d.uw_errors
                );
                match &d.burst {
                    Burst::R { unit } => {
                        let hex: Vec<String> = unit.iter().map(|x| format!("{x:02X}")).collect();
                        let what = match rtframe::r_data(unit) {
                            Some(h) => format!(
                                "user data {} of {} from {:06X}",
                                h.index + 1,
                                h.total,
                                h.aes
                            ),
                            None => rtframe::r_name(unit[2]).to_string(),
                        };
                        println!(
                            "R  {quality}  {what}
      {}",
                            hex.join(" ")
                        );
                    }
                    Burst::T { aes, ges, units } => {
                        let good = units.iter().filter(|u| u.crc_ok).count();
                        println!(
                            "T  {quality}  from {aes:06X} to GES {ges:02X}, {good} of {} units good",
                            units.len()
                        );
                    }
                }
            }
        }
    }
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
    // `--at HZ` skips the survey and looks for a wide carrier near HZ (for
    // carriers too weak to stand out of the spectrum).
    let forced: Vec<(f64, f64, f32)> = args
        .windows(2)
        .filter(|w| w[0] == "--at")
        .filter_map(|w| {
            // `HZ` for a fast carrier, `HZ:slow` for 600 or 1200 bit/s.
            let (hz, slow) = match w[1].split_once(':') {
                Some((hz, _)) => (hz, true),
                None => (w[1].as_str(), false),
            };
            Some((hz.parse().ok()?, if slow { 1500.0 } else { 6000.0 }, 0.0))
        })
        .collect();
    let list = if !forced.is_empty() {
        forced
    } else if rec.real {
        // A receiver's audio: the passband is one lump in the spectrum, so
        // look for the line pairs of every rate across the whole of it.
        acquire::find(&rec.samples, rate, &[600, 1200, 8400, 10500], rate / 2.0)
            .into_iter()
            .filter(|f| f.strength >= 4.5 && f.offset_hz > 0.0)
            .map(|f| {
                (
                    f.offset_hz,
                    if f.bit_rate <= 1200 { 1500.0 } else { 6000.0 },
                    0.0,
                )
            })
            .collect()
    } else {
        carriers(&rec.samples, rate)
    };
    // `--burst RATE` with `--at HZ`: the channel is watched for bursts.
    let burst_rate: Option<u32> = args
        .windows(2)
        .find(|w| w[0] == "--burst")
        .and_then(|w| w[1].parse().ok());
    if let Some(bit_rate) = burst_rate {
        for (centre, _, _) in &list {
            bursts(&rec.samples, rate, *centre, bit_rate);
        }
        return;
    }
    for (centre, width, peak) in list {
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
        let mut ddc = Ddc::new(rate, centre, Ddc::decimation_for(rate, top, 2.5));
        let mut wide = Vec::new();
        ddc.process(&rec.samples, &mut wide);
        let found = acquire::find(&wide, ddc.out_rate(), rates, width.max(600.0));
        if std::env::var("AERORX_TRACE").is_ok() {
            eprintln!("{found:?}");
        }
        let Some(best) = found.first().filter(|f| f.strength >= 4.0) else {
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
            voice_call(&rec.samples, rate, carrier, path);
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
