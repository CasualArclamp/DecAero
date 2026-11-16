// made by CasualArclamp
//! A small WAV reader for recordings: 8-bit unsigned or 16-bit signed PCM,
//! two channels as I and Q. A mono file, or a stereo one whose channels are
//! the same (the audio output of a receiver), is real IF: Q is left at zero.

use num_complex::Complex32;
use std::io::{Error, ErrorKind, Result};

pub struct Recording {
    pub sample_rate: f64,
    pub samples: Vec<Complex32>,
    /// True when the file held one real signal rather than I and Q.
    pub real: bool,
}

fn bad(msg: &str) -> Error {
    Error::new(ErrorKind::InvalidData, msg.to_string())
}

pub fn read(path: &std::path::Path) -> Result<Recording> {
    let bytes = std::fs::read(path)?;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad("not a WAV file"));
    }
    let (mut channels, mut rate, mut bits) = (0usize, 0u32, 0u16);
    let mut pos = 12;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let len = u32::from_le_bytes([
            bytes[pos + 4],
            bytes[pos + 5],
            bytes[pos + 6],
            bytes[pos + 7],
        ]) as usize;
        let body = pos + 8;
        if id == b"fmt " && body + 16 <= bytes.len() {
            channels = usize::from(u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]));
            rate = u32::from_le_bytes([
                bytes[body + 4],
                bytes[body + 5],
                bytes[body + 6],
                bytes[body + 7],
            ]);
            bits = u16::from_le_bytes([bytes[body + 14], bytes[body + 15]]);
        } else if id == b"data" {
            let data = &bytes[body..(body + len).min(bytes.len())];
            let values: Vec<f32> = match bits {
                8 => data
                    .iter()
                    .map(|&b| (f32::from(b) - 127.5) / 127.5)
                    .collect(),
                16 => data
                    .chunks_exact(2)
                    .map(|c| f32::from(i16::from_le_bytes([c[0], c[1]])) / 32768.0)
                    .collect(),
                _ => return Err(bad("only 8 and 16 bit PCM are read")),
            };
            let (samples, real) = match channels {
                1 => (
                    values.iter().map(|&v| Complex32::new(v, 0.0)).collect(),
                    true,
                ),
                2 => {
                    let same = values.chunks_exact(2).take(100_000).all(|c| c[0] == c[1]);
                    let s = values
                        .chunks_exact(2)
                        .map(|c| Complex32::new(c[0], if same { 0.0 } else { c[1] }))
                        .collect();
                    (s, same)
                }
                _ => return Err(bad("one or two channels expected")),
            };
            return Ok(Recording {
                sample_rate: f64::from(rate),
                samples,
                real,
            });
        }
        pos = body + len + (len & 1);
    }
    Err(bad("no data chunk"))
}
