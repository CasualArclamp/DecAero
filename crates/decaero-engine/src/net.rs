// made by CasualArclamp
//! Live input over the network.
//!
//! * `sdrpp://HOST[:5259]`: an SDR++ server (`sdrpp --server`). The protocol
//!   is SDR++'s own, read from its source (`core/src/server_protocol.h`,
//!   `core/src/server.cpp`, `dsp/compression/sample_stream_compressor.h`,
//!   GPL-3, (C) Alexandre Rouma): packets of an 8-byte header (type, size
//!   including the header), commands with a 4-byte number, and baseband
//!   packets of an 8-byte sub-header (compression, sample type, scale) and
//!   samples, optionally zstd-compressed.
//! * `tcp://HOST:PORT`, `listen://ADDR:PORT`, `udp://ADDR:PORT`: plain
//!   interleaved I/Q, as SDR++'s IQ Exporter, GNU Radio or DecSAT's
//!   `net_send` send it. The sample rate cannot be learnt from the stream, so
//!   it is given.
//!
//! A reader thread fills a queue; when the engine falls behind, the oldest
//! blocks are dropped and counted rather than letting the delay grow.

use crate::Source;
use num_complex::Complex32;
use std::io::{Error, ErrorKind, Read, Result, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// SDR++ server packet types and commands (`server_protocol.h`).
pub mod sdrpp {
    pub const PACKET_COMMAND: u32 = 0;
    pub const PACKET_COMMAND_ACK: u32 = 1;
    pub const PACKET_BASEBAND: u32 = 2;
    pub const PACKET_BASEBAND_COMPRESSED: u32 = 3;
    pub const COMMAND_GET_UI: u32 = 0x00;
    pub const COMMAND_UI_ACTION: u32 = 0x01;
    pub const COMMAND_START: u32 = 0x02;
    pub const COMMAND_STOP: u32 = 0x03;
    pub const COMMAND_SET_FREQUENCY: u32 = 0x04;
    pub const COMMAND_SET_SAMPLE_TYPE: u32 = 0x06;
    pub const COMMAND_SET_COMPRESSION: u32 = 0x07;
    pub const COMMAND_SET_SAMPLERATE: u32 = 0x80;
    pub const COMMAND_DISCONNECT: u32 = 0x81;
    /// `dsp::compression::PCMType`.
    pub const PCM_I8: u16 = 0;
    pub const PCM_I16: u16 = 1;
    pub const PCM_F32: u16 = 2;
    pub const DEFAULT_PORT: u16 = 5259;
}

/// Sample format of a plain I/Q stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    I8,
    U8,
    I16,
    F32,
}

impl Format {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "int8" | "i8" | "cs8" => Some(Self::I8),
            "uint8" | "u8" | "cu8" => Some(Self::U8),
            "int16" | "i16" | "cs16" => Some(Self::I16),
            "float32" | "f32" | "cf32" => Some(Self::F32),
            _ => None,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::I8 => "int8",
            Self::U8 => "uint8",
            Self::I16 => "int16",
            Self::F32 => "float32",
        }
    }
    /// Bytes in one I/Q pair.
    pub fn pair_bytes(&self) -> usize {
        match self {
            Self::I8 | Self::U8 => 2,
            Self::I16 => 4,
            Self::F32 => 8,
        }
    }
    /// Whole pairs of `bytes` as samples of about unit scale.
    pub fn decode(&self, bytes: &[u8], out: &mut Vec<Complex32>) {
        match self {
            Self::I8 => out.extend(
                bytes
                    .chunks_exact(2)
                    .map(|c| Complex32::new(f32::from(c[0] as i8), f32::from(c[1] as i8)) / 128.0),
            ),
            Self::U8 => {
                out.extend(bytes.chunks_exact(2).map(|c| {
                    Complex32::new(f32::from(c[0]) - 127.5, f32::from(c[1]) - 127.5) / 127.5
                }))
            }
            Self::I16 => out.extend(bytes.chunks_exact(4).map(|c| {
                Complex32::new(
                    f32::from(i16::from_le_bytes([c[0], c[1]])),
                    f32::from(i16::from_le_bytes([c[2], c[3]])),
                ) / 32768.0
            })),
            Self::F32 => out.extend(bytes.chunks_exact(8).map(|c| {
                Complex32::new(
                    f32::from_le_bytes([c[0], c[1], c[2], c[3]]),
                    f32::from_le_bytes([c[4], c[5], c[6], c[7]]),
                )
            })),
        }
    }
}

