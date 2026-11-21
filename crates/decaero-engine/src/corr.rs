// made by CasualArclamp
//! A channel worker's path for the L-band correction carriers
//! (`decaero_core::corr`): plain BPSK, frames found and decoded down to the
//! clear header, the encrypted payload shown only as its first bytes.
//!
//! The worker comes here when a carrier has no Aero line pair but does give
//! a plain BPSK carrier line and a clock line at one of the correction
//! rates (`channel::run`).

use crate::channel::{ChannelId, ChannelStatus, Event, Kind, es_n0_for_ber};
use decaero_core::corr::{self, FrameDecoder, StreamCheck, StreamPiece};
use decaero_dsp::acquire::{self, FoundBpsk};
use decaero_dsp::ddc::Ddc;
use decaero_dsp::demod::{Demod, Modulation};
use num_complex::Complex32;
use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

/// Line strengths (amplitude over the median) that name a correction carrier.
const CARRIER_LINE: f32 = 8.0;
const CLOCK_LINE: f32 = 4.0;

/// Look for a correction carrier in the blocks a worker kept while it
/// searched for Aero. `offset_hz` is where the worker was put, `width_hz`
/// how wide the carrier looked there: a rate the width cannot hold is not
/// looked for (a plain tone gives a squared line too, and a modulated
/// neighbour in the window a clock line, which together fooled the search
/// on a narrow spur 5 kHz from a 2438 carrier).
pub(crate) fn find(
    kept: &[Arc<Vec<Complex32>>],
    in_rate: f64,
    offset_hz: f64,
    search_hz: f64,
    width_hz: f64,
) -> Option<FoundBpsk> {
    // Measured widths 6 dB down: 1.14 times the symbol rate on the framed
    // carriers, 0.94 on the 1200 ones. Half the rate is the floor.
    let rates: Vec<f64> = corr::ALL_RATES
        .iter()
        .copied()
        .filter(|&r| width_hz >= 0.5 * r)
        .collect();
    if rates.is_empty() {
        return None;
    }
    // Four samples a symbol of the fastest rate: its clock line is then
    // well inside the band.
    let mut ddc = Ddc::new(
        in_rate,
        offset_hz,
        Ddc::decimation_for(in_rate, corr::ALL_RATES[0], 4.0),
    );
    let mut wide = Vec::new();
    for b in kept {
        ddc.process(b, &mut wide);
    }
    acquire::find_bpsk(&wide, ddc.out_rate(), &rates, search_hz.min(2000.0))
        .filter(|f| f.carrier_strength >= CARRIER_LINE && f.clock_strength >= CLOCK_LINE)
}

/// The kind a correction carrier is shown as: its symbol rate, whole.
fn kind_for(symbol_rate: f64) -> Kind {
    Kind::Corr(symbol_rate.round() as u32)
}

pub(crate) struct CorrRunning {
    ddc: Ddc,
    demod: Demod,
    kind: Kind,
    framed: Option<FrameDecoder>,
    stream: Option<StreamCheck>,
    /// Raw bit errors and bits of recent frames (a running average).
    recent_errors: f32,
    recent_bits: f32,
    /// Stream carriers: pieces with code lock, and whether the last had it.
    pieces_locked: u64,
    stream_locked: bool,
    corrected: u64,
    /// The first bytes of each payload seen, and when (repeats are named).
    seen: HashMap<[u8; 8], f64>,
    /// A frame or code lock has been seen: until then the channel is shown
    /// as still searching, and it is given up at `deadline` (seconds of
    /// input). Tones and the skirts of neighbours can pass the line test;
    /// they never give a frame.
    confirmed: bool,
    deadline: f64,
    gave_up: bool,
}

