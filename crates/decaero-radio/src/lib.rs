// made by CasualArclamp
//! The radios DecAero can drive: the same ones as DecSAT, through DecSAT's
//! own drivers (`decsat-io`). Receive only: nothing here opens a transmit
//! path, and a bias tee is switched on only when the request asks for it.
//!
//! The generic description of a radio (its rates and gain stages, so the GUI
//! can draw controls for any of them) is DecSAT's `decsat-remote/src/radios.rs`
//! (same author, GPL-3), with the RTL-SDR and UHD added.

#[cfg(all(feature = "rtl", any(target_os = "android", target_os = "linux")))]
mod rtl_usb;

use decaero_engine::Source;
use decsat_core::Result;
use decsat_io::IqSource;
use num_complex::Complex32;
use std::sync::Arc;

/// One gain control. A switch is a stage with `max` 1 and `step` 1.
#[derive(Debug, Clone, PartialEq)]
pub struct Gain {
    pub name: String,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub value: f64,
}

/// A radio this build can drive.
#[derive(Debug, Clone, PartialEq)]
pub struct RadioInfo {
    pub key: String,
    pub name: String,
    pub min_hz: f64,
    pub max_hz: f64,
    /// Sample rates offered, Hz, lowest first.
    pub rates: Vec<f64>,
    pub gains: Vec<Gain>,
    /// What the radio can put on its antenna port to feed an LNA, if
    /// anything (the voltage, for the label).
    pub bias_tee: Option<&'static str>,
}

/// What to open.
#[derive(Debug, Clone, PartialEq)]
pub struct RadioRequest {
    pub key: String,
    pub center_hz: f64,
    pub sample_rate: f64,
    /// Stages left out keep the radio's defaults.
    pub gains: Vec<(String, f64)>,
    /// Power on the antenna port for an LNA. Off unless the user asks: it
    /// puts DC on whatever is plugged in.
    pub bias_tee: bool,
}

/// Live controls of an open radio.
pub trait RadioControl: Send + Sync {
    fn set_center(&self, hz: f64) -> Result<()>;
    fn set_gain(&self, name: &str, value: f64) -> Result<()>;
}

/// Asks the system for a USB device with one of these (vendor, product) ids
/// and gives its open file descriptor, or says why not. The descriptor stays
/// the system's: it is duplicated here before use.
pub type UsbOpener = Box<dyn Fn(&[(u16, u16)]) -> std::result::Result<i32, String> + Send + Sync>;

static USB_OPENER: std::sync::OnceLock<UsbOpener> = std::sync::OnceLock::new();

/// Android only lets a program open a USB device through its own permission
/// dialog, which is Java's business: the app hands in the way to ask. Where
/// none is set (a desktop) the drivers find their radios themselves.
pub fn set_usb_opener(opener: UsbOpener) {
    let _ = USB_OPENER.set(opener);
}

/// The radio's descriptor from the system, where the system has to be asked.
#[cfg(any(target_os = "android", target_os = "linux"))]
#[allow(dead_code)] // used by whichever radios are compiled in
fn usb_fd(ids: &[(u16, u16)]) -> Result<Option<std::os::fd::OwnedFd>> {
    use std::os::fd::BorrowedFd;
    let Some(opener) = USB_OPENER.get() else {
        return Ok(None);
    };
    let fd = opener(ids).map_err(bad)?;
    // SAFETY: the opener has just returned this descriptor as open, and it
    // stays open (the app keeps the connection) while we duplicate it.
    //
    // Rust note: `BorrowedFd` is a descriptor we may use but not close;
    // `try_clone_to_owned` is `dup`, giving one that is ours to close.
    let ours = unsafe { BorrowedFd::borrow_raw(fd) }.try_clone_to_owned()?;
    Ok(Some(ours))
}

