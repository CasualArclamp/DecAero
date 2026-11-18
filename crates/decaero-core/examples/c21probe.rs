// made by CasualArclamp
//! Work out the 21000 bit/s C channel's frame from soft bits
//! (`decaero-dsp --example softbits`): try header lengths and interleaver
//! widths, and see which lets the Viterbi decoder make sense of the body.
//! `c21probe SOFT.bin FRAME_START [FRAMES]`
//! FRAME_START is the index of a frame's first bit; frames are 10500 bits.

use decaero_core::fec::{self, Viterbi};

const FRAME: usize = 10_500;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, start, rest @ ..] = args.as_slice() else {
        eprintln!("usage: c21probe SOFT.bin FRAME_START [FRAMES]");
        return;
    };
    let start: usize = start.parse().expect("frame start");
    let frames: usize = rest.first().and_then(|f| f.parse().ok()).unwrap_or(8);
    let soft: Vec<f32> = std::fs::read(path)
        .expect("read the soft bits")
        .into_iter()
        .map(|b| f32::from(b as i8) / 40.0)
        .collect();
    let mut found: Vec<(f32, String)> = Vec::new();
    // One rail the other way up, or not.
    for flip in [false, true] {
        for skip in (96..=220).step_by(2) {
            for cols in (1..=24).chain([27, 32, 36, 48, 54, 64, 81, 108, 162]) {
                let block = fec::ROWS * cols;
                let blocks = (FRAME - skip) / block;
                if blocks == 0 {
                    continue;
                }
                let (mut errors, mut bits) = (0u32, 0u32);
                let mut viterbi = Viterbi::new();
                for f in 0..frames {
                    let at = start + f * FRAME + skip;
                    let Some(body) = soft.get(at..at + blocks * block) else {
                        break;
                    };
                    let body: Vec<f32> = body
                        .iter()
                        .enumerate()
                        .map(|(i, &s)| if flip && (at + i) & 1 == 1 { -s } else { s })
                        .collect();
                    let mut plain = Vec::with_capacity(body.len());
                    for b in body.chunks_exact(block) {
                        plain.extend(fec::deinterleave(b, cols));
                    }
                    let decoded = viterbi.decode_block(&plain);
                    let (e, n) = fec::channel_errors(&decoded, &plain, true);
                    errors += e;
                    bits += n;
                }
                if bits > 0 {
                    found.push((
                        errors as f32 / bits as f32,
                        format!("skip {skip:3}  64 x {cols:3}  {blocks:3} blocks  flip {flip}"),
                    ));
                }
            }
        }
    }
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    for (rate, what) in found.iter().take(12) {
        println!("{:5.1}% channel errors  {what}", rate * 100.0);
    }
    let middle = found[found.len() / 2].0;
    println!(
        "(the middle of all {} tries: {:.1}%)",
        found.len(),
        middle * 100.0
    );
}
