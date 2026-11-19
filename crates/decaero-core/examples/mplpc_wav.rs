// made by CasualArclamp
//! Speech from saved 9.6 kbit/s voice frames (the headless example's
//! `--raw-voice` files: 24 octets a frame, 50 frames a second).
//! `mplpc_wav VOICE.bin OUT.wav [--plain]` (`--plain`: the appendix's
//! decoder to the letter, without the post-filter the apps play through)

use decaero_core::mplpc::{Decoder, FRAME_BYTES, FRAME_SAMPLES, FrameKind};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let plain = args.iter().any(|a| a == "--plain");
    args.retain(|a| a != "--plain");
    let [input, output] = args.as_slice() else {
        eprintln!("usage: mplpc_wav VOICE.bin OUT.wav [--plain]");
        return;
    };
    let raw = std::fs::read(input).expect("read the voice frames");
    let mut decoder = if plain {
        Decoder::new()
    } else {
        Decoder::with_postfilter()
    };
    let mut pcm = Vec::with_capacity(raw.len() / FRAME_BYTES * FRAME_SAMPLES);
    let mut kinds = [0u32; 3];
    for frame in raw.chunks_exact(FRAME_BYTES) {
        let mut samples = [0i16; FRAME_SAMPLES];
        let kind = decoder.decode(frame.try_into().expect("24 octets"), &mut samples);
        kinds[match kind {
            FrameKind::Voice => 0,
            FrameKind::Repeat => 1,
            FrameKind::Mute => 2,
        }] += 1;
        pcm.extend_from_slice(&samples);
    }
    // A plain 16-bit mono WAV at 8 kHz.
    let data = (pcm.len() * 2) as u32;
    let mut wav = Vec::with_capacity(44 + pcm.len() * 2);
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
    std::fs::write(output, wav).expect("write the WAV");
    println!(
        "{} frames: {} good, {} repeated, {} muted",
        kinds.iter().sum::<u32>(),
        kinds[0],
        kinds[1],
        kinds[2]
    );
}