pub(crate) fn start(
    in_rate: f64,
    carrier: f64,
    found: FoundBpsk,
    started_s: f64,
    status: &Arc<Mutex<ChannelStatus>>,
) -> CorrRunning {
    let ddc = Ddc::new(
        in_rate,
        carrier,
        Ddc::decimation_for(in_rate, found.symbol_rate, 8.0),
    );
    let demod = Demod::with_rate(
        Modulation::Bpsk {
            roll_off: corr::roll_off(found.symbol_rate),
        },
        found.symbol_rate,
        ddc.out_rate(),
        0.0,
    );
    let kind = kind_for(found.symbol_rate);
    {
        let mut s = status.lock().unwrap();
        s.carrier_hz = carrier;
        s.acquired_hz = carrier;
    }
    let framed = corr::framed(found.symbol_rate);
    // Three and a bit frames, or a dozen seconds of pieces.
    let patience = if framed {
        3.2 * corr::FRAME_SYMBOLS as f64 / found.symbol_rate
    } else {
        12.0
    };
    CorrRunning {
        confirmed: false,
        deadline: started_s + patience,
        gave_up: false,
        ddc,
        demod,
        kind,
        framed: framed.then(FrameDecoder::new),
        stream: (!framed).then(StreamCheck::new),
        recent_errors: 0.0,
        recent_bits: 0.0,
        pieces_locked: 0,
        stream_locked: false,
        corrected: 0,
        seen: HashMap::new(),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

fn clock(s: f64) -> String {
    let s = s.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

impl CorrRunning {
    /// No frame and no code lock came in time: not a correction carrier.
    pub(crate) fn gave_up(&self) -> bool {
        self.gave_up
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn process(
        &mut self,
        block: &[Complex32],
        seconds: f64,
        id: ChannelId,
        status: &Arc<Mutex<ChannelStatus>>,
        events: &Sender<Event>,
        narrow: &mut Vec<Complex32>,
        soft: &mut Vec<f32>,
    ) {
        narrow.clear();
        soft.clear();
        self.ddc.process(block, narrow);
        for piece in narrow.chunks(512) {
            self.demod.process(piece, soft);
        }
        let (mut errors, mut bits, mut new_frames) = (0u32, 0u32, 0u64);
        let mut header: Option<(u16, u8, String)> = None;
        let locked;
        let mut totals = None;
        if let Some(dec) = self.framed.as_mut() {
            let mut frames = Vec::new();
            dec.push(soft, &mut frames);
            for f in &frames {
                errors += f.channel_errors;
                bits += f.channel_bits;
                new_frames += 1;
                let head = f.payload_head(8);
                let key: [u8; 8] = std::array::from_fn(|i| head.get(i).copied().unwrap_or(0));
                let again = self.seen.get(&key).copied();
                self.seen.entry(key).or_insert(seconds);
                let shown = hex(&head);
                let _ = events.send(Event::Log {
                    channel: id,
                    kind: self.kind,
                    seconds,
                    aes: None,
                    ges: None,
                    title: format!("corr {:04X}", f.id),
                    text: format!(
                        "field {:02X} · payload {shown}… (encrypted){}{} · BER {:.1e}",
                        f.field,
                        again.map_or(String::new(), |t| format!(
                            " · same start as at {}",
                            clock(t)
                        )),
                        if f.flywheel { " · word not seen" } else { "" },
                        f.ber(),
                    ),
                });
                header = Some((f.id, f.field, shown));
            }
            locked = dec.locked();
            totals = Some((dec.frames, dec.uw_wrong, dec.uw_bits, dec.lock_losses));
        } else if let Some(check) = self.stream.as_mut() {
            let mut pieces: Vec<StreamPiece> = Vec::new();
            check.push(soft, &mut pieces);
            for p in &pieces {
                let good = p.channel_errors * 12 < p.channel_bits;
                if good {
                    errors += p.channel_errors;
                    bits += p.channel_bits;
                    self.pieces_locked += 1;
                    new_frames += 1;
                }
                self.stream_locked = good;
            }
            locked = self.stream_locked;
        } else {
            locked = false;
        }
        if bits > 0 {
            self.recent_errors = 0.9 * self.recent_errors + errors as f32;
            self.recent_bits = 0.9 * self.recent_bits + bits as f32;
            self.corrected += u64::from(errors);
        }

        if new_frames > 0 {
            self.confirmed = true;
        } else if !self.confirmed && seconds > self.deadline {
            self.gave_up = true;
        }
        let ds = self.demod.status();
        let mut s = status.lock().unwrap();
        s.kind = if self.confirmed {
            self.kind
        } else {
            Kind::Searching
        };
        s.locked = locked;
        s.coherence = ds.coherence;
        s.carrier_hz = s.acquired_hz + ds.offset_hz;
        s.clock_ppm = ds.clock_ppm;
        if !ds.scatter.is_empty() {
            s.scatter.clone_from(&ds.scatter);
        }
        if let Some((frames, uw_wrong, uw_bits, losses)) = totals {
            s.frames = frames;
            s.uw_wrong = uw_wrong;
            s.uw_bits = uw_bits;
            s.lock_losses = losses;
        } else {
            s.frames = self.pieces_locked;
        }
        s.corrected_bits = self.corrected;
        if let Some((cid, field, payload)) = header {
            s.corr_id = Some(cid);
            s.corr_field = Some(field);
            s.last_unit = format!("payload {payload}…");
        }
        if new_frames > 0 {
            s.last_good_s = seconds;
        }
        // The figures as the Aero channels give them (`channel::process`),
        // for a code on plain, not differential, bits.
        let limit_ber = self.kind.ber_limit();
        if locked && self.recent_bits > 500.0 {
            let ber = (self.recent_errors / self.recent_bits).clamp(1e-5, 0.4);
            s.pre_fec_ber = ber;
            let moments = 10f32.powf(ds.es_n0_db / 10.0);
            let from_errors = es_n0_for_ber(ber.max(1e-6));
            let es = if ber < 3e-3 {
                from_errors.min(moments).max(es_n0_for_ber(1.5e-3))
            } else {
                from_errors
            };
            let limit = es_n0_for_ber(limit_ber);
            s.es_n0_db = 10.0 * es.log10();
            s.eb_n0_db = 10.0 * (es / 0.5).log10();
            s.margin_db = 10.0 * (es / limit).log10();
            s.fec_load = (ber / limit_ber).clamp(0.0, 1.0);
        } else if !locked {
            s.es_n0_db = ds.es_n0_db;
            s.eb_n0_db = ds.es_n0_db + 3.0;
            s.margin_db = 0.0;
            s.fec_load = 1.0;
        }
    }
}
