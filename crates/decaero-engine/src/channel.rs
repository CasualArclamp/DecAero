// made by CasualArclamp
//! One carrier's worker thread: name the carrier, demodulate it, decode its
//! frames, turn voice into audio, and keep the figures the GUI shows.
//!
//! The thread first only listens: it keeps the wide blocks it is given while
//! a coarse down-converter collects a few seconds for the squared-line
//! acquisition. Once that names a bit rate and a carrier, the kept blocks are
//! played through the real chain, so nothing from the start is lost.

use decaero_ambe::{Decoder as VoiceDecoder, FRAME_SAMPLES, FrameKind};
use decaero_core::cframe::CDecoder;
use decaero_core::pframe::{PDecoder, PFormat, SU_BYTES};
use decaero_core::su::{self, Acars, CAssignment, Meaning, Reassembler};
use decaero_dsp::acquire;
use decaero_dsp::ddc::Ddc;
use decaero_dsp::demod::{Demod, Modulation};
use num_complex::Complex32;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub type ChannelId = u64;

/// What a channel turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Still collecting samples to name it.
    Searching,
    /// P channel at this bit rate.
    P(u32),
    /// C channel (voice) at 8400 bit/s.
    C,
    /// No Aero line pair found: the worker has stopped.
    NotAero,
    /// The carrier went away: the worker has stopped.
    Ended,
}

impl Kind {
    pub fn label(&self) -> String {
        match self {
            Kind::Searching => "search".into(),
            Kind::P(r) => format!("P {r}"),
            Kind::C => "C 8400".into(),
            Kind::NotAero => "not Aero".into(),
            Kind::Ended => "ended".into(),
        }
    }
    pub fn stopped(&self) -> bool {
        matches!(self, Kind::NotAero | Kind::Ended)
    }
    /// Raw bit error rate at which the channel code gives up, roughly.
    pub fn ber_limit(&self) -> f32 {
        match self {
            Kind::C => 0.06,
            _ => 0.11,
        }
    }
}

/// Everything the GUI shows about one channel.
#[derive(Debug, Clone)]
pub struct ChannelStatus {
    pub id: ChannelId,
    pub kind: Kind,
    /// Carrier offset from the centre of the input, in Hz (tracked).
    pub carrier_hz: f64,
    /// Carrier offset when the channel was acquired (for the drift figure).
    pub acquired_hz: f64,
    pub locked: bool,
    pub coherence: f32,
    /// Es/N0 from the corrected bit errors once frames decode (firm), from
    /// the sample moments before that (rough).
    pub es_n0_db: f32,
    pub eb_n0_db: f32,
    /// Es/N0 above the point where the channel code gives up.
    pub margin_db: f32,
    /// Share of the code's correcting power in use: 0 clean, 1 at the limit.
    pub fec_load: f32,
    pub pre_fec_ber: f32,
    pub clock_ppm: f64,
    pub scatter: Vec<Complex32>,
    pub frames: u64,
    pub uw_wrong: u64,
    pub uw_bits: u64,
    pub crc_good: u64,
    pub crc_bad: u64,
    pub corrected_bits: u64,
    pub lock_losses: u64,
    /// Voice frames: voiced, repeated, silent, tone.
    pub voice: [u64; 4],
    pub aes: Option<u32>,
    pub ges: Option<u8>,
    /// Seconds of input when the channel started and when it last decoded.
    pub started_s: f64,
    pub last_good_s: f64,
    /// Share of one core this worker uses (1.0 = a whole core in real time).
    pub cpu: f32,
    /// Eb/N0 once a second, newest last.
    pub history: VecDeque<f32>,
    pub last_unit: String,
    /// Peak of the decoded audio, 0 to 1.
    pub audio_level: f32,
    /// The worker has stopped (the carrier went, or it was removed).
    pub closed: bool,
}

