// made by CasualArclamp
//! Record a part of the band as I/Q: mixed to zero, decimated, and written
//! as a 16-bit stereo WAV (I left, Q right) that this program and SDR++ read
//! back. For keeping a signal to work on later.

use decaero_dsp::ddc::Ddc;
use num_complex::Complex32;
use std::fs::File;
use std::io::{BufWriter, Result, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The sample rate is at least this many times the width asked for: the
/// decimator is flat over the middle quarter of what it puts out.
const OVERSAMPLE: f64 = 4.0;

pub struct IqRecorder {
    path: PathBuf,
    file: BufWriter<File>,
    ddc: Ddc,
    out: Vec<Complex32>,
    /// Set from the first block: its level is put 12 dB under full scale,
    /// and that scale is kept so the level in the file means something.
    scale: Option<f32>,
    frames: u64,
    rate: u32,
    centre_hz: f64,
    closed: bool,
}

impl IqRecorder {
    /// Record `lo_hz..hi_hz` (from the centre of the input) of an input at
    /// `in_rate`, to a new file at `path`.
    pub fn new(path: &Path, in_rate: f64, lo_hz: f64, hi_hz: f64) -> Result<Self> {
        let width = (hi_hz - lo_hz).max(1.0);
        let decim = ((in_rate / (OVERSAMPLE * width)).floor() as usize).max(1);
        let centre_hz = (lo_hz + hi_hz) / 2.0;
        let ddc = Ddc::new(in_rate, centre_hz, decim);
        let rate = ddc.out_rate().round() as u32;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = BufWriter::new(File::create(path)?);
        file.write_all(&header(rate, 0))?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            ddc,
            out: Vec::new(),
            scale: None,
            frames: 0,
            rate,
            centre_hz,
            closed: false,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The file's sample rate, Hz.
    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Where the file's zero frequency is, Hz from the centre of the input.
    pub fn centre_hz(&self) -> f64 {
        self.centre_hz
    }

    pub fn seconds(&self) -> f64 {
        self.frames as f64 / f64::from(self.rate.max(1))
    }

    /// A block of the input.
    pub fn feed(&mut self, block: &[Complex32]) -> Result<()> {
        self.out.clear();
        self.ddc.process(block, &mut self.out);
        if self.out.is_empty() {
            return Ok(());
        }
        let scale = *self.scale.get_or_insert_with(|| {
            let power =
                self.out.iter().map(Complex32::norm_sqr).sum::<f32>() / self.out.len() as f32;
            0.25 * 32767.0 / power.sqrt().max(1e-9)
        });
        let mut bytes = Vec::with_capacity(self.out.len() * 4);
        for s in &self.out {
            for v in [s.re, s.im] {
                let v = (v * scale).round().clamp(-32767.0, 32767.0) as i16;
                bytes.extend_from_slice(&v.to_le_bytes());
            }
        }
        self.frames += self.out.len() as u64;
        self.file.write_all(&bytes)
    }

    /// Write the lengths into the header.
    fn close(&mut self) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.file.flush()?;
        let file = self.file.get_mut();
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&header(self.rate, self.frames))
    }

    /// End the recording: the file is complete after this.
    pub fn finish(mut self) -> Result<PathBuf> {
        self.close()?;
        Ok(self.path.clone())
    }
}

/// A recording dropped without `finish` (the engine was stopped) is still
/// left as a whole file.
impl Drop for IqRecorder {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

/// A 44-byte WAV header: 16-bit PCM, two channels.
fn header(rate: u32, frames: u64) -> [u8; 44] {
    let data = (frames * 4).min(u64::from(u32::MAX - 36)) as u32;
    let mut h = [0u8; 44];
    h[..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data).to_le_bytes());
    h[8..16].copy_from_slice(b"WAVEfmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes());
    h[22..24].copy_from_slice(&2u16.to_le_bytes());
    h[24..28].copy_from_slice(&rate.to_le_bytes());
    h[28..32].copy_from_slice(&(rate * 4).to_le_bytes());
    h[32..34].copy_from_slice(&4u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_is_recorded_at_zero() {
        // A tone 100.3 kHz up in a 1 MS/s input; record 20 kHz around 100 kHz.
        let rate = 1_000_000.0;
        let input: Vec<Complex32> = (0..500_000)
            .map(|i| {
                let (s, c) = (std::f64::consts::TAU * 100_300.0 * i as f64 / rate).sin_cos();
                Complex32::new(c as f32, s as f32) * 0.1
            })
            .collect();
        let path = std::env::temp_dir().join(format!("decaero-iqrec-{}.wav", std::process::id()));
        let mut rec = IqRecorder::new(&path, rate, 90_000.0, 110_000.0).unwrap();
        assert_eq!((rec.rate(), rec.centre_hz()), (83_333, 100_000.0));
        for block in input.chunks(50_000) {
            rec.feed(block).unwrap();
        }
        assert!((rec.seconds() - 0.5).abs() < 0.01);
        let path = rec.finish().unwrap();
        let back = decaero_dsp::wav::read(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(back.sample_rate, 83_333.0);
        assert!(!back.real);
        // The tone is now 300 Hz up: the phase turns that much a sample.
        let s = &back.samples[1000..];
        let turn: Complex32 = s.windows(2).map(|w| w[1] * w[0].conj()).sum();
        let hz = f64::from(turn.im.atan2(turn.re)) / std::f64::consts::TAU * 1_000_000.0 / 12.0;
        assert!((hz - 300.0).abs() < 2.0, "{hz}");
        // A quarter of full scale.
        let level = (s.iter().map(Complex32::norm_sqr).sum::<f32>() / s.len() as f32).sqrt();
        assert!((level - 0.25).abs() < 0.02, "{level}");
    }
}