/// What the reader thread and the source share.
struct Link {
    stop: AtomicBool,
    dropped: AtomicU64,
    /// Set when the far end went away, with why.
    failure: Mutex<Option<String>>,
    /// An SDR++ server's menu, and how many times it has been sent.
    menu: Mutex<(u64, Vec<crate::smgui::Widget>)>,
}

/// The engine's side of any network input.
pub struct NetSource {
    name: String,
    rate: f64,
    blocks: Receiver<Vec<Complex32>>,
    held: Vec<Complex32>,
    link: Arc<Link>,
    /// For an SDR++ server: the socket to send commands on.
    control: Option<TcpStream>,
}

impl NetSource {
    /// Blocks the reader could not queue because the engine was behind.
    pub fn dropped(&self) -> u64 {
        self.link.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for NetSource {
    fn drop(&mut self) {
        self.link.stop.store(true, Ordering::Relaxed);
        if let Some(c) = &mut self.control {
            let _ = send_command(c, sdrpp::COMMAND_STOP, &[]);
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl Source for NetSource {
    fn rate(&self) -> f64 {
        self.rate
    }
    fn read(&mut self, n: usize) -> Option<Vec<Complex32>> {
        while self.held.len() < n {
            match self.blocks.recv_timeout(Duration::from_millis(500)) {
                Ok(b) => self.held.extend(b),
                Err(RecvTimeoutError::Timeout) => {
                    if self.link.stop.load(Ordering::Relaxed)
                        || self.link.failure.lock().unwrap().is_some()
                    {
                        return None;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => return None,
            }
        }
        let rest = self.held.split_off(n);
        Some(std::mem::replace(&mut self.held, rest))
    }
    fn describe(&self) -> String {
        match &*self.link.failure.lock().unwrap() {
            Some(why) => format!("{} · {why}", self.name),
            None => format!("{} · {:.0} kS/s", self.name, self.rate / 1e3),
        }
    }
    fn live(&self) -> bool {
        true
    }
    fn tune(&mut self, center_hz: f64) {
        if let Some(c) = &mut self.control {
            let _ = send_command(c, sdrpp::COMMAND_SET_FREQUENCY, &center_hz.to_le_bytes());
        }
    }
    fn menu(&self, seen: u64) -> Option<(u64, Vec<crate::smgui::Widget>)> {
        let menu = self.link.menu.lock().unwrap();
        (menu.0 != seen).then(|| menu.clone())
    }
    fn menu_action(&mut self, id: &str, value: &crate::smgui::Value) {
        if let Some(c) = &mut self.control {
            let _ = send_command(
                c,
                sdrpp::COMMAND_UI_ACTION,
                &crate::smgui::action(id, value),
            );
        }
    }
}

fn bad(msg: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidData, msg.into())
}

/// Queue a block for the engine, or drop it and count it.
fn deliver(tx: &SyncSender<Vec<Complex32>>, link: &Link, block: Vec<Complex32>) -> bool {
    match tx.try_send(block) {
        Ok(()) => true,
        Err(TrySendError::Full(_)) => {
            link.dropped.fetch_add(1, Ordering::Relaxed);
            true
        }
        Err(TrySendError::Disconnected(_)) => false,
    }
}

fn send_command(stream: &mut TcpStream, command: u32, data: &[u8]) -> Result<()> {
    let mut p = Vec::with_capacity(12 + data.len());
    p.extend_from_slice(&sdrpp::PACKET_COMMAND.to_le_bytes());
    p.extend_from_slice(&((12 + data.len()) as u32).to_le_bytes());
    p.extend_from_slice(&command.to_le_bytes());
    p.extend_from_slice(data);
    stream.write_all(&p)
}

/// One packet: (type, body after the 8-byte header).
fn read_packet(stream: &mut TcpStream) -> Result<(u32, Vec<u8>)> {
    let mut head = [0u8; 8];
    stream.read_exact(&mut head)?;
    let kind = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    let size = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    if !(8..=64 << 20).contains(&size) {
        return Err(bad(format!("packet of {size} bytes")));
    }
    let mut body = vec![0u8; size - 8];
    stream.read_exact(&mut body)?;
    Ok((kind, body))
}

/// The samples of a baseband packet body (after decompression): sub-header
/// of compression type, sample type and scale, then the samples. Integer
/// samples were scaled so the block's largest value is full scale.
pub fn baseband(body: &[u8], out: &mut Vec<Complex32>) -> Result<()> {
    if body.len() < 8 {
        return Err(bad("short baseband packet"));
    }
    let sample_type = u16::from_le_bytes([body[2], body[3]]);
    let scale = f32::from_le_bytes([body[4], body[5], body[6], body[7]]);
    let data = &body[8..];
    let start = out.len();
    match sample_type {
        sdrpp::PCM_F32 => Format::F32.decode(data, out),
        sdrpp::PCM_I16 => Format::I16.decode(data, out),
        sdrpp::PCM_I8 => Format::I8.decode(data, out),
        t => return Err(bad(format!("sample type {t}"))),
    }
    if sample_type != sdrpp::PCM_F32 {
        for s in &mut out[start..] {
            *s *= scale;
        }
    }
    Ok(())
}

fn unzstd(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder =
        ruzstd::decoding::StreamingDecoder::new(data).map_err(|e| bad(e.to_string()))?;
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

/// Options for an SDR++ server.
#[derive(Debug, Clone, Copy)]
pub struct SdrppOptions {
    /// Ask the server for 8-bit rather than 16-bit samples (half the traffic;
    /// each block is scaled to its peak, so little is lost on a quiet band).
    pub eight_bit: bool,
    /// zstd compression on the server's side.
    pub compressed: bool,
    /// Centre frequency to tune to, Hz; 0 leaves the server's tuning alone.
    pub center_hz: f64,
}

impl Default for SdrppOptions {
    fn default() -> Self {
        Self {
            eight_bit: false,
            compressed: true,
            center_hz: 0.0,
        }
    }
}

/// Connect to an SDR++ server and start its radio.
pub fn open_sdrpp(host: &str, options: SdrppOptions) -> Result<NetSource> {
    let addr = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:{}", sdrpp::DEFAULT_PORT)
    };
    let mut stream = TcpStream::connect(&addr)?;
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    // The server tells its sample rate as soon as a client is in.
    let mut rate = 0.0f64;
    for _ in 0..50 {
        let (kind, body) = read_packet(&mut stream)?;
        if kind == sdrpp::PACKET_COMMAND && body.len() >= 4 {
            match u32::from_le_bytes([body[0], body[1], body[2], body[3]]) {
                sdrpp::COMMAND_SET_SAMPLERATE if body.len() == 12 => {
                    rate = f64::from_le_bytes(body[4..12].try_into().unwrap_or_default());
                    break;
                }
                sdrpp::COMMAND_DISCONNECT => {
                    return Err(bad("the server is busy with another client"));
                }
                _ => {}
            }
        }
    }
    if rate <= 0.0 {
        return Err(bad("the server did not give a sample rate"));
    }
    send_command(&mut stream, sdrpp::COMMAND_GET_UI, &[])?;
    send_command(
        &mut stream,
        sdrpp::COMMAND_SET_SAMPLE_TYPE,
        &[if options.eight_bit { 0 } else { 1 }],
    )?;
    send_command(
        &mut stream,
        sdrpp::COMMAND_SET_COMPRESSION,
        &[u8::from(options.compressed)],
    )?;
    if options.center_hz > 0.0 {
        send_command(
            &mut stream,
            sdrpp::COMMAND_SET_FREQUENCY,
            &options.center_hz.to_le_bytes(),
        )?;
    }
    send_command(&mut stream, sdrpp::COMMAND_START, &[])?;
    // The menu again, as it is with the radio running.
    send_command(&mut stream, sdrpp::COMMAND_GET_UI, &[])?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;

    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    let link = Arc::new(Link {
        stop: AtomicBool::new(false),
        dropped: AtomicU64::new(0),
        failure: Mutex::new(None),
        menu: Mutex::new((0, Vec::new())),
    });
    let mut reader = stream.try_clone()?;
    let shared = link.clone();
    // The rate can change while running (the user changes it at the server):
    // the stream is then no longer what the engine was started for.
    let started_rate = rate;
    std::thread::Builder::new().name("sdrpp-server".into()).spawn(move || {
        let fail = |why: String| *shared.failure.lock().unwrap() = Some(why);
        while !shared.stop.load(Ordering::Relaxed) {
            let (kind, body) = match read_packet(&mut reader) {
                Ok(p) => p,
                Err(e) => {
                    fail(format!("connection lost ({e})"));
                    break;
                }
            };
            let mut block = Vec::new();
            let decoded = match kind {
                sdrpp::PACKET_BASEBAND => baseband(&body, &mut block),
                sdrpp::PACKET_BASEBAND_COMPRESSED => unzstd(&body).and_then(|plain| baseband(&plain, &mut block)),
                // The answer to a menu request or a menu action is the menu.
                sdrpp::PACKET_COMMAND_ACK if body.len() >= 4 => {
                    let command = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
                    if (command == sdrpp::COMMAND_GET_UI || command == sdrpp::COMMAND_UI_ACTION)
                        && let Some(widgets) = crate::smgui::parse(&body[4..])
                    {
                        let mut menu = shared.menu.lock().unwrap();
                        *menu = (menu.0 + 1, widgets);
                    }
                    Ok(())
                }
                sdrpp::PACKET_COMMAND if body.len() >= 4 => {
                    match u32::from_le_bytes([body[0], body[1], body[2], body[3]]) {
                        sdrpp::COMMAND_DISCONNECT => {
                            fail("the server closed the session".into());
                            break;
                        }
                        sdrpp::COMMAND_SET_SAMPLERATE if body.len() == 12 => {
                            let now = f64::from_le_bytes(body[4..12].try_into().unwrap_or_default());
                            if (now - started_rate).abs() > 1.0 {
                                fail(format!("the server changed its sample rate to {:.0} kS/s: reconnect", now / 1e3));
                                break;
                            }
                        }
                        _ => {}
                    }
                    Ok(())
                }
                _ => Ok(()),
            };
            if let Err(e) = decoded {
                fail(format!("bad packet ({e})"));
                break;
            }
            if !block.is_empty() && !deliver(&tx, &shared, block) {
                break;
            }
        }
    })?;
    Ok(NetSource {
        name: format!("SDR++ server {addr}"),
        rate,
        blocks: rx,
        held: Vec::new(),
        link,
        control: Some(stream),
    })
}

/// A plain I/Q stream: `tcp://` connects, `listen://` waits for one sender,
/// `udp://` binds and takes datagrams.
pub fn open_raw(url: &str, rate: f64, format: Format) -> Result<NetSource> {
    let (scheme, addr) = url
        .split_once("://")
        .ok_or_else(|| bad("expected tcp://, listen:// or udp://"))?;
    let (tx, rx) = std::sync::mpsc::sync_channel(64);
    let link = Arc::new(Link {
        stop: AtomicBool::new(false),
        dropped: AtomicU64::new(0),
        failure: Mutex::new(None),
        menu: Mutex::new((0, Vec::new())),
    });
    let shared = link.clone();
    let pair = format.pair_bytes();
    // About a fortieth of a second a block.
    let block_bytes = ((rate / 40.0) as usize).max(256) * pair;
    let fail = move |shared: &Link, why: String| *shared.failure.lock().unwrap() = Some(why);
    match scheme {
        "tcp" | "listen" => {
            let listener = if scheme == "listen" {
                Some(TcpListener::bind(addr)?)
            } else {
                None
            };
            let first = if scheme == "tcp" {
                Some(TcpStream::connect(addr)?)
            } else {
                None
            };
            std::thread::Builder::new()
                .name("net-iq".into())
                .spawn(move || {
                    let mut stream = match (first, &listener) {
                        (Some(s), _) => s,
                        (None, Some(l)) => match l.accept() {
                            Ok((s, _)) => s,
                            Err(e) => return fail(&shared, format!("accept failed ({e})")),
                        },
                        (None, None) => return,
                    };
                    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
                    let mut buf = vec![0u8; block_bytes];
                    while !shared.stop.load(Ordering::Relaxed) {
                        if let Err(e) = stream.read_exact(&mut buf) {
                            return fail(&shared, format!("connection lost ({e})"));
                        }
                        let mut block = Vec::with_capacity(block_bytes / pair);
                        format.decode(&buf, &mut block);
                        if !deliver(&tx, &shared, block) {
                            return;
                        }
                    }
                })?;
        }
        "udp" => {
            let socket = UdpSocket::bind(addr)?;
            socket.set_read_timeout(Some(Duration::from_millis(500)))?;
            std::thread::Builder::new()
                .name("net-iq".into())
                .spawn(move || {
                    let mut buf = vec![0u8; 65_536];
                    let mut carry: Vec<u8> = Vec::new();
                    while !shared.stop.load(Ordering::Relaxed) {
                        let Ok(n) = socket.recv(&mut buf) else {
                            continue;
                        };
                        carry.extend_from_slice(&buf[..n]);
                        if carry.len() >= block_bytes {
                            let whole = carry.len() / pair * pair;
                            let mut block = Vec::with_capacity(whole / pair);
                            format.decode(&carry[..whole], &mut block);
                            carry.drain(..whole);
                            if !deliver(&tx, &shared, block) {
                                return;
                            }
                        }
                    }
                })?;
        }
        other => return Err(bad(format!("unknown scheme {other}://"))),
    }
    Ok(NetSource {
        name: format!("{url} {}", format.label()),
        rate,
        blocks: rx,
        held: Vec::new(),
        link,
        control: None,
    })
}

/// Open any network input from its URL. `rate` and `format` are for plain
/// streams; an SDR++ server gives its own rate.
pub fn open(
    url: &str,
    rate: f64,
    format: Format,
    sdrpp_options: SdrppOptions,
) -> Result<NetSource> {
    match url.strip_prefix("sdrpp://") {
        Some(host) => open_sdrpp(host, sdrpp_options),
        None => open_raw(url, rate, format),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(kind: u32, body: &[u8]) -> Vec<u8> {
        let mut p = kind.to_le_bytes().to_vec();
        p.extend_from_slice(&((8 + body.len()) as u32).to_le_bytes());
        p.extend_from_slice(body);
        p
    }

    /// A stand-in SDR++ server: gives its rate, takes the client's commands,
    /// then sends one 16-bit block and one compressed 8-bit block.
    #[test]
    fn sdrpp_server_session() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut body = sdrpp::COMMAND_SET_SAMPLERATE.to_le_bytes().to_vec();
            body.extend_from_slice(&250_000f64.to_le_bytes());
            s.write_all(&packet(sdrpp::PACKET_COMMAND, &body)).unwrap();
            // GET_UI, SET_SAMPLE_TYPE, SET_COMPRESSION, SET_FREQUENCY, START.
            let mut commands = Vec::new();
            let mut frequency = 0.0;
            for _ in 0..5 {
                let (kind, body) = read_packet(&mut s).unwrap();
                assert_eq!(kind, sdrpp::PACKET_COMMAND);
                let c = u32::from_le_bytes(body[..4].try_into().unwrap());
                if c == sdrpp::COMMAND_SET_FREQUENCY {
                    frequency = f64::from_le_bytes(body[4..12].try_into().unwrap());
                }
                commands.push(c);
            }
            // 16-bit: peak 0.5, so full scale stands for 0.5.
            let mut bb = vec![0u8, 0, 1, 0];
            bb.extend_from_slice(&0.5f32.to_le_bytes());
            for v in [32767i16, 0, -16384, 16384] {
                bb.extend_from_slice(&v.to_le_bytes());
            }
            s.write_all(&packet(sdrpp::PACKET_BASEBAND, &bb)).unwrap();
            // 8-bit, zstd-compressed.
            let mut bb = vec![0u8, 0, 0, 0];
            bb.extend_from_slice(&0.25f32.to_le_bytes());
            bb.extend_from_slice(&[64u8, 0xC0, 0, 127]);
            let z = ruzstd::encoding::compress_to_vec(
                &bb[..],
                ruzstd::encoding::CompressionLevel::Fastest,
            );
            s.write_all(&packet(sdrpp::PACKET_BASEBAND_COMPRESSED, &z))
                .unwrap();
            std::thread::sleep(Duration::from_millis(300));
            (commands, frequency)
        });
        let options = SdrppOptions {
            center_hz: 1_545_000_000.0,
            ..SdrppOptions::default()
        };
        let mut source = open_sdrpp(&addr.to_string(), options).unwrap();
        assert_eq!(source.rate(), 250_000.0);
        assert!(source.live());
        let got = source.read(4).unwrap();
        assert!((got[0] - Complex32::new(0.5, 0.0)).norm() < 1e-4);
        assert!((got[1] - Complex32::new(-0.25, 0.25)).norm() < 1e-4);
        assert!((got[2] - Complex32::new(0.125, -0.125)).norm() < 1e-4);
        assert!((got[3] - Complex32::new(0.0, 0.248)).norm() < 1e-2);
        let (commands, frequency) = server.join().unwrap();
        assert_eq!(
            commands,
            [
                sdrpp::COMMAND_GET_UI,
                sdrpp::COMMAND_SET_SAMPLE_TYPE,
                sdrpp::COMMAND_SET_COMPRESSION,
                sdrpp::COMMAND_SET_FREQUENCY,
                sdrpp::COMMAND_START
            ]
        );
        assert_eq!(frequency, 1_545_000_000.0);
    }

    #[test]
    fn plain_tcp_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let sender = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let bytes: Vec<u8> = (0..40_000u32)
                .flat_map(|i| ((i % 200) as i16 * 100).to_le_bytes())
                .collect();
            s.write_all(&bytes).unwrap();
            std::thread::sleep(Duration::from_millis(200));
        });
        let mut source = open_raw(&format!("tcp://{addr}"), 48_000.0, Format::I16).unwrap();
        let got = source.read(1000).unwrap();
        assert_eq!(got.len(), 1000);
        assert!((got[1].re - 200.0 / 32768.0).abs() < 1e-6);
        assert!((got[1].im - 300.0 / 32768.0).abs() < 1e-6);
        sender.join().unwrap();
    }
}