/// USB ids of the RTL2832U dongles in common use (librtlsdr's `known_devices`:
/// the generic ids, which the RTL-SDR Blog V3 and V4 use too).
#[cfg(any(target_os = "android", target_os = "linux"))]
#[allow(dead_code)]
const RTL_IDS: [(u16, u16); 2] = [(0x0bda, 0x2838), (0x0bda, 0x2832)];

fn bad(msg: impl Into<String>) -> decsat_core::Error {
    decsat_core::Error::Other(msg.into())
}

#[allow(dead_code)] // used by whichever radios are compiled in
fn value(gains: &[Gain], name: &str) -> f64 {
    gains
        .iter()
        .find(|g| g.name == name)
        .map_or(0.0, |g| g.value)
}

#[allow(dead_code)]
fn stage(name: &str, min: f64, max: f64, step: f64, value: f64) -> Gain {
    Gain {
        name: name.into(),
        min,
        max,
        step,
        value,
    }
}

/// Every radio this build can drive. Listing them touches no hardware.
// Each push is behind its own feature, so this cannot be one `vec![]`.
#[allow(clippy::vec_init_then_push)]
pub fn list() -> Vec<RadioInfo> {
    #[allow(unused_mut)]
    let mut v: Vec<RadioInfo> = Vec::new();
    #[cfg(feature = "hackrf")]
    v.push(RadioInfo {
        key: "hackrf".into(),
        name: "HackRF One".into(),
        min_hz: 1e6,
        max_hz: 6e9,
        rates: vec![2e6, 4e6, 8e6, 10e6, 12.5e6, 16e6, 20e6],
        gains: hackrf_gains(),
        bias_tee: Some("3.3 V"),
    });
    #[cfg(feature = "airspy")]
    v.push(RadioInfo {
        key: "airspy".into(),
        name: "Airspy Mini / R2 / HydraSDR".into(),
        min_hz: decsat_io::airspy::AIRSPY_MIN_HZ as f64,
        max_hz: decsat_io::airspy::AIRSPY_MAX_HZ as f64,
        rates: decsat_io::AIRSPY_RATES
            .iter()
            .map(|r| f64::from(*r))
            .collect(),
        gains: airspy_gains(),
        bias_tee: Some("4.5 V"),
    });
    #[cfg(feature = "pluto")]
    v.push(RadioInfo {
        key: "pluto".into(),
        name: "ADALM-PLUTO / Pluto+".into(),
        min_hz: 70e6,
        max_hz: 6e9,
        rates: vec![2e6, 4e6, 64e6 / 7.0, 16e6, 20e6],
        gains: pluto_gains(),
        bias_tee: None,
    });
    #[cfg(feature = "sdrplay")]
    v.push(RadioInfo {
        key: "sdrplay".into(),
        name: "SDRplay RSPduo / RSP".into(),
        min_hz: decsat_io::sdrplay::SDRPLAY_MIN_HZ as f64,
        max_hz: decsat_io::sdrplay::SDRPLAY_MAX_HZ as f64,
        rates: decsat_io::SDRPLAY_RATES
            .iter()
            .map(|r| f64::from(*r))
            .collect(),
        gains: sdrplay_gains(),
        bias_tee: Some("where the model has one"),
    });
    #[cfg(feature = "fobos")]
    v.push(RadioInfo {
        key: "fobos".into(),
        name: "Fobos SDR".into(),
        min_hz: decsat_io::fobos::FOBOS_MIN_HZ as f64,
        max_hz: 6e9,
        rates: decsat_io::FOBOS_RATES
            .iter()
            .map(|r| f64::from(*r))
            .collect(),
        gains: fobos_gains(),
        bias_tee: None,
    });
    #[cfg(all(
        feature = "rtl",
        any(windows, target_os = "android", target_os = "linux")
    ))]
    v.push(RadioInfo {
        key: "rtl".into(),
        name: "RTL-SDR (V3, V4)".into(),
        min_hz: 24e6,
        max_hz: 1.766e9,
        rates: vec![1.024e6, 1.4e6, 1.8e6, 2.048e6, 2.4e6],
        gains: vec![
            stage("auto", 0.0, 1.0, 1.0, 0.0),
            stage("gain", 0.0, 49.6, 0.1, 40.0),
        ],
        bias_tee: Some("4.5 V"),
    });
    #[cfg(all(windows, feature = "uhd"))]
    v.push(RadioInfo {
        key: "uhd".into(),
        name: "USRP (UHD)".into(),
        min_hz: 10e6,
        max_hz: 6e9,
        rates: vec![1e6, 2e6, 4e6, 5e6, 8e6, 10e6],
        gains: vec![stage("gain", 0.0, 76.0, 1.0, 40.0)],
        bias_tee: None,
    });
    for r in &mut v {
        r.rates.sort_by(f64::total_cmp);
    }
    v
}