impl ChannelStatus {
    pub fn new(id: ChannelId, carrier_hz: f64, started_s: f64) -> Self {
        Self {
            id,
            kind: Kind::Searching,
            carrier_hz,
            acquired_hz: carrier_hz,
            locked: false,
            coherence: 0.0,
            es_n0_db: 0.0,
            eb_n0_db: 0.0,
            margin_db: 0.0,
            fec_load: 1.0,
            pre_fec_ber: 0.0,
            clock_ppm: 0.0,
            scatter: Vec::new(),
            frames: 0,
            uw_wrong: 0,
            uw_bits: 0,
            crc_good: 0,
            crc_bad: 0,
            corrected_bits: 0,
            lock_losses: 0,
            voice: [0; 4],
            aes: None,
            ges: None,
            started_s,
            last_good_s: started_s,
            cpu: 0.0,
            history: VecDeque::new(),
            last_unit: String::new(),
            audio_level: 0.0,
            closed: false,
        }
    }
}

/// What a worker tells the engine.
#[derive(Debug, Clone)]
pub enum Event {
    /// A good signal unit worth a log line.
    Unit {
        channel: ChannelId,
        kind: Kind,
        seconds: f64,
        bytes: [u8; SU_BYTES],
        name: &'static str,
    },
    Acars {
        channel: ChannelId,
        seconds: f64,
        aes: u32,
        ges: u8,
        acars: Acars,
    },
    /// User data that is not ACARS.
    Data {
        channel: ChannelId,
        seconds: f64,
        aes: u32,
        ges: u8,
        bytes: Vec<u8>,
    },
    /// A voice channel was assigned (the engine may start a decoder there).
    Assignment {
        channel: ChannelId,
        seconds: f64,
        assignment: CAssignment,
    },
    /// Decoded audio, 8 kHz, for the recorder and the loudspeaker.
    Voice { channel: ChannelId, pcm: Vec<i16> },
    /// The deep scan found an Aero line pair here (offset from the centre).
    Found {
        offset_hz: f64,
        bit_rate: u32,
        strength: f32,
    },
    /// The worker has finished.
    Closed { channel: ChannelId },
}

/// What the engine asks of a new worker.
pub struct Request {
    pub id: ChannelId,
    pub in_rate: f64,
    /// Where to look, as an offset from the centre of the input.
    pub offset_hz: f64,
    /// Bit rates to look for, likeliest first.
    pub rates: Vec<u32>,
    /// How far from `offset_hz` the carrier may be.
    pub search_hz: f64,
    pub started_s: f64,
    /// Set when the deep scan has already named the bit rate and put
    /// `offset_hz` on the carrier: the worker starts decoding at once.
    pub known_rate: Option<u32>,
}

/// Es/N0 (as a ratio) that gives raw bit error rate `p` on an antipodal signal.
fn es_n0_for_ber(p: f32) -> f32 {
    // Invert p = Q(sqrt(2 g)) by bisection on g in dB.
    let q = |g: f32| 0.5 * erfc(g.sqrt());
    let (mut lo, mut hi) = (-10.0f32, 20.0f32);
    for _ in 0..30 {
        let mid = 0.5 * (lo + hi);
        if q(10f32.powf(mid / 10.0)) > p {
            lo = mid
        } else {
            hi = mid
        }
    }
    10f32.powf(0.5 * (lo + hi) / 10.0)
}

/// Complementary error function (Abramowitz and Stegun 7.1.26).
fn erfc(x: f32) -> f32 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x);
    let poly = t
        * (0.254_829_6
            + t * (-0.284_496_74 + t * (1.421_413_7 + t * (-1.453_152 + t * 1.061_405_4))));
    poly * (-x * x).exp()
}

// One of these lives per worker thread, so the size gap between the two
// costs nothing worth a heap allocation.
#[allow(clippy::large_enum_variant)]
enum Chain {
    P {
        decoder: PDecoder,
        reassembler: Reassembler,
    },
    C {
        decoder: CDecoder,
        voice: VoiceDecoder,
    },
}

struct Running {
    ddc: Ddc,
    demod: Demod,
    chain: Chain,
    /// Raw bit errors and bits of recent frames (a running average).
    recent_errors: f32,
    recent_bits: f32,
    differential: bool,
    code_rate: f32,
}

