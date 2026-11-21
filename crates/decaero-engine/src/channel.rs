// made by CasualArclamp
//! One carrier's worker thread: name the carrier, demodulate it, decode its
//! frames, turn voice into audio, and keep the figures the GUI shows.
//!
//! The thread first only listens: it keeps the wide blocks it is given while
//! a coarse down-converter collects a few seconds for the squared-line
//! acquisition. Once that names a bit rate and a carrier, the kept blocks are
//! played through the real chain, so nothing from the start is lost.

use crate::feed::Link;
use decaero_ambe::{Decoder as VoiceDecoder, FRAME_SAMPLES, FrameKind};
use decaero_core::c21frame::{self, C21Decoder};
use decaero_core::cframe::CDecoder;
use decaero_core::mplpc;
use decaero_core::pframe::{PDecoder, PFormat, SU_BYTES};
use decaero_core::rtframe::{self, Burst, RReassembler};
use decaero_core::su::{self, Acars, CAssignment, Meaning, Reassembler};
use decaero_dsp::acquire;
use decaero_dsp::burst::BurstReceiver;
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
    /// R and T channel bursts at this bit rate (aircraft to ground).
    Rt(u32),
    /// C channel (voice) at 8400 bit/s.
    C,
    /// An L-band correction carrier (plain BPSK, not Aero) at this symbol
    /// rate, whole: 2438, 1219 or 1200 (`decaero_core::corr`).
    Corr(u32),
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
            Kind::Rt(r) => format!("R/T {r}"),
            Kind::C => "C 8400".into(),
            Kind::Corr(r) => format!("corr {r}"),
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
    /// A C channel of the older kind: 21000 bit/s, 9.6 kbit/s speech.
    pub wide: bool,
    /// Its frames are not speech though the channel is clean: a fax or
    /// data call (21000 bit/s only).
    pub data: bool,
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
    /// R and T bursts decoded on this channel.
    pub bursts: u64,
    /// The worker has stopped (the carrier went, or it was removed).
    pub closed: bool,
    /// A correction carrier's clear header: its 16-bit ID and the 7 bits
    /// after it, from the last frame.
    pub corr_id: Option<u16>,
    pub corr_field: Option<u8>,
}

impl ChannelStatus {
    /// The channel's kind in words, with its bit rate.
    pub fn label(&self) -> String {
        if self.kind == Kind::C && self.wide {
            "C 21000".into()
        } else {
            self.kind.label()
        }
    }

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
            wide: false,
            data: false,
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
            bursts: 0,
            closed: false,
            corr_id: None,
            corr_field: None,
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
        link: Link,
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
    /// Half a second of a 21000 bit/s call: its 25 voice frames as they
    /// came, for whoever wants the bits. `data`: the call is not speech.
    /// `held`: frames from before that was known, sent again.
    RawVoice {
        channel: ChannelId,
        frames: Vec<[u8; c21frame::VOICE_BYTES]>,
        data: bool,
        held: bool,
    },
    /// The deep scan found an Aero line pair here (offset from the centre).
    Found {
        offset_hz: f64,
        bit_rate: u32,
        strength: f32,
    },
    /// A line for the log that is none of the above (bursts).
    Log {
        channel: ChannelId,
        kind: Kind,
        seconds: f64,
        aes: Option<u32>,
        ges: Option<u8>,
        title: String,
        text: String,
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
    /// How wide the carrier looked (or the box the user drew), in Hz.
    pub width_hz: f64,
    pub started_s: f64,
    /// Set when the deep scan has already named the bit rate and put
    /// `offset_hz` on the carrier: the worker starts decoding at once.
    pub known_rate: Option<u32>,
    /// The user put this decoder here: it stays however quiet the channel is
    /// (a burst channel can be silent for minutes).
    pub persistent: bool,
    /// A carrier is expected here any moment (an assigned voice channel):
    /// look every second at the newest two seconds, for half a minute.
    pub eager: bool,
}

/// Es/N0 (as a ratio) that gives raw bit error rate `p` on an antipodal signal.
pub(crate) fn es_n0_for_ber(p: f32) -> f32 {
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
    /// The 21000 bit/s C channel.
    C21 {
        decoder: C21Decoder,
        voice: Box<mplpc::Decoder>,
    },
}

/// Catches R and T bursts on a channel that carries no continuous P signal.
struct BurstPath {
    receiver: BurstReceiver,
    bit_rate: u32,
    r_data: RReassembler,
    t_data: Reassembler,
}

/// Watches a decoder that has lost its place and finds the carrier again.
///
/// The demodulator's frequency loop only reads a few hertz either way at
/// the slow rates. A break in the samples or a burst of interference can
/// throw it further, and then it never comes back: the constellation is a
/// ring. So when a channel stays out of lock, the carrier is looked for
/// afresh in the last seconds of samples, the way it was first found.
struct Resync {
    /// Recent samples, thinned to about 2.7 a bit (little noise besides the
    /// signal goes into the squaring that way).
    recent: Vec<Complex32>,
    rate: f64,
    keep: usize,
    /// Samples being averaged into the next kept one.
    sum: Complex32,
    count: usize,
    /// When lock was last held, and when a search was last made.
    out_since: Option<f64>,
    tried: f64,
}

/// One thinned sample for this many of the demodulator's.
const THIN: usize = 3;

impl Resync {
    fn new(rate: f64, bit_rate: u32) -> Self {
        let rate = rate / THIN as f64;
        // Enough for bins of well under a hertz at the slow rates.
        let seconds = if bit_rate <= 1200 { 4.0 } else { 1.5 };
        Self {
            recent: Vec::new(),
            rate,
            keep: (seconds * rate) as usize,
            sum: Complex32::default(),
            count: 0,
            out_since: None,
            tried: 0.0,
        }
    }