#[cfg(feature = "hackrf")]
fn hackrf_gains() -> Vec<Gain> {
    let d = decsat_io::HackRfGains::default();
    vec![
        stage("amp", 0.0, 1.0, 1.0, f64::from(u8::from(d.amp))),
        stage("lna", 0.0, 40.0, 8.0, f64::from(d.lna_db)),
        stage("vga", 0.0, 62.0, 2.0, f64::from(d.vga_db)),
    ]
}

#[cfg(feature = "hackrf")]
fn hackrf_of(g: &[Gain]) -> decsat_io::HackRfGains {
    decsat_io::HackRfGains {
        amp: value(g, "amp") >= 0.5,
        lna_db: value(g, "lna") as u16,
        vga_db: value(g, "vga") as u16,
    }
}

#[cfg(feature = "airspy")]
fn airspy_gains() -> Vec<Gain> {
    let d = decsat_io::AirspyGains::default();
    vec![
        stage("sensitivity", 0.0, 1.0, 1.0, 0.0),
        stage("gain", 0.0, 21.0, 1.0, f64::from(d.step)),
    ]
}

#[cfg(feature = "airspy")]
fn airspy_of(g: &[Gain]) -> decsat_io::AirspyGains {
    decsat_io::AirspyGains {
        mode: if value(g, "sensitivity") >= 0.5 {
            decsat_io::AirspyGainMode::Sensitivity
        } else {
            decsat_io::AirspyGainMode::Linearity
        },
        step: value(g, "gain") as u8,
    }
}

#[cfg(feature = "fobos")]
fn fobos_gains() -> Vec<Gain> {
    let d = decsat_io::FobosGains::default();
    vec![
        stage("lna", 0.0, 3.0, 1.0, f64::from(d.lna)),
        stage("vga", 0.0, 31.0, 1.0, f64::from(d.vga)),
    ]
}

#[cfg(feature = "fobos")]
fn fobos_of(g: &[Gain]) -> decsat_io::FobosGains {
    decsat_io::FobosGains {
        lna: value(g, "lna") as u8,
        vga: value(g, "vga") as u8,
    }
}

/// Both SDRplay stages are gain *reductions* (as in the API): a lower number
/// is more gain.
#[cfg(feature = "sdrplay")]
fn sdrplay_gains() -> Vec<Gain> {
    let d = decsat_io::SdrplayGains::default();
    vec![
        stage("lna_state", 0.0, 9.0, 1.0, f64::from(d.lna)),
        stage("if_gr", 20.0, 59.0, 1.0, f64::from(d.gr)),
        // 0 = Tuner A, 1 = Tuner B (RSPduo); read when the radio opens.
        stage("tuner_b", 0.0, 1.0, 1.0, 0.0),
    ]
}

#[cfg(feature = "sdrplay")]
fn sdrplay_of(g: &[Gain]) -> decsat_io::SdrplayGains {
    decsat_io::SdrplayGains {
        lna: value(g, "lna_state") as u8,
        gr: value(g, "if_gr") as u8,
    }
}