/// The worker thread's body. `stop` is raised by the engine to end it.
pub fn run(
    req: Request,
    blocks: Receiver<Arc<Vec<Complex32>>>,
    status: Arc<Mutex<ChannelStatus>>,
    events: Sender<Event>,
    stop: Arc<AtomicBool>,
    samples_seen: Arc<AtomicU64>,
) {
    let id = req.id;
    {
        let mut s = status.lock().unwrap();
        *s = ChannelStatus::new(id, req.offset_hz, req.started_s);
    }
    // Acquisition runs at about 2.5 samples per bit of the fastest rate.
    let top = f64::from(req.rates.iter().copied().max().unwrap_or(600));
    let mut coarse = Ddc::new(
        req.in_rate,
        req.offset_hz,
        Ddc::decimation_for(req.in_rate, top, 2.5),
    );
    let mut coarse_out: Vec<Complex32> = Vec::new();
    let mut kept: Vec<Arc<Vec<Complex32>>> = Vec::new();
    let mut tries = 0;
    let mut running: Option<Running> = None;
    let mut seconds = req.started_s;
    let mut busy = 0.0f64;
    let mut last_second = seconds.floor();
    let mut quiet_since = seconds;
    let mut narrow = Vec::new();
    let mut soft = Vec::new();

    if let Some(bit_rate) = req.known_rate {
        running = Some(start_chain(&req, req.offset_hz, bit_rate, &status));
    }

    while let Ok(block) = blocks.recv() {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let began = Instant::now();
        let block_seconds = block.len() as f64 / req.in_rate;
        seconds += block_seconds;
        samples_seen.fetch_add(block.len() as u64, Ordering::Relaxed);

        if running.is_none() {
            coarse.process(&block, &mut coarse_out);
            kept.push(block.clone());
            let have = coarse_out.len() as f64 / coarse.out_rate();
            // Try at 3 s; a weak carrier gets a second look at 10 s.
            let due = if tries == 0 { 3.0 } else { 10.0 };
            if have < due {
                continue;
            }
            tries += 1;
            let found = acquire::find(&coarse_out, coarse.out_rate(), &req.rates, req.search_hz);
            let best = found.first().copied().filter(|f| {
                // Clear of chance, and clear of the runner-up.
                f.strength >= 4.5
                    && found
                        .get(1)
                        .is_none_or(|second| f.strength > 1.6 * second.strength)
            });
            let Some(best) = best else {
                if tries >= 2 {
                    status.lock().unwrap().kind = Kind::NotAero;
                    break;
                }
                continue;
            };
            let mut r = start_chain(&req, req.offset_hz + best.offset_hz, best.bit_rate, &status);
            // Play back what was kept while searching.
            let mut t = req.started_s;
            for b in kept.drain(..) {
                t += b.len() as f64 / req.in_rate;
                process(&mut r, &b, t, id, &status, &events, &mut narrow, &mut soft);
            }
            coarse_out = Vec::new();
            running = Some(r);
            quiet_since = seconds;
        } else if let Some(r) = running.as_mut() {
            process(
                r,
                &block,
                seconds,
                id,
                &status,
                &events,
                &mut narrow,
                &mut soft,
            );
        }

        busy += began.elapsed().as_secs_f64();
        let mut s = status.lock().unwrap();
        if s.locked || s.coherence > 0.12 {
            quiet_since = seconds;
        }
        if seconds.floor() > last_second {
            last_second = seconds.floor();
            s.cpu = (0.7 * f64::from(s.cpu) + 0.3 * busy) as f32;
            busy = 0.0;
            let eb = s.eb_n0_db;
            s.history.push_back(eb);
            if s.history.len() > 120 {
                s.history.pop_front();
            }
        }
        // A call that has gone is closed after a few seconds; a P channel is
        // given longer, it will be back.
        let patience = if s.kind == Kind::C {
            5.0
        } else if s.frames == 0 {
            // Nothing decoded yet: it was probably never a carrier.
            12.0
        } else {
            30.0
        };
        if running.is_some() && seconds - quiet_since > patience {
            break;
        }
    }
    {
        let mut s = status.lock().unwrap();
        if s.kind == Kind::Searching {
            s.kind = Kind::Ended;
        }
        s.locked = false;
        s.closed = true;
    }
    let _ = events.send(Event::Closed { channel: id });
}

