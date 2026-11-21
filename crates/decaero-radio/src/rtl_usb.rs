// made by CasualArclamp
//! RTL-SDR, receive only: the `rtl-sdr-rs` crate (pure Rust over USB,
//! MPL-2.0), which knows the RTL-SDR Blog V3 and V4 (the V4's R828D, its
//! 28.8 MHz reference and its input switching). No `rtlsdr.dll` and no
//! libusb: on Windows the dongle needs the WinUSB driver (Zadig), as for any
//! RTL-SDR program.
//!
//! On Android the device arrives as a file descriptor from the system (see
//! `usb_fd` in `lib.rs`); elsewhere the first dongle found is opened.
//!
//! The dongle lives on a thread of its own, which reads blocks and takes
//! tuning and gain changes between them. Rust note: that way nothing here
//! depends on whether the driver's handle may move between threads (`Send`).
//!
//! **Bias tee only when asked**, and switched off again when the radio is
//! let go, as on the other radios.
//!
//! Not tried on a real dongle by whoever wrote it.

use decsat_core::{Error, Iq, Result};
use decsat_io::IqSource;
use rtl_sdr_rs::{RtlSdr, TunerGain};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::time::Duration;

/// Bytes asked of the dongle at a time: 16 384 I/Q pairs, as librtlsdr's
/// tools do (a multiple of the 512-byte USB packet).
const READ_BYTES: usize = 32768;
/// Blocks that may wait for the engine: about a third of a second at 2.4 MS/s.
const QUEUE: usize = 48;

pub struct Settings {
    pub center_hz: u32,
    pub rate: u32,
    pub manual_gain: bool,
    pub gain_tenths: i32,
    pub bias_tee: bool,
    /// Android: the descriptor the system opened. `None`: find the dongle.
    pub fd: Option<crate::UsbFd>,
}

enum Change {
    Center(u32),
    Gain { manual: bool, tenths: i32 },
}

/// Tuning and gain of a running dongle.
pub struct Control {
    changes: Sender<Change>,
}

impl Control {
    pub fn tune(&self, hz: u32) {
        let _ = self.changes.send(Change::Center(hz));
    }
    pub fn gain(&self, manual: bool, tenths: i32) {
        let _ = self.changes.send(Change::Gain { manual, tenths });
    }
}