#[cfg(feature = "pluto")]
fn pluto_gains() -> Vec<Gain> {
    vec![
        stage("agc", 0.0, 1.0, 1.0, 0.0),
        stage("gain", -1.0, 73.0, 1.0, 50.0),
    ]
}

#[cfg(feature = "pluto")]
fn pluto_of(g: &[Gain]) -> decsat_io::PlutoGain {
    if value(g, "agc") >= 0.5 {
        decsat_io::PlutoGain::Agc
    } else {
        decsat_io::PlutoGain::Manual(value(g, "gain"))
    }
}

/// Keeps a radio's gain stages so one can change at a time: the drivers set
/// them all together.
struct Live<C> {
    ctl: C,
    gains: std::sync::Mutex<Vec<Gain>>,
    center: fn(&C, f64) -> Result<()>,
    apply: fn(&C, &[Gain]) -> Result<()>,
}

impl<C: Send + Sync> RadioControl for Live<C> {
    fn set_center(&self, hz: f64) -> Result<()> {
        (self.center)(&self.ctl, hz)
    }

    fn set_gain(&self, name: &str, v: f64) -> Result<()> {
        let mut g = self.gains.lock().unwrap();
        let s = g
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| bad(format!("no gain called {name}")))?;
        s.value = v.clamp(s.min, s.max);
        (self.apply)(&self.ctl, &g)
    }
}

/// An open radio as an input for the engine.
pub struct RadioSource {
    name: String,
    inner: Box<dyn IqSource>,
    control: Option<Arc<dyn RadioControl>>,
    held: Vec<Complex32>,
    scratch: Vec<Complex32>,
    failure: Option<String>,
}

impl RadioSource {
    /// A handle for changing gain while the engine owns the source.
    pub fn control(&self) -> Option<Arc<dyn RadioControl>> {
        self.control.clone()
    }
}

impl Source for RadioSource {
    fn rate(&self) -> f64 {
        self.inner.sample_rate()
    }
    fn read(&mut self, n: usize) -> Option<Vec<Complex32>> {
        while self.held.len() < n {
            match self.inner.read(&mut self.scratch) {
                Ok(0) => return None,
                Ok(_) => self.held.extend_from_slice(&self.scratch),
                Err(e) => {
                    self.failure = Some(e.to_string());
                    return None;
                }
            }
        }
        let rest = self.held.split_off(n);
        Some(std::mem::replace(&mut self.held, rest))
    }
    fn describe(&self) -> String {
        match &self.failure {
            Some(why) => format!("{} · {why}", self.name),
            None => format!("{} · {:.3} MS/s", self.name, self.inner.sample_rate() / 1e6),
        }
    }
    fn live(&self) -> bool {
        true
    }
    fn tune(&mut self, center_hz: f64) {
        if let Some(c) = &self.control
            && let Err(e) = c.set_center(center_hz)
        {
            self.failure = Some(e.to_string());
        }
    }
}