/// Build the demodulator and decoder for a carrier of a known rate.
fn start_chain(
    req: &Request,
    carrier: f64,
    bit_rate: u32,
    status: &Arc<Mutex<ChannelStatus>>,
) -> Running {
    let ddc = Ddc::new(
        req.in_rate,
        carrier,
        Ddc::decimation_for(req.in_rate, f64::from(bit_rate), 8.0),
    );
    let demod = Demod::new(
        Modulation::for_rate(bit_rate),
        bit_rate,
        ddc.out_rate(),
        0.0,
    );
    let (chain, kind, code_rate) = match PFormat::from_rate(bit_rate) {
        Some(f) => (
            Chain::P {
                decoder: PDecoder::new(f),
                reassembler: Reassembler::new(),
            },
            Kind::P(bit_rate),
            0.5,
        ),
        None => (
            Chain::C {
                decoder: CDecoder::new(),
                voice: VoiceDecoder::new(),
            },
            Kind::C,
            2.0 / 3.0,
        ),
    };
    {
        let mut s = status.lock().unwrap();
        s.kind = kind;
        s.carrier_hz = carrier;
        s.acquired_hz = carrier;
    }
    Running {
        ddc,
        demod,
        chain,
        recent_errors: 0.0,
        recent_bits: 0.0,
        differential: bit_rate <= 1200,
        code_rate,
    }
}

