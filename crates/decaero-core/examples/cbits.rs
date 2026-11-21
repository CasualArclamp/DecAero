// made by CasualArclamp
//! The channel bits of 8400 bit/s C channel frames with random voice and
//! empty signal units, one byte (0 or 1) a bit, for a test modulator to put on a
//! carrier: `cargo run -p decaero-core --example cbits -- FRAMES SEED OUT.bin`.
//! The serial order is the encoder's: the two rails alternate, Q first.

use decaero_core::cframe::{self, VOICE_BYTES, VOICE_FRAMES};

fn main() -> std::io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let frames: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(80);
    let mut seed: u32 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(1);
    let out = args.get(2).map_or("cbits.bin", String::as_str);
    let mut next = || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 24) as u8
    };
    let mut bits = Vec::new();
    for _ in 0..frames {
        let mut voice = [[0u8; VOICE_BYTES]; VOICE_FRAMES];
        voice.iter_mut().flatten().for_each(|b| *b = next());
        let units = [[0u8; 10]; 3];
        // Random signal units would now and then read as a call release,
        // which closes the decoder: fill units of zeros instead.
        bits.extend(cframe::encode(&voice, &units));
    }
    std::fs::write(out, bits)
}
