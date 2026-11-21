// made by CasualArclamp
//! Bench receiver for the L-band correction carriers (`decaero_core::corr`):
//! acquisition, plain BPSK demodulator, frame decoder, on a recording.
//!
//! `cargo run --release -p decaero-dsp --example corrrx -- FILE.wav --at HZ [--at HZ ...]`
//! (`HZ` is the carrier's offset from the recording's centre).

use decaero_core::corr::{self, FrameDecoder, StreamCheck};
use decaero_dsp::acquire;
use decaero_dsp::ddc::Ddc;
use decaero_dsp::demod::{Demod, Modulation};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .expect("a WAV file");
    let rec = decaero_dsp::wav::read(std::path::Path::new(path)).expect("read the recording");
    let rate = rec.sample_rate;
    let offsets: Vec<f64> = args
        .windows(2)
        .filter(|w| w[0] == "--at")
        .filter_map(|w| w[1].parse().ok())
        .collect();
    for at in offsets {
        // Look: four samples a symbol of the fastest rate, three seconds.
        let mut coarse = Ddc::new(rate, at, Ddc::decimation_for(rate, corr::ALL_RATES[0], 4.0));
        let mut wide = Vec::new();
        let look = ((3.0 * rate) as usize).min(rec.samples.len());
        coarse.process(&rec.samples[..look], &mut wide);
        let Some(found) = acquire::find_bpsk(&wide, coarse.out_rate(), &corr::ALL_RATES, 1500.0)
        else {
            println!("{at:>10.0} Hz  nothing found");
            continue;
        };
        let carrier = at + found.offset_hz;
        println!(
            "{carrier:>10.1} Hz  {:.3} sym/s  carrier line x{:.1}  clock line x{:.1}",
            found.symbol_rate, found.carrier_strength, found.clock_strength
        );
        let mut ddc = Ddc::new(
            rate,
            carrier,
            Ddc::decimation_for(rate, found.symbol_rate, 8.0),
        );
        let mut narrow = Vec::new();
        ddc.process(&rec.samples, &mut narrow);
        let mut demod = Demod::with_rate(
            Modulation::Bpsk {
                roll_off: corr::roll_off(found.symbol_rate),
            },
            found.symbol_rate,
            ddc.out_rate(),
            0.0,
        );
        let mut soft = Vec::new();
        for piece in narrow.chunks(512) {
            demod.process(piece, &mut soft);
        }
        let st = demod.status();
        println!(
            "            {} symbols  Es/N0 {:.1} dB  coherence {:.2}  offset {:+.1} Hz  clock {:+.0} ppm",
            soft.len(),
            st.es_n0_db,
            st.coherence,
            st.offset_hz,
            st.clock_ppm
        );
        if corr::framed(found.symbol_rate) {
            let mut dec = FrameDecoder::new();
            let mut frames = Vec::new();
            dec.push(&soft, &mut frames);
            for f in &frames {
                let head: String = f
                    .payload_head(8)
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect();
                println!(
                    "            id {:04X}  field {:02X}  payload {head}..  BER {:.4}  UW {} wrong{}{}{}",
                    f.id,
                    f.field,
                    f.ber(),
                    f.uw_errors,
                    if f.inverted { "  inverted" } else { "" },
                    if f.flywheel { "  flywheel" } else { "" },
                    if f.tail_clear() { "" } else { "  TAIL?" },
                );
            }
            println!(
                "            {} frames, {} lock losses",
                dec.frames, dec.lock_losses
            );
        } else {
            let mut check = StreamCheck::new();
            let mut pieces = Vec::new();
            check.push(&soft, &mut pieces);
            let (e, b) = pieces.iter().fold((0u64, 0u64), |a, p| {
                (
                    a.0 + u64::from(p.channel_errors),
                    a.1 + u64::from(p.channel_bits),
                )
            });
            let good = pieces
                .iter()
                .filter(|p| p.channel_errors * 20 < p.channel_bits)
                .count();
            println!(
                "            unframed: {} pieces, {good} with code lock, BER {:.4}",
                pieces.len(),
                e as f64 / b.max(1) as f64
            );
        }
    }
}