pub struct Source {
    rate: f64,
    name: String,
    blocks: Receiver<Vec<u8>>,
    changes: Sender<Change>,
    stop: Arc<AtomicBool>,
    lost: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

fn other(what: &str, e: impl std::fmt::Display) -> Error {
    Error::Other(format!("RTL-SDR: {what}: {e}"))
}

/// The tuner's own gain step nearest to what was asked (tenths of a dB).
fn nearest_gain(sdr: &RtlSdr, tenths: i32) -> i32 {
    sdr.get_tuner_gains()
        .ok()
        .and_then(|steps| steps.into_iter().min_by_key(|g| (g - tenths).abs()))
        .unwrap_or(tenths)
}

fn set_gain(sdr: &mut RtlSdr, manual: bool, tenths: i32) -> rtl_sdr_rs::error::Result<()> {
    let gain = if manual {
        TunerGain::Manual(nearest_gain(sdr, tenths))
    } else {
        TunerGain::Auto
    };
    sdr.set_tuner_gain(gain)
}

/// Open and set up the dongle; the name it goes by. The settings end here,
/// and with them our copy of the descriptor (the driver has its own).
fn start(s: Settings) -> Result<(RtlSdr, String)> {
    let mut sdr = match &s.fd {
        #[cfg(unix)]
        Some(fd) => RtlSdr::open_with_fd(std::os::fd::AsRawFd::as_raw_fd(fd)),
        // There is no such value on Windows (see `UsbFd`).
        #[cfg(not(unix))]
        Some(never) => match *never {},
        None => RtlSdr::open_with_index(0),
    }
    .map_err(|e| other("cannot open the dongle", e))?;
    sdr.set_sample_rate(s.rate)
        .map_err(|e| other("sample rate", e))?;
    sdr.set_center_freq(s.center_hz)
        .map_err(|e| other("frequency", e))?;
    set_gain(&mut sdr, s.manual_gain, s.gain_tenths).map_err(|e| other("gain", e))?;
    // Antenna power only when asked for (see the module comment).
    sdr.set_bias_tee(s.bias_tee)
        .map_err(|e| other("bias tee", e))?;
    sdr.reset_buffer().map_err(|e| other("buffer reset", e))?;
    let tuner = sdr.get_tuner_id().unwrap_or("unknown tuner").to_string();
    let name = match sdr.blog_model() {
        rtl_sdr_rs::BlogModel::None => format!("RTL-SDR ({tuner})"),
        model => format!("RTL-SDR Blog {model} ({tuner})"),
    };
    Ok((sdr, name))
}

fn run(
    mut sdr: RtlSdr,
    changes: Receiver<Change>,
    blocks: SyncSender<Vec<u8>>,
    stop: Arc<AtomicBool>,
    lost: Arc<AtomicU64>,
) {
    let mut buf = vec![0u8; READ_BYTES];
    let mut failures = 0;
    while !stop.load(Ordering::Relaxed) {
        for change in changes.try_iter() {
            // A change that fails leaves the radio as it was.
            let _ = match change {
                Change::Center(hz) => sdr.set_center_freq(hz),
                Change::Gain { manual, tenths } => set_gain(&mut sdr, manual, tenths),
            };
        }
        match sdr.read_sync(&mut buf) {
            Ok(n) if n > 0 => {
                failures = 0;
                match blocks.try_send(buf[..n - n % 2].to_vec()) {
                    Ok(()) => {}
                    // The engine is behind: this block is dropped.
                    Err(TrySendError::Full(_)) => {
                        lost.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(TrySendError::Disconnected(_)) => break,
                }
            }
            _ => {
                // Unplugged, most likely: give up after a few tries, and
                // the source then reports the stream as ended.
                failures += 1;
                if failures > 20 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
    let _ = sdr.set_bias_tee(false);
    let _ = sdr.close();
}

impl Source {
    pub fn open(settings: Settings) -> Result<Self> {
        let (changes, changes_rx) = std::sync::mpsc::channel();
        let (blocks_tx, blocks) = std::sync::mpsc::sync_channel(QUEUE);
        let (opened_tx, opened) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicU64::new(0));
        let rate = f64::from(settings.rate);
        let (stop2, lost2) = (stop.clone(), lost.clone());
        let thread =
            std::thread::Builder::new()
                .name("rtl-sdr".into())
                .spawn(move || match start(settings) {
                    Ok((sdr, name)) => {
                        let _ = opened_tx.send(Ok(name));
                        run(sdr, changes_rx, blocks_tx, stop2, lost2);
                    }
                    Err(e) => {
                        let _ = opened_tx.send(Err(e));
                    }
                })?;
        let name = opened
            .recv_timeout(Duration::from_secs(15))
            .map_err(|_| Error::Other("RTL-SDR: no answer from the dongle".into()))??;
        Ok(Self {
            rate,
            name,
            blocks,
            changes,
            stop,
            lost,
            thread: Some(thread),
        })
    }

    pub fn control(&self) -> Control {
        Control {
            changes: self.changes.clone(),
        }
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        // The thread sees this within one block and lets the dongle go;
        // wait for that, so the dongle is free if it is opened again at once.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl IqSource for Source {
    fn read(&mut self, out: &mut Vec<Iq>) -> Result<usize> {
        out.clear();
        match self.blocks.recv_timeout(Duration::from_secs(3)) {
            Ok(block) => {
                // The 8-bit samples are offset binary: 127.5 is zero.
                out.extend(block.chunks_exact(2).map(|p| {
                    Iq::new(
                        (f32::from(p[0]) - 127.5) / 128.0,
                        (f32::from(p[1]) - 127.5) / 128.0,
                    )
                }));
                Ok(out.len())
            }
            Err(RecvTimeoutError::Timeout) => {
                Err(Error::Other("RTL-SDR: no samples for 3 s".into()))
            }
            // The thread has gone: the dongle was unplugged.
            Err(RecvTimeoutError::Disconnected) => Ok(0),
        }
    }

    fn sample_rate(&self) -> f64 {
        self.rate
    }

    fn describe(&self) -> String {
        match self.lost.load(Ordering::Relaxed) {
            0 => self.name.clone(),
            n => format!("{} · {n} blocks lost", self.name),
        }
    }
}