    fn add(&mut self, narrow: &[Complex32]) {
        for &x in narrow {
            self.sum += x;
            self.count += 1;
            if self.count == THIN {
                self.recent.push(self.sum / THIN as f32);
                self.sum = Complex32::default();
                self.count = 0;
            }
        }
        if self.recent.len() > 2 * self.keep {
            let cut = self.recent.len() - self.keep;
            self.recent.drain(..cut);
        }
    }

    /// Give the lock state after every block; returns the carrier offset to
    /// move to when the channel has been out long enough and the carrier is
    /// plainly there.
    fn check(&mut self, locked: bool, seconds: f64, bit_rate: u32, search_hz: f64) -> Option<f64> {
        if locked {
            self.out_since = None;
            return None;
        }
        let since = *self.out_since.get_or_insert(seconds);
        // A frame or two of grace, then a look every few seconds.
        let (grace, every) = if bit_rate <= 1200 {
            (2.5, 4.0)
        } else {
            (1.0, 1.5)
        };
        if seconds - since < grace || seconds - self.tried < every || self.recent.len() < self.keep
        {
            return None;
        }
        self.tried = seconds;
        let from = self.recent.len() - self.keep;
        let found = acquire::find(&self.recent[from..], self.rate, &[bit_rate], search_hz);
        found
            .first()
            .filter(|f| f.strength >= 4.5)
            .map(|f| f.offset_hz)
    }
}

/// Half seconds of clean channel with no speech in them before a call is
/// taken as data, and how many frames from before that are kept (a minute).
const DATA_HALVES: u32 = 4;
const HELD_FRAMES: usize = 3000;

struct Running {
    /// 21000 bit/s: half seconds running that were clean but not speech,
    /// whether the call is taken as data, and its frames until that is known.
    data_halves: u32,
    data: bool,
    held: Vec<[u8; c21frame::VOICE_BYTES]>,
    /// The tone frame last put in the log.
    last_tone: Option<[u8; 9]>,
    resync: Resync,
    /// How far either side of the decoder's centre the carrier may be.
    search_hz: f64,
    /// The burst path, for the bit rates aircraft send bursts at.
    bursts: Option<BurstPath>,
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
    // A correction carrier instead (`corr.rs`).
    let mut corr_run: Option<crate::corr::CorrRunning> = None;
    let mut seconds = req.started_s;
    let mut busy = 0.0f64;
    let mut last_second = seconds.floor();
    let mut quiet_since = seconds;
    let mut bursts_seen = 0u64;
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

