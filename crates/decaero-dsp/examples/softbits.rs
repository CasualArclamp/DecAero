// made by CasualArclamp
//! Demodulate one carrier of a narrow recording and write its soft bits, for
//! working out a channel whose frame is not known:
//! `softbits FILE.wav BIT_RATE OFFSET_HZ OUT.bin [ROLL_OFF]`
//! OUT.bin is one signed byte a bit (positive = 1), in the demodulator's
//! serial order (for A-QPSK the two rails alternating).

use decaero_dsp::ddc::Ddc;
use decaero_dsp::demod::{Demod, Modulation};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, bit_rate, offset, out, rest @ ..] = args.as_slice() else {
        eprintln!("usage: softbits FILE.wav BIT_RATE OFFSET_HZ OUT.bin [ROLL_OFF]");
        return;
    };
    let bit_rate: u32 = bit_rate.parse().expect("bit rate");
    let offset: f64 = offset.parse().expect("offset in Hz");
    let rec = decaero_dsp::wav::read(std::path::Path::new(path)).expect("read the recording");
    let rate = rec.sample_rate;
    let modulation = match rest.first().and_then(|r| r.parse::<f32>().ok()) {
        Some(roll_off) => Modulation::AQpsk { roll_off },
        None => Modulation::for_rate(bit_rate),
    };
    let mut ddc = Ddc::new(
        rate,
        offset,
        Ddc::decimation_for(rate, f64::from(bit_rate), 8.0),
    );
    let mut demod = Demod::new(modulation, bit_rate, ddc.out_rate(), 0.0);
    println!(
        "{rate} S/s in, {:.0} S/s to the demodulator ({:.2} a bit)",
        ddc.out_rate(),
        ddc.out_rate() / f64::from(bit_rate)
    );
    let mut narrow = Vec::new();
    let mut soft = Vec::new();
    let mut bytes: Vec<u8> = Vec::new();
    let second = rate as usize;
    for (i, block) in rec.samples.chunks(second / 10).enumerate() {
        narrow.clear();
        ddc.process(block, &mut narrow);
        soft.clear();
        for piece in narrow.chunks(512) {
            demod.process(piece, &mut soft);
        }
        let scale = 40.0;
        bytes.extend(
            soft.iter()
                .map(|&s| (s * scale).clamp(-127.0, 127.0) as i8 as u8),
        );
        if i % 20 == 19 {
            let st = demod.status();
            println!(
                "{:5.1} s  Es/N0 {:5.1} dB  coherence {:.2}  offset {:+8.1} Hz  clock {:+6.1} ppm  bits {}",
                (i + 1) as f64 / 10.0,
                st.es_n0_db,
                st.coherence,
                offset + st.offset_hz,
                st.clock_ppm,
                bytes.len()
            );
        }
    }
    std::fs::write(out, &bytes).expect("write the soft bits");
    println!("{} soft bits to {out}", bytes.len());
}
