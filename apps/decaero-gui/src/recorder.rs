// made by CasualArclamp
//! Call recordings: one small Ogg Opus file per call (about 12 kbit/s, so an
//! hour of talk is around 5 MB), named by date, time, frequency and aircraft.
//!
//! The Ogg wrapping follows RFC 7845: an `OpusHead` packet, an `OpusTags`
//! packet, then 20 ms audio packets with the position counted at 48 kHz.

use decaero_engine::channel::ChannelId;
use ogg::writing::{PacketWriteEndInfo, PacketWriter};
use std::collections::HashMap;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Samples in a 20 ms frame at 8 kHz.
const FRAME: usize = 160;

struct CallFile {
    writer: PacketWriter<'static, BufWriter<File>>,
    encoder: opus::Encoder,
    serial: u32,
    /// Position at 48 kHz, as Ogg Opus counts it.
    granule: u64,
    pending: Vec<i16>,
    path: PathBuf,
    last_audio: Instant,
    frames: u64,
}

pub struct Recorder {
    dir: PathBuf,
    calls: HashMap<ChannelId, CallFile>,
    next_serial: u32,
    /// The newest file written, for the status line.
    pub last: Option<PathBuf>,
    pub error: Option<String>,
}

/// UTC date and time of day from the system clock: (year, month, day, h, m, s).
fn utc_now() -> (i64, u32, u32, u32, u32, u32) {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400) as u32);
    // Days to a calendar date (Howard Hinnant's civil-from-days).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, rem / 3600, rem / 60 % 60, rem % 60)
}

impl Recorder {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            calls: HashMap::new(),
            next_serial: 1,
            last: None,
            error: None,
        }
    }

    /// Where recordings go unless told otherwise: Documents\DecAero\calls.
    pub fn default_dir() -> PathBuf {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from);
        home.map(|h| h.join("Documents"))
            .unwrap_or_default()
            .join("DecAero")
            .join("calls")
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn open(&mut self, carrier_hz: f64) -> Result<CallFile, String> {
        let (y, mo, d, h, mi, s) = utc_now();
        let folder = self.dir.join(format!("{y:04}-{mo:02}-{d:02}"));
        std::fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        let path = folder.join(format!(
            "{h:02}{mi:02}{s:02}Z_{:+.1}kHz.opus",
            carrier_hz / 1e3
        ));
        let file = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut encoder = opus::Encoder::new(8000, opus::Channels::Mono, opus::Application::Voip)
            .map_err(|e| e.to_string())?;
        encoder
            .set_bitrate(opus::Bitrate::Bits(12_000))
            .map_err(|e| e.to_string())?;
        let serial = self.next_serial;
        self.next_serial += 1;
        let mut writer = PacketWriter::new(BufWriter::new(file));
        // OpusHead: version 1, one channel, pre-skip (the encoder's delay at
        // 48 kHz), the original sample rate, no gain, channel mapping 0.
        let pre_skip = (encoder.get_lookahead().unwrap_or(0).max(0) as u16) * 6;
        let mut head = b"OpusHead".to_vec();
        head.push(1);
        head.push(1);
        head.extend_from_slice(&pre_skip.to_le_bytes());
        head.extend_from_slice(&8000u32.to_le_bytes());
        head.extend_from_slice(&0i16.to_le_bytes());
        head.push(0);
        writer
            .write_packet(head, serial, PacketWriteEndInfo::EndPage, 0)
            .map_err(|e| e.to_string())?;
        let vendor = b"DecAero";
        let mut tags = b"OpusTags".to_vec();
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&0u32.to_le_bytes());
        writer
            .write_packet(tags, serial, PacketWriteEndInfo::EndPage, 0)
            .map_err(|e| e.to_string())?;
        Ok(CallFile {
            writer,
            encoder,
            serial,
            granule: 0,
            pending: Vec::new(),
            path,
            last_audio: Instant::now(),
            frames: 0,
        })
    }

    /// Voice from a call (8 kHz). A file is started with the first of it.
    pub fn feed(&mut self, channel: ChannelId, carrier_hz: f64, pcm: &[i16]) {
        if !self.calls.contains_key(&channel) {
            match self.open(carrier_hz) {
                Ok(c) => {
                    self.calls.insert(channel, c);
                }
                Err(e) => {
                    self.error = Some(e);
                    return;
                }
            }
        }
        let Some(call) = self.calls.get_mut(&channel) else {
            return;
        };
        call.pending.extend_from_slice(pcm);
        call.last_audio = Instant::now();
        let mut packet = [0u8; 400];
        while call.pending.len() >= FRAME {
            let frame: Vec<i16> = call.pending.drain(..FRAME).collect();
            match call.encoder.encode(&frame, &mut packet) {
                Ok(n) => {
                    call.granule += 960;
                    call.frames += 1;
                    // A page every second keeps the file playable if we stop hard.
                    let end = if call.frames.is_multiple_of(50) {
                        PacketWriteEndInfo::EndPage
                    } else {
                        PacketWriteEndInfo::NormalPacket
                    };
                    if let Err(e) = call.writer.write_packet(
                        packet[..n].to_vec(),
                        call.serial,
                        end,
                        call.granule,
                    ) {
                        self.error = Some(e.to_string());
                    }
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
    }

    /// The call is over: close its file, adding the aircraft to the name.
    pub fn finish(&mut self, channel: ChannelId, aes: Option<u32>) {
        let Some(mut call) = self.calls.remove(&channel) else {
            return;
        };
        // One last frame of silence carries the end-of-stream mark.
        let mut packet = [0u8; 400];
        if let Ok(n) = call.encoder.encode(&[0i16; FRAME], &mut packet) {
            call.granule += 960;
            let _ = call.writer.write_packet(
                packet[..n].to_vec(),
                call.serial,
                PacketWriteEndInfo::EndStream,
                call.granule,
            );
        }
        drop(call.writer);
        let mut path = call.path;
        if let Some(aes) = aes {
            let named = path.with_file_name(format!(
                "{}_{aes:06X}.opus",
                path.file_stem().and_then(|s| s.to_str()).unwrap_or("call")
            ));
            if std::fs::rename(&path, &named).is_ok() {
                path = named;
            }
        }
        self.last = Some(path);
    }

    /// Close files of calls that have gone quiet or are no longer listed.
    pub fn tidy(&mut self, ended: &[(ChannelId, Option<u32>)]) {
        for (channel, aes) in ended {
            self.finish(*channel, *aes);
        }
        let stale: Vec<ChannelId> = self
            .calls
            .iter()
            .filter(|(_, c)| c.last_audio.elapsed().as_secs() > 15)
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.finish(id, None);
        }
    }

    pub fn open_calls(&self) -> usize {
        self.calls.len()
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let open: Vec<ChannelId> = self.calls.keys().copied().collect();
        for id in open {
            self.finish(id, None);
        }
    }
}