#[allow(clippy::too_many_arguments)]
fn process(
    r: &mut Running,
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
    r.ddc.process(block, narrow);
    for piece in narrow.chunks(512) {
        r.demod.process(piece, soft);
    }
    // Decode, collecting what changed.
    let mut frame_errors = 0u32;
    let mut frame_bits = 0u32;
    let mut frames = 0u64;
    let mut aircraft: Option<(u32, u8)> = None;
    let mut last_unit: Option<String> = None;
    let mut voice_kinds = [0u64; 4];
    let mut audio_peak = 0f32;
    let (locked, totals, kind) = match &mut r.chain {
        Chain::P {
            decoder,
            reassembler,
        } => {
            let mut out = Vec::new();
            decoder.push(soft, &mut out);
            let kind = Kind::P(decoder.format().rate);
            for f in &out {
                frames += 1;
                frame_errors += f.channel_errors;
                frame_bits += f.channel_bits;
                for u in f.units.iter().filter(|u| u.crc_ok) {
                    let name = su::p_name(u.bytes[0]);
                    match su::p_meaning(&u.bytes) {
                        Meaning::Assignment(assignment) => {
                            let _ = events.send(Event::Assignment {
                                channel: id,
                                seconds,
                                assignment,
                            });
                        }
                        Meaning::Aircraft { .. }
                        | Meaning::Channel { .. }
                        | Meaning::Satellite { .. }
                        | Meaning::Plain => {}
                    }
                    if let Some(data) = reassembler.push(&u.bytes) {
                        let ev = match su::acars(&data.bytes) {
                            Some(acars) => Event::Acars {
                                channel: id,
                                seconds,
                                aes: data.aes,
                                ges: data.ges,
                                acars,
                            },
                            None => Event::Data {
                                channel: id,
                                seconds,
                                aes: data.aes,
                                ges: data.ges,
                                bytes: data.bytes,
                            },
                        };
                        let _ = events.send(ev);
                    }
                    if !su::is_filler(u.bytes[0]) && u.bytes[0] & 0xC0 != 0xC0 {
                        last_unit = Some(name.to_string());
                        let _ = events.send(Event::Unit {
                            channel: id,
                            kind,
                            seconds,
                            bytes: u.bytes,
                            name,
                        });
                    }
                }
            }
            let st = decoder.stats();
            (
                decoder.locked(),
                (
                    st.frames,
                    st.uw_bits_wrong,
                    st.uw_bits,
                    st.crc_good,
                    st.crc_bad,
                    st.channel_errors,
                    st.lock_losses,
                ),
                kind,
            )
        }
        Chain::C { decoder, voice } => {
            let mut out = Vec::new();
            decoder.push(soft, &mut out);
            for f in &out {
                frames += 1;
                frame_errors += f.channel_errors;
                frame_bits += f.channel_bits;
                for u in f.units.iter().filter(|u| u.crc_ok) {
                    if u.bytes[0] == 0x01 {
                        continue;
                    }
                    let name = match u.bytes[0] {
                        0x30 => "call progress",
                        0x60 => "telephony acknowledge",
                        _ => "sub-band signalling",
                    };
                    if matches!(u.bytes[0], 0x30 | 0x60) {
                        let aes = (u32::from(u.bytes[1]) << 16)
                            | (u32::from(u.bytes[2]) << 8)
                            | u32::from(u.bytes[3]);
                        aircraft = Some((aes, u.bytes[4]));
                    }
                    last_unit = Some(name.to_string());
                    let _ = events.send(Event::Unit {
                        channel: id,
                        kind: Kind::C,
                        seconds,
                        bytes: u.bytes,
                        name,
                    });
                }
                let mut pcm = Vec::with_capacity(f.voice.len() * FRAME_SAMPLES);
                for v in &f.voice {
                    let mut samples = [0i16; FRAME_SAMPLES];
                    let info = voice.decode(v, &mut samples);
                    voice_kinds[match info.kind {
                        FrameKind::Voice => 0,
                        FrameKind::Repeat => 1,
                        FrameKind::Silence => 2,
                        FrameKind::Tone => 3,
                    }] += 1;
                    audio_peak = samples
                        .iter()
                        .fold(audio_peak, |m, &s| m.max(f32::from(s).abs() / 32768.0));
                    pcm.extend_from_slice(&samples);
                }
                let _ = events.send(Event::Voice { channel: id, pcm });
            }
            let st = decoder.stats();
            (
                decoder.locked(),
                (
                    st.frames,
                    st.uw_bits_wrong,
                    st.uw_bits,
                    st.crc_good,
                    st.crc_bad,
                    st.channel_errors,
                    st.lock_losses,
                ),
                Kind::C,
            )
        }
    };
    if frames > 0 {
        // About ten frames of memory.
        r.recent_errors = 0.9 * r.recent_errors + frame_errors as f32;
        r.recent_bits = 0.9 * r.recent_bits + frame_bits as f32;
    }

    let ds = r.demod.status();
    let mut s = status.lock().unwrap();
    s.kind = kind;
    s.locked = locked;
    s.coherence = ds.coherence;
    s.carrier_hz = s.acquired_hz + ds.offset_hz;
    s.clock_ppm = ds.clock_ppm;
    if !ds.scatter.is_empty() {
        s.scatter.clone_from(&ds.scatter);
    }
    (
        s.frames,
        s.uw_wrong,
        s.uw_bits,
        s.crc_good,
        s.crc_bad,
        s.corrected_bits,
        s.lock_losses,
    ) = totals;
    for (a, b) in s.voice.iter_mut().zip(voice_kinds) {
        *a += b;
    }
    if let Some((aes, ges)) = aircraft {
        s.aes = Some(aes);
        s.ges = Some(ges);
    }
    if let Some(u) = last_unit {
        s.last_unit = u;
    }
    if frames > 0 {
        s.last_good_s = seconds;
        s.audio_level = audio_peak;
    }
    if locked && r.recent_bits > 500.0 {
        let ber = (r.recent_errors / r.recent_bits).clamp(1e-5, 0.4);
        s.pre_fec_ber = ber;
        // A-BPSK bits are products of two samples: undo the doubling.
        let p = if r.differential {
            0.5 * (1.0 - (1.0 - 2.0 * ber).max(0.0).sqrt())
        } else {
            ber
        };
        // With few errors to count, the error rate says little: above about
        // 8 dB the sample moments are the better measure.
        let moments = 10f32.powf(ds.es_n0_db / 10.0);
        let from_errors = es_n0_for_ber(p.max(1e-6));
        let es = if ber < 3e-3 {
            from_errors.min(moments).max(es_n0_for_ber(1.5e-3))
        } else {
            from_errors
        };
        let limit = es_n0_for_ber(if r.differential {
            0.5 * (1.0 - (1.0 - 2.0 * kind.ber_limit()).sqrt())
        } else {
            kind.ber_limit()
        });
        s.es_n0_db = 10.0 * es.log10();
        s.eb_n0_db = 10.0 * (es / r.code_rate).log10();
        s.margin_db = 10.0 * (es / limit).log10();
        s.fec_load = (ber / kind.ber_limit()).clamp(0.0, 1.0);
    } else if !locked {
        s.es_n0_db = ds.es_n0_db;
        s.eb_n0_db = ds.es_n0_db - 10.0 * r.code_rate.log10();
        s.margin_db = 0.0;
        s.fec_load = 1.0;
    }
}