/// Open a radio. This takes the device: do it only when the user asks.
pub fn open(req: &RadioRequest) -> Result<RadioSource> {
    let info = list()
        .into_iter()
        .find(|r| r.key == req.key)
        .ok_or_else(|| bad(format!("no radio called {}", req.key)))?;
    if !(info.min_hz..=info.max_hz).contains(&req.center_hz) {
        return Err(bad(format!(
            "{} tunes {:.0} to {:.0} MHz: set the centre frequency",
            info.name,
            info.min_hz / 1e6,
            info.max_hz / 1e6
        )));
    }
    let mut gains = info.gains.clone();
    for (name, v) in &req.gains {
        if let Some(g) = gains.iter_mut().find(|g| g.name == *name) {
            g.value = v.clamp(g.min, g.max);
        }
    }
    let (inner, control): (Box<dyn IqSource>, Option<Arc<dyn RadioControl>>) =
        match req.key.as_str() {
            #[cfg(feature = "hackrf")]
            "hackrf" => {
                let s = decsat_io::HackRfSource::open(decsat_io::HackRfSettings {
                    center_hz: req.center_hz as u64,
                    sample_rate: req.sample_rate as u32,
                    gains: hackrf_of(&gains),
                    bias_tee: req.bias_tee && info.bias_tee.is_some(),
                })?;
                let live = Live {
                    ctl: s.control(),
                    gains: gains.into(),
                    center: |c: &decsat_io::HackRfControl, hz| c.set_center(hz as u64),
                    apply: |c: &decsat_io::HackRfControl, g| c.set_gains(hackrf_of(g)),
                };
                (Box::new(s), Some(Arc::new(live)))
            }
            #[cfg(feature = "airspy")]
            "airspy" => {
                // Android: the system opens it, and the driver is told.
                #[cfg(target_os = "android")]
                if let Some(fd) = usb_fd(&[(0x1d50, 0x60a1)])? {
                    libairspy_rs::offer_fd(fd);
                }
                let s = decsat_io::AirspySource::open(decsat_io::AirspySettings {
                    center_hz: req.center_hz as u64,
                    sample_rate: req.sample_rate as u32,
                    gains: airspy_of(&gains),
                    bias_tee: req.bias_tee && info.bias_tee.is_some(),
                })?;
                let live = Live {
                    ctl: s.control(),
                    gains: gains.into(),
                    center: |c: &decsat_io::AirspyControl, hz| c.set_center(hz as u64),
                    apply: |c: &decsat_io::AirspyControl, g| c.set_gains(airspy_of(g)),
                };
                (Box::new(s), Some(Arc::new(live)))
            }
            #[cfg(feature = "fobos")]
            "fobos" => {
                let s = decsat_io::FobosSource::open(decsat_io::FobosSettings {
                    center_hz: req.center_hz as u64,
                    sample_rate: req.sample_rate as u32,
                    gains: fobos_of(&gains),
                    external_clock: false,
                })?;
                let live = Live {
                    ctl: s.control(),
                    gains: gains.into(),
                    center: |c: &decsat_io::FobosControl, hz| c.set_center(hz as u64),
                    apply: |c: &decsat_io::FobosControl, g| c.set_gains(fobos_of(g)),
                };
                (Box::new(s), Some(Arc::new(live)))
            }
            #[cfg(feature = "sdrplay")]
            "sdrplay" => {
                let s = decsat_io::SdrplaySource::open(decsat_io::SdrplaySettings {
                    center_hz: req.center_hz as u64,
                    sample_rate: req.sample_rate as u32,
                    gains: sdrplay_of(&gains),
                    tuner_b: value(&gains, "tuner_b") >= 0.5,
                    bias_tee: req.bias_tee && info.bias_tee.is_some(),
                })?;
                let live = Live {
                    ctl: s.control(),
                    gains: gains.into(),
                    center: |c: &decsat_io::SdrplayControl, hz| c.set_center(hz as u64),
                    apply: |c: &decsat_io::SdrplayControl, g| c.set_gains(sdrplay_of(g)),
                };
                (Box::new(s), Some(Arc::new(live)))
            }
            #[cfg(feature = "pluto")]
            "pluto" => {
                let s = decsat_io::PlutoSource::open(decsat_io::PlutoSettings {
                    center_hz: req.center_hz,
                    sample_rate: req.sample_rate,
                    gain: pluto_of(&gains),
                    ..Default::default()
                })?;
                let live = Live {
                    ctl: s.control(),
                    gains: gains.into(),
                    center: |c: &decsat_io::PlutoControl, hz| c.set_center(hz),
                    apply: |c: &decsat_io::PlutoControl, g| c.set_gain(pluto_of(g)),
                };
                (Box::new(s), Some(Arc::new(live)))
            }
            #[cfg(all(windows, feature = "rtl"))]
            "rtl" => {
                let s = decsat_io::RtlSource::open(decsat_io::RtlSettings {
                    center_hz: req.center_hz as u32,
                    rate: req.sample_rate as u32,
                    gain_tenths: (value(&gains, "gain") * 10.0) as i32,
                    manual_gain: value(&gains, "auto") < 0.5,
                    bias_tee: req.bias_tee && info.bias_tee.is_some(),
                    ..Default::default()
                })?;
                let live = Live {
                    ctl: s.control(),
                    gains: gains.into(),
                    center: |c: &decsat_io::RtlControl, hz| {
                        c.tune(hz as u32);
                        Ok(())
                    },
                    apply: |c: &decsat_io::RtlControl, g| {
                        c.gain(value(g, "auto") < 0.5, (value(g, "gain") * 10.0) as i32);
                        Ok(())
                    },
                };
                (Box::new(s), Some(Arc::new(live)))
            }
            #[cfg(all(feature = "rtl", any(target_os = "android", target_os = "linux")))]
            "rtl" => {
                let s = rtl_usb::Source::open(rtl_usb::Settings {
                    center_hz: req.center_hz as u32,
                    rate: req.sample_rate as u32,
                    gain_tenths: (value(&gains, "gain") * 10.0) as i32,
                    manual_gain: value(&gains, "auto") < 0.5,
                    bias_tee: req.bias_tee && info.bias_tee.is_some(),
                    fd: usb_fd(&RTL_IDS)?,
                })?;
                let live = Live {
                    ctl: s.control(),
                    gains: gains.into(),
                    center: |c: &rtl_usb::Control, hz| {
                        c.tune(hz as u32);
                        Ok(())
                    },
                    apply: |c: &rtl_usb::Control, g| {
                        c.gain(value(g, "auto") < 0.5, (value(g, "gain") * 10.0) as i32);
                        Ok(())
                    },
                };
                (Box::new(s), Some(Arc::new(live)))
            }
            #[cfg(all(windows, feature = "uhd"))]
            "uhd" => {
                let s = decsat_io::UhdSource::open(decsat_io::UhdSettings {
                    center_hz: req.center_hz,
                    rate: req.sample_rate,
                    gain: value(&gains, "gain"),
                    ..Default::default()
                })?;
                let live = Live {
                    ctl: s.control(),
                    gains: gains.into(),
                    center: |c: &decsat_io::UhdControl, hz| {
                        c.tune(hz);
                        Ok(())
                    },
                    apply: |c: &decsat_io::UhdControl, g| {
                        c.gain(value(g, "gain"));
                        Ok(())
                    },
                };
                (Box::new(s), Some(Arc::new(live)))
            }
            other => return Err(bad(format!("{other} is not built into this program"))),
        };
    Ok(RadioSource {
        name: info.name,
        inner,
        control,
        held: Vec::new(),
        scratch: Vec::new(),
        failure: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Listing must not touch hardware and every radio must be usable for
    /// L-band Aero (1545 MHz) and offer at least one rate.
    #[test]
    fn every_radio_is_described() {
        let radios = list();
        assert!(!radios.is_empty());
        for r in &radios {
            assert!(!r.rates.is_empty(), "{}", r.key);
            assert!(r.rates.windows(2).all(|w| w[0] <= w[1]));
            assert!(
                (r.min_hz..=r.max_hz).contains(&1.545e9),
                "{} cannot tune L-band",
                r.key
            );
            for g in &r.gains {
                assert!(g.min <= g.value && g.value <= g.max, "{} {}", r.key, g.name);
            }
        }
    }

    #[test]
    fn an_unknown_radio_or_a_missing_frequency_is_refused() {
        let ask = |key: &str, center_hz: f64| RadioRequest {
            key: key.into(),
            center_hz,
            sample_rate: 2e6,
            gains: vec![],
            bias_tee: false,
        };
        assert!(open(&ask("no-such-radio", 1.545e9)).is_err());
        // Refused before any hardware is touched.
        assert!(open(&ask("hackrf", 0.0)).is_err());
    }
}