        if running.is_none() && corr_run.is_none() {
            coarse.process(&block, &mut coarse_out);
            kept.push(block.clone());
            let have = coarse_out.len() as f64 / coarse.out_rate();
            // Try at 3 s; a weak carrier gets a second look at 10 s.
            let due = if req.eager {
                2.0
            } else if tries == 0 {
                3.0
            } else {
                10.0
            };
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
                // Not Aero; perhaps a correction carrier (a voice channel
                // that is expected is not looked at for one).
                if !req.eager
                    && let Some(found) = crate::corr::find(
                        &kept,
                        req.in_rate,
                        req.offset_hz,
                        req.search_hz,
                        req.width_hz,
                    )
                {
                    let mut c = crate::corr::start(
                        req.in_rate,
                        req.offset_hz + found.offset_hz,
                        found,
                        req.started_s,
                        &status,
                    );
                    let mut t = req.started_s;
                    for b in kept.drain(..) {
                        t += b.len() as f64 / req.in_rate;
                        c.process(&b, t, id, &status, &events, &mut narrow, &mut soft);
                    }
                    coarse_out = Vec::new();
                    corr_run = Some(c);
                    quiet_since = seconds;
                    continue;
                }
                if tries >= if req.eager { 30 } else { 2 } {
                    status.lock().unwrap().kind = Kind::NotAero;
                    break;
                }
                if req.eager {
                    // Keep the newest second: the next look is at that and
                    // the second to come, so a carrier that has just come
                    // up is not drowned by the noise from before it.
                    let keep = coarse.out_rate() as usize;
                    let cut = coarse_out.len().saturating_sub(keep);
                    coarse_out.drain(..cut);
                    let mut held = 0.0;
                    let from = kept
                        .iter()
                        .rposition(|b| {
                            held += b.len() as f64 / req.in_rate;
                            held >= 1.0
                        })
                        .unwrap_or(0);
                    kept.drain(..from);
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
        } else if let Some(c) = corr_run.as_mut() {
            c.process(
                &block,
                seconds,
                id,
                &status,
                &events,
                &mut narrow,
                &mut soft,
            );
            if c.gave_up() {
                status.lock().unwrap().kind = Kind::NotAero;
                break;
            }
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
        if s.bursts != bursts_seen {
            bursts_seen = s.bursts;
            quiet_since = seconds;
        }
        let patience = if req.persistent {
            f64::MAX
        } else if s.kind == Kind::C {
            5.0
        } else if matches!(s.kind, Kind::Rt(_)) {
            // Aircraft do not send all the time.
            180.0
        } else if s.frames == 0 {
            // Nothing decoded yet: it was probably never a carrier.
            12.0
        } else {
            30.0
        };
        if (running.is_some() || corr_run.is_some()) && seconds - quiet_since > patience {
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
        _ if bit_rate == 21_000 => (
            Chain::C21 {
                decoder: C21Decoder::new(),
                voice: Box::new(mplpc::Decoder::with_postfilter()),
            },
            Kind::C,
            0.5,
        ),
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
        s.wide = bit_rate == 21_000;
        s.carrier_hz = carrier;
        s.acquired_hz = carrier;
    }
    let bursts = PFormat::from_rate(bit_rate)
        .filter(|_| bit_rate != 21_000)
        .map(|_| BurstPath {
            receiver: BurstReceiver::new(ddc.out_rate(), bit_rate),
            bit_rate,
            r_data: RReassembler::new(),
            t_data: Reassembler::new(),
        });
    Running {
        data_halves: 0,
        data: false,
        held: Vec::new(),
        last_tone: None,
        resync: Resync::new(ddc.out_rate(), bit_rate),
        search_hz: req.search_hz.min(f64::from(bit_rate) * 0.4),
        bursts,
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
    r.resync.add(narrow);
    // Decode, collecting what changed.
    let mut frame_errors = 0u32;
    let mut frame_bits = 0u32;
    let mut frames = 0u64;
    let mut aircraft: Option<(u32, u8)> = None;
    let mut last_unit: Option<String> = None;
    let mut voice_kinds = [0u64; 4];
    // Tone frames logged this block (see below).
    let mut tones: Vec<[u8; 9]> = Vec::new();
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
                        | Meaning::Satellite(_)
                        | Meaning::Plain => {}
                    }
                    if let Some(data) = reassembler.push(&u.bytes) {
                        let ev = match su::acars(&data.bytes) {
                            Some(acars) => Event::Acars {
                                channel: id,
                                seconds,
                                link: Link {
                                    aes: data.aes,
                                    ges: data.ges,
                                    q_no: data.q_no,
                                    ref_no: data.ref_no,
                                    from_aircraft: false,
                                },
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
        Chain::C21 { decoder, voice } => {
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
                let mut pcm = Vec::with_capacity(f.voice.len() * mplpc::FRAME_SAMPLES);
                let mut speech = 0;
                for v in &f.voice {
                    let mut samples = [0i16; mplpc::FRAME_SAMPLES];
                    let kind = voice.decode(v, &mut samples);
                    speech += u32::from(kind == mplpc::FrameKind::Voice);
                    voice_kinds[match kind {
                        mplpc::FrameKind::Voice => 0,
                        mplpc::FrameKind::Repeat => 1,
                        mplpc::FrameKind::Mute => 2,
                    }] += 1;
                    audio_peak = samples
                        .iter()
                        .fold(audio_peak, |m, &s| m.max(f32::from(s).abs() / 32768.0));
                    pcm.extend_from_slice(&samples);
                }
                let _ = events.send(Event::Voice { channel: id, pcm });
                // A clean channel whose frames all fail the speech codec's
                // checks carries something else: fax or data. Their layout
                // is not known, so they are kept for working it out.
                let clean = f.channel_errors * 50 < f.channel_bits;
                if clean && speech <= 5 {
                    r.data_halves += 1;
                } else if clean {
                    r.data_halves = 0;
                }
                if !r.data && r.data_halves >= DATA_HALVES {
                    r.data = true;
                    let _ = events.send(Event::RawVoice {
                        channel: id,
                        frames: std::mem::take(&mut r.held),
                        data: true,
                        held: true,
                    });
                }
                if !r.data && r.held.len() < HELD_FRAMES {
                    r.held.extend_from_slice(&f.voice);
                }
                let _ = events.send(Event::RawVoice {
                    channel: id,
                    frames: f.voice.to_vec(),
                    data: r.data,
                    held: false,
                });
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
                    // A clean tone frame goes to the log as it stands: the
                    // decoder cannot play it yet (the layout of this codec's
                    // tone frames is not known), and these are what it will
                    // be worked out from. One line for each new pattern.
                    if info.kind == FrameKind::Tone
                        && info.total_errors == 0
                        && r.last_tone != Some(info.data)
                    {
                        r.last_tone = Some(info.data);
                        tones.push(info.data);
                    }
                    audio_peak = samples
                        .iter()
                        .fold(audio_peak, |m, &s| m.max(f32::from(s).abs() / 32768.0));
                    pcm.extend_from_slice(&samples);
                }
                let _ = events.send(Event::Voice { channel: id, pcm });
                for data in tones.drain(..) {
                    let hex: String = data.iter().map(|b| format!("{b:02X}")).collect();
                    let _ = events.send(Event::Log {
                        channel: id,
                        kind: Kind::C,
                        seconds,
                        aes: aircraft.map(|a| a.0),
                        ges: aircraft.map(|a| a.1),
                        title: "tone frame".into(),
                        text: format!(
                            "{hex} (not played: this codec's tone layout is not known yet)"
                        ),
                    });
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
                Kind::C,
            )
        }
    };
    // Bursts, where there is no continuous signal holding the channel.
    let mut burst_figures: Option<(f32, f32, Vec<Complex32>)> = None;
    let mut bursts_decoded = 0u64;
    if let Some(path) = r.bursts.as_mut().filter(|_| !locked) {
        let mut found = Vec::new();
        path.receiver.process(narrow, &mut found);
        for b in found {
            let Some(d) = rtframe::decode_burst(&b.soft, path.bit_rate > 5000) else {
                continue;
            };
            bursts_decoded += 1;
            let rt = Kind::Rt(path.bit_rate);
            let quality = format!(
                "Es/N0 {:.1} dB, pre-FEC BER {:.1e}",
                b.es_n0_db,
                d.pre_fec_ber()
            );
            burst_figures = Some((b.es_n0_db, d.pre_fec_ber(), b.scatter));
            let log = |aes, ges, title: String, text: String| {
                let _ = events.send(Event::Log {
                    channel: id,
                    kind: rt,
                    seconds,
                    aes,
                    ges,
                    title,
                    text,
                });
            };
            let message = |data: su::UserData| match su::acars(&data.bytes) {
                Some(acars) => Event::Acars {
                    channel: id,
                    seconds,
                    link: Link {
                        aes: data.aes,
                        ges: data.ges,
                        q_no: data.q_no,
                        ref_no: data.ref_no,
                        from_aircraft: true,
                    },
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
            match &d.burst {
                Burst::R { unit } => {
                    let hex = unit
                        .iter()
                        .map(|x| format!("{x:02X}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    match rtframe::r_data(unit) {
                        Some(h) => {
                            aircraft = Some((h.aes, h.ges));
                            log(
                                Some(h.aes),
                                Some(h.ges),
                                format!("R burst: user data {} of {}", h.index + 1, h.total),
                                format!("{hex}\n{quality}"),
                            );
                            if let Some(data) = path.r_data.push(unit) {
                                let _ = events.send(message(data));
                            }
                        }
                        None => log(
                            None,
                            None,
                            format!("R burst: {}", rtframe::r_name(unit[2])),
                            format!("{hex}\n{quality}"),
                        ),
                    }
                    last_unit = Some("R burst".to_string());
                }
                Burst::T { aes, ges, units } => {
                    aircraft = Some((*aes, *ges));
                    let good = units.iter().filter(|u| u.crc_ok).count();
                    log(
                        Some(*aes),
                        Some(*ges),
                        format!("T burst: {good} of {} units good", units.len()),
                        quality.clone(),
                    );
                    for u in units.iter().filter(|u| u.crc_ok) {
                        if let Some(data) = path.t_data.push(&u.bytes) {
                            let _ = events.send(message(data));
                        }
                    }
                    last_unit = Some("T burst".to_string());
                }
            }
        }
    }

    if frames > 0 {
        // About ten frames of memory.
        r.recent_errors = 0.9 * r.recent_errors + frame_errors as f32;
        r.recent_bits = 0.9 * r.recent_bits + frame_bits as f32;
    }

    // Out of lock for a while: find the carrier again.
    let burst_channel = matches!(status.lock().unwrap().kind, Kind::Rt(_));
    let bit_rate = match kind {
        Kind::P(rate) | Kind::Rt(rate) => rate,
        _ => 8400,
    };
    if !burst_channel && let Some(offset) = r.resync.check(locked, seconds, bit_rate, r.search_hz) {
        let moved = offset - r.demod.status().offset_hz;
        // Close by, the loop is not what is wrong: leave it be.
        if moved.abs() > 2.0 {
            r.demod.retune(offset);
            let _ = events.send(Event::Log {
                channel: id,
                kind,
                seconds,
                aes: None,
                ges: None,
                title: "carrier found again".into(),
                text: format!(
                    "sync was lost; the carrier is {moved:+.1} Hz from where the loop had gone"
                ),
            });
        }
    }
    let ds = r.demod.status();
    let mut s = status.lock().unwrap();
    // A burst channel keeps its name between bursts.
    if !(matches!(s.kind, Kind::Rt(_)) && !locked) {
        s.kind = kind;
    }
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
    s.data = r.data;
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
    if bursts_decoded > 0 {
        s.bursts += bursts_decoded;
        s.last_good_s = seconds;
        if let Kind::P(rate) = kind
            && s.frames == 0
        {
            // Bursts and never a continuous frame: an R/T channel.
            s.kind = Kind::Rt(rate);
        }
    }
    if let Some((es_n0_db, ber, scatter)) = burst_figures {
        s.es_n0_db = es_n0_db;
        s.eb_n0_db = es_n0_db + 3.0;
        s.pre_fec_ber = ber;
        s.fec_load = (ber / kind.ber_limit()).clamp(0.0, 1.0);
        let limit = 10.0 * es_n0_for_ber(kind.ber_limit()).log10();
        s.margin_db = es_n0_db - limit;
        s.scatter = scatter;
        return;
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
    } else if !locked && !matches!(s.kind, Kind::Rt(_)) {
        s.es_n0_db = ds.es_n0_db;
        s.eb_n0_db = ds.es_n0_db - 10.0 * r.code_rate.log10();
        s.margin_db = 0.0;
        s.fec_load = 1.0;
    }
}
