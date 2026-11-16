// made by CasualArclamp
//! Decode a file of P channel soft bits (little-endian f32, positive = 1):
//! `cargo run -p decaero-core --example psoft -- FILE.f32 600`
//! A bench tool for trying a demodulator's output against the frame decoder.

use decaero_core::pframe::{PDecoder, PFormat};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("soft bit file");
    let rate: u32 = args.next().and_then(|r| r.parse().ok()).unwrap_or(600);
    let bytes = std::fs::read(&path).expect("read file");
    let soft: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let mut dec = PDecoder::new(PFormat::from_rate(rate).expect("600, 1200 or 10500"));
    let mut frames = Vec::new();
    dec.push(&soft, &mut frames);
    for f in &frames {
        println!(
            "frame {:2} fmt {} uw errors {:2} pre-FEC BER {:.3} CRC {}/{}",
            f.header.frame_counter,
            f.header.format_id,
            f.uw_errors,
            f.pre_fec_ber(),
            f.crc_good(),
            f.units.len()
        );
        for u in &f.units {
            let hex: Vec<String> = u.bytes[..10].iter().map(|b| format!("{b:02X}")).collect();
            println!(
                "   {} {}",
                if u.crc_ok { "ok " } else { "BAD" },
                hex.join(" ")
            );
        }
    }
    println!("{:?} locked {}", dec.stats(), dec.locked());
}
