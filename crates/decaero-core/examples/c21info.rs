// made by CasualArclamp
//! Decode the 21000 bit/s C channel's frames from soft bits and write each
//! frame's 5184 information bits as a line of 0s and 1s (not descrambled):
//! `c21info SOFT.bin FRAME_START OUT.txt`
//! Frame: 10500 bits; 132 of header, then 27 interleaver blocks of 64 x 6,
//! rate 1/2 (found with `c21probe`).

use decaero_core::fec::{self, Viterbi};

const FRAME: usize = 10_500;
const SKIP: usize = 132;
const COLS: usize = 6;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, start, out] = args.as_slice() else {
        eprintln!("usage: c21info SOFT.bin FRAME_START OUT.txt");
        return;
    };
    let start: usize = start.parse().expect("frame start");
    let soft: Vec<f32> = std::fs::read(path)
        .expect("read the soft bits")
        .into_iter()
        .map(|b| f32::from(b as i8) / 40.0)
        .collect();
    let mut viterbi = Viterbi::new();
    let mut text = String::new();
    let mut f = 0;
    while let Some(frame) = soft.get(start + f * FRAME..start + (f + 1) * FRAME) {
        let mut plain = Vec::with_capacity(FRAME - SKIP);
        for block in frame[SKIP..].chunks_exact(fec::ROWS * COLS) {
            plain.extend(fec::deinterleave(block, COLS));
        }
        let decoded = viterbi.decode_block(&plain);
        let (errors, bits) = fec::channel_errors(&decoded, &plain, true);
        let header: String = frame[..SKIP]
            .iter()
            .map(|&s| if s > 0.0 { '1' } else { '0' })
            .collect();
        println!(
            "frame {f:3}  {:5.1}% channel errors  header {header}",
            100.0 * errors as f32 / bits.max(1) as f32
        );
        text.extend(decoded.iter().map(|&b| if b == 1 { '1' } else { '0' }));
        text.push('\n');
        f += 1;
    }
    std::fs::write(out, text).expect("write the bits");
}
