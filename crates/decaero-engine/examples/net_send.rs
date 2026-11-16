// made by CasualArclamp
//! Plays a recording out over the network at the speed it was recorded, for
//! trying the live inputs without a radio:
//!
//! `cargo run --release -p decaero-engine --example net_send -- FILE.wav sdrpp://127.0.0.1:5259`
//!     stands in for an SDR++ server (16-bit samples, or 8-bit and zstd when
//!     the client asks, as the real one does)
//! `cargo run --release -p decaero-engine --example net_send -- FILE.wav listen://127.0.0.1:7777 int16`
//!     a plain I/Q stream, as SDR++'s IQ Exporter sends it
//!
//! It serves one client at a time and starts the file again for the next.

use decaero_engine::net::sdrpp;
use num_complex::Complex32;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

fn packet(kind: u32, body: &[u8]) -> Vec<u8> {
    let mut p = kind.to_le_bytes().to_vec();
    p.extend_from_slice(&((8 + body.len()) as u32).to_le_bytes());
    p.extend_from_slice(body);
    p
}

/// Samples to bytes; integer formats are scaled to the block's peak when
/// `peak` is given (the SDR++ server's way), else to full scale 1.0.
fn encode(block: &[Complex32], eight_bit: bool, float: bool, peak: Option<f32>) -> Vec<u8> {
    let scale = 1.0 / peak.unwrap_or(1.0).max(1e-9);
    let mut out = Vec::with_capacity(block.len() * 8);
    for s in block {
        for v in [s.re, s.im] {
            if float {
                out.extend_from_slice(&v.to_le_bytes());
            } else if eight_bit {
                out.push((v * scale * 127.0).clamp(-128.0, 127.0) as i8 as u8);
            } else {
                out.extend_from_slice(
                    &((v * scale * 32767.0).clamp(-32768.0, 32767.0) as i16).to_le_bytes(),
                );
            }
        }
    }
    out
}

fn serve_sdrpp(mut stream: TcpStream, samples: &[Complex32], rate: f64) -> std::io::Result<()> {
    let mut body = sdrpp::COMMAND_SET_SAMPLERATE.to_le_bytes().to_vec();
    body.extend_from_slice(&rate.to_le_bytes());
    stream.write_all(&packet(sdrpp::PACKET_COMMAND, &body))?;
    stream.set_nonblocking(true)?;
    let (mut eight_bit, mut compressed, mut running) = (false, false, false);
    let mut inbox: Vec<u8> = Vec::new();
    let block = (rate / 40.0) as usize;
    let started = Instant::now();
    let mut sent = 0usize;
    loop {
        // Whatever commands have arrived.
        let mut buf = [0u8; 4096];
        match stream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => inbox.extend_from_slice(&buf[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e),
        }
        while inbox.len() >= 12 {
            let size = u32::from_le_bytes(inbox[4..8].try_into().unwrap()) as usize;
            if inbox.len() < size.max(12) {
                break;
            }
            let command = u32::from_le_bytes(inbox[8..12].try_into().unwrap());
            let data = inbox[12..size].to_vec();
            inbox.drain(..size);
            match command {
                sdrpp::COMMAND_GET_UI | sdrpp::COMMAND_SET_FREQUENCY => {
                    if command == sdrpp::COMMAND_SET_FREQUENCY && data.len() == 8 {
                        println!(
                            "  tuned to {:.6} MHz",
                            f64::from_le_bytes(data[..8].try_into().unwrap()) / 1e6
                        );
                    }
                    stream.set_nonblocking(false)?;
                    stream.write_all(&packet(sdrpp::PACKET_COMMAND_ACK, &command.to_le_bytes()))?;
                    stream.set_nonblocking(true)?;
                }
                sdrpp::COMMAND_SET_SAMPLE_TYPE => eight_bit = data.first() == Some(&0),
                sdrpp::COMMAND_SET_COMPRESSION => compressed = data.first() == Some(&1),
                sdrpp::COMMAND_START => running = true,
                sdrpp::COMMAND_STOP => running = false,
                _ => {}
            }
        }
        if !running {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        // Keep to the recording's own pace.
        let due = (started.elapsed().as_secs_f64() * rate) as usize;
        if sent + block > due {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let at = sent % samples.len().max(1);
        let end = (at + block).min(samples.len());
        let chunk = &samples[at..end];
        sent += chunk.len();
        let peak = chunk
            .iter()
            .fold(1e-9f32, |m, s| m.max(s.re.abs()).max(s.im.abs()));
        let mut bb = vec![0u8, 0];
        bb.extend_from_slice(
            &(if eight_bit {
                sdrpp::PCM_I8
            } else {
                sdrpp::PCM_I16
            })
            .to_le_bytes(),
        );
        bb.extend_from_slice(&peak.to_le_bytes());
        bb.extend_from_slice(&encode(chunk, eight_bit, false, Some(peak)));
        let out = if compressed {
            let z = ruzstd::encoding::compress_to_vec(
                &bb[..],
                ruzstd::encoding::CompressionLevel::Fastest,
            );
            packet(sdrpp::PACKET_BASEBAND_COMPRESSED, &z)
        } else {
            packet(sdrpp::PACKET_BASEBAND, &bb)
        };
        stream.set_nonblocking(false)?;
        stream.write_all(&out)?;
        stream.set_nonblocking(true)?;
    }
}

fn serve_raw(
    mut stream: TcpStream,
    samples: &[Complex32],
    rate: f64,
    format: &str,
) -> std::io::Result<()> {
    let block = (rate / 40.0) as usize;
    let started = Instant::now();
    let mut sent = 0usize;
    loop {
        let due = (started.elapsed().as_secs_f64() * rate) as usize;
        if sent + block > due {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let at = sent % samples.len().max(1);
        let chunk = &samples[at..(at + block).min(samples.len())];
        sent += chunk.len();
        stream.write_all(&encode(chunk, format == "int8", format == "float32", None))?;
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (path, url) = (
        args.first().expect("a WAV file"),
        args.get(1)
            .expect("sdrpp://ADDR:PORT or listen://ADDR:PORT"),
    );
    let format = args.get(2).map_or("int16", String::as_str);
    let rec = decaero_dsp::wav::read(std::path::Path::new(path)).expect("read the recording");
    let (scheme, addr) = url.split_once("://").expect("a URL");
    let listener = TcpListener::bind(addr).expect("bind");
    println!("{} at {:.0} kS/s on {url}", path, rec.sample_rate / 1e3);
    for stream in listener.incoming().flatten() {
        println!("client {:?}", stream.peer_addr().ok());
        stream.set_nodelay(true).ok();
        let result = match scheme {
            "sdrpp" => serve_sdrpp(stream, &rec.samples, rec.sample_rate),
            _ => serve_raw(stream, &rec.samples, rec.sample_rate, format),
        };
        println!("client gone ({result:?})");
    }
}
