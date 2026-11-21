// made by CasualArclamp
//! Call recordings: one small Ogg Opus file per call (about 12 kbit/s, so an
//! hour of talk is around 5 MB), named by date, time, frequency and aircraft.
//! Each finished call also gets a line in `calls.csv` beside the day folders.
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
    /// For the call log: when it began (UTC) and where it was.
    began: (i64, u32, u32, u32, u32, u32),
    carrier_hz: f64,
}

pub struct Recorder {
    dir: PathBuf,
    calls: HashMap<ChannelId, CallFile>,
    next_serial: u32,
    /// The newest file written, for the status line.
    pub last: Option<PathBuf>,
    pub error: Option<String>,
    /// The input's centre frequency, for the call log (0 when not known).
    pub center_hz: f64,
}

/// A call that is over, as the call log wants it.
pub struct Ended {
    pub channel: ChannelId,
    pub aes: Option<u32>,
    pub ges: Option<u8>,
    /// Registration and type, where the aircraft is known.
    pub aircraft: String,
}

/// Seconds since 1970 by the system clock.
pub fn unix_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// UTC date and time of day from the system clock: (year, month, day, h, m, s).
fn utc_now() -> (i64, u32, u32, u32, u32, u32) {
    civil(unix_now() as i64)
}

/// Seconds since 1970 as a UTC date and time of day.
pub fn civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
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

impl Ended {
    /// A call closed without its end being seen (it went quiet, or we quit).
    fn unknown(channel: ChannelId) -> Self {
        Self {
            channel,
            aes: None,
            ges: None,
            aircraft: String::new(),
        }
    }
}

impl Recorder {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            calls: HashMap::new(),
            next_serial: 1,
            last: None,
            error: None,
            center_hz: 0.0,
        }
    }

    /// Where recordings go unless told otherwise: Documents\DecAero\calls.
    pub fn default_dir() -> PathBuf {
        if let Some(dir) = crate::data_dir() {
            return dir.join("calls");
        }
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
            began: (y, mo, d, h, mi, s),
            carrier_hz,
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
    pub fn finish(&mut self, end: &Ended) {
        let (channel, aes) = (end.channel, end.aes);
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
        if let Err(e) = self.log(&call.began, call.frames, call.carrier_hz, end, &path) {
            self.error = Some(format!("call log: {e}"));
        }
        self.last = Some(path);
    }

    /// The call log: `calls.csv`, a line per call, opened by any spreadsheet.
    fn log(
        &self,
        began: &(i64, u32, u32, u32, u32, u32),
        frames: u64,
        carrier_hz: f64,
        end: &Ended,
        file: &Path,
    ) -> std::io::Result<()> {
        use std::io::Write;
        let path = self.dir.join("calls.csv");
        let new = !path.exists();
        let mut out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        if new {
            writeln!(
                out,
                "date,time_utc,seconds,mhz,offset_khz,aes,aircraft,ges,file"
            )?;
        }
        let (y, mo, d, h, mi, s) = *began;
        let mhz = if self.center_hz > 0.0 {
            format!("{:.4}", (self.center_hz + carrier_hz) / 1e6)
        } else {
            String::new()
        };
        // The file as a path from the log's own folder.
        let file = file.strip_prefix(&self.dir).unwrap_or(file);
        writeln!(
            out,
            "{y:04}-{mo:02}-{d:02},{h:02}:{mi:02}:{s:02},{:.1},{mhz},{:+.1},{},\"{}\",{},{}",
            frames as f64 * 0.02,
            carrier_hz / 1e3,
            end.aes.map(|a| format!("{a:06X}")).unwrap_or_default(),
            end.aircraft.replace('"', "'"),
            end.ges.map(|g| format!("{g:02X}")).unwrap_or_default(),
            file.display().to_string().replace('\\', "/"),
        )
    }

    /// Is a file open for this channel?
    pub fn recording(&self, channel: ChannelId) -> bool {
        self.calls.contains_key(&channel)
    }

    /// Close files of calls that have gone quiet or are no longer listed.
    pub fn tidy(&mut self, ended: &[Ended]) {
        for end in ended {
            self.finish(end);
        }
        let stale: Vec<ChannelId> = self
            .calls
            .iter()
            .filter(|(_, c)| c.last_audio.elapsed().as_secs() > 15)
            .map(|(id, _)| *id)
            .collect();
        for id in stale {
            self.finish(&Ended::unknown(id));
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
            self.finish(&Ended::unknown(id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_is_saved_and_logged() {
        let dir = std::env::temp_dir().join(format!("decaero-calls-{}", std::process::id()));
        let mut rec = Recorder::new(dir.clone());
        rec.center_hz = 1545e6;
        // Two seconds of a 400 Hz tone on one channel, a little on another.
        let tone: Vec<i16> = (0..16_000)
            .map(|i| ((i as f32 * 0.314).sin() * 8000.0) as i16)
            .collect();
        rec.feed(7, 262_500.0, &tone);
        rec.feed(8, -40_000.0, &tone[..1600]);
        assert!(rec.recording(7) && rec.open_calls() == 2);
        rec.tidy(&[Ended {
            channel: 7,
            aes: Some(0x4CA123),
            ges: Some(0xC1),
            aircraft: "EI-ABC · A333".into(),
        }]);
        assert!(rec.error.is_none(), "{:?}", rec.error);
        let saved = rec.last.clone().unwrap();
        assert!(saved.to_string_lossy().ends_with("_+262.5kHz_4CA123.opus"));
        assert!(std::fs::metadata(&saved).unwrap().len() > 500);
        // The other is closed, with what little is known, when we stop.
        drop(rec);
        let log = std::fs::read_to_string(dir.join("calls.csv")).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3, "{log}");
        assert!(lines[0].starts_with("date,time_utc,seconds,mhz"));
        assert!(
            lines[1].contains(",2.0,1545.2625,+262.5,4CA123,\"EI-ABC · A333\",C1,20"),
            "{log}"
        );
        assert!(lines[1].ends_with("_+262.5kHz_4CA123.opus") && !lines[1].contains('\\'));
        assert!(lines[2].contains(",0.2,1544.9600,-40.0,,\"\",,"), "{log}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
