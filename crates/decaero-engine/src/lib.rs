// made by CasualArclamp
//! The DecAero receiver engine.
//!
//! One front end thread reads the input in blocks, keeps the spectrum and the
//! waterfall rows, and hands every block (shared, not copied) to a worker
//! thread per carrier, as DecSAT's engine does. A carrier watcher on the
//! averaged spectrum starts a worker on anything new, and a P channel's
//! C-channel assignments start one where a call is about to appear: that is
//! "voice follow".

pub mod channel;
pub mod feed;
pub mod net;
pub mod scanner;
pub mod survey;

use channel::{ChannelId, ChannelStatus, Event, Kind, Request};
use num_complex::Complex32;
use rustfft::FftPlanner;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// The widest vector unit the DSP loops use on this CPU (SSE2, AVX2 or AVX-512).
pub fn vector_level() -> &'static str {
    decaero_dsp::simd::vector_level()
}

/// Where samples come from.
pub trait Source: Send {
    fn rate(&self) -> f64;
    /// Up to `n` samples; `None` at the end.
    fn read(&mut self, n: usize) -> Option<Vec<Complex32>>;
    fn describe(&self) -> String;
    /// A live source cannot be made to wait: a slow worker loses blocks.
    fn live(&self) -> bool {
        false
    }
    /// One real signal rather than I and Q (a receiver's audio): the lower
    /// half of the band is then only the mirror image of the upper.
    fn real(&self) -> bool {
        false
    }
    /// Tune a radio to a new centre frequency (nothing for a recording).
    fn tune(&mut self, _center_hz: f64) {}
}

/// A recording played from memory.
pub struct FileSource {
    name: String,
    rate: f64,
    samples: Vec<Complex32>,
    pos: usize,
    pub repeat: bool,
    real: bool,
}

impl FileSource {
    pub fn open(path: &std::path::Path, repeat: bool) -> std::io::Result<Self> {
        let rec = decaero_dsp::wav::read(path)?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("recording")
            .to_string();
        Ok(Self {
            name,
            rate: rec.sample_rate,
            real: rec.real,
            samples: rec.samples,
            pos: 0,
            repeat,
        })
    }
}

impl Source for FileSource {
    fn rate(&self) -> f64 {
        self.rate
    }
    fn read(&mut self, n: usize) -> Option<Vec<Complex32>> {
        if self.pos >= self.samples.len() {
            if !self.repeat || self.samples.is_empty() {
                return None;
            }
            self.pos = 0;
        }
        let end = (self.pos + n).min(self.samples.len());
        let out = self.samples[self.pos..end].to_vec();
        self.pos = end;
        Some(out)
    }
    fn describe(&self) -> String {
        format!("{} · {:.0} kS/s", self.name, self.rate / 1e3)
    }
    fn real(&self) -> bool {
        self.real
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    pub fft_size: usize,
    /// Play a file at the speed it was recorded (otherwise as fast as the
    /// workers take it).
    pub realtime: bool,
    /// Centre frequency of the input in Hz, 0 if unknown. Needed to follow a
    /// P channel's voice assignments.
    pub center_hz: f64,
    /// Start a decoder on every new carrier and every assignment.
    pub follow: bool,
    /// dB over the floor a carrier must reach to be given a decoder.
    pub carrier_db: f32,
    /// Where decoded messages and positions are sent on to.
    pub feeds: feed::FeedOptions,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            fft_size: 8192,
            realtime: true,
            center_hz: 0.0,
            follow: true,
            carrier_db: 4.0,
            feeds: feed::FeedOptions::default(),
        }
    }
}

/// One line of the message log.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub seq: u64,
    pub seconds: f64,
    pub channel: ChannelId,
    pub kind: Kind,
    pub aes: Option<u32>,
    pub ges: Option<u8>,
    pub title: String,
    pub text: String,
    /// True for ACARS and other user data (the Messages tab), false for
    /// signalling units.
    pub message: bool,
}

/// One voice call, for the calls table and the activity map.
#[derive(Debug, Clone)]
pub struct Call {
    pub channel: ChannelId,
    pub carrier_hz: f64,
    pub aes: Option<u32>,
    pub ges: Option<u8>,
    pub start_s: f64,
    pub end_s: Option<f64>,
    pub frames: u64,
}

/// A snapshot for the GUI.
#[derive(Debug, Clone, Default)]
pub struct View {
    pub source: String,
    pub rate: f64,
    pub center_hz: f64,
    /// Averaged spectrum in dB, lowest frequency first.
    pub spectrum_db: Vec<f32>,
    pub floor_db: f32,
    pub row_seq: u64,
    pub seconds: f64,
    pub ended: bool,
    pub paused: bool,
    pub channels: Vec<ChannelStatus>,
    pub calls: Vec<Call>,
    pub log_seq: u64,
    /// Blocks a live input could not hand to a busy worker.
    pub dropped_blocks: u64,
    /// Front end load, share of one core.
    pub load: f32,
    pub listening: Option<ChannelId>,
    /// The input is one real signal: only the upper half of the band counts.
    pub real: bool,
    /// How the feeds out are doing.
    pub feeds: feed::FeedCounts,
}

pub enum Command {
    /// Start a decoder on whatever Aero carrier lies within `width_hz`
    /// around this offset from the centre: it works out the bit rate itself.
    Add {
        offset_hz: f64,
        width_hz: f64,
    },
    Remove(ChannelId),
    Listen(Option<ChannelId>),
    Scan(bool),
    Follow(bool),
    Pause(bool),
    SetCenter(f64),
    /// Change where the feeds go.
    Feeds(feed::FeedOptions),
}

struct Worker {
    id: ChannelId,
    offset_hz: f64,
    width_hz: f64,
    tx: SyncSender<Arc<Vec<Complex32>>>,
    status: Arc<Mutex<ChannelStatus>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    closed: bool,
}

struct Shared {
    view: Mutex<View>,
    rows: Mutex<VecDeque<(u64, Vec<f32>)>>,
    log: Mutex<VecDeque<LogLine>>,
    commands: Mutex<Vec<Command>>,
    /// Decoded audio of the channel being listened to, 8 kHz.
    audio: Mutex<VecDeque<i16>>,
    /// Voice for the recorder: (channel, samples), drained by the app.
    recorded: Mutex<Vec<(ChannelId, Vec<i16>)>>,
    stop: AtomicBool,
}

pub struct Engine {
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

impl Engine {
    pub fn start(source: Box<dyn Source>, options: Options) -> Self {
        let shared = Arc::new(Shared {
            view: Mutex::new(View {
                source: source.describe(),
                rate: source.rate(),
                center_hz: options.center_hz,
                ..View::default()
            }),
            rows: Mutex::new(VecDeque::new()),
            log: Mutex::new(VecDeque::new()),
            commands: Mutex::new(Vec::new()),
            audio: Mutex::new(VecDeque::new()),
            recorded: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let s = shared.clone();
        let join = std::thread::Builder::new()
            .name("front-end".into())
            .spawn(move || front_end(source, options, s))
            .expect("start the front end thread");
        Self {
            shared,
            join: Some(join),
        }
    }

    pub fn view(&self) -> View {
        self.shared.view.lock().unwrap().clone()
    }

    /// Waterfall rows newer than `after` (dB, lowest frequency first).
    pub fn rows_after(&self, after: u64) -> Vec<(u64, Vec<f32>)> {
        self.shared
            .rows
            .lock()
            .unwrap()
            .iter()
            .filter(|(seq, _)| *seq > after)
            .cloned()
            .collect()
    }

    /// Log lines newer than `after`.
    pub fn log_after(&self, after: u64) -> Vec<LogLine> {
        self.shared
            .log
            .lock()
            .unwrap()
            .iter()
            .filter(|l| l.seq > after)
            .cloned()
            .collect()
    }

    pub fn send(&self, command: Command) {
        self.shared.commands.lock().unwrap().push(command);
    }

    /// Take up to `max` samples of the listened channel's audio (8 kHz).
    pub fn take_audio(&self, max: usize) -> Vec<i16> {
        let mut a = self.shared.audio.lock().unwrap();
        let n = max.min(a.len());
        a.drain(..n).collect()
    }

    /// A handle the audio callback can pull from without the engine.
    pub fn audio_tap(&self) -> AudioTap {
        AudioTap {
            shared: self.shared.clone(),
        }
    }

    /// Voice decoded since the last call, per channel, for the recorder.
    pub fn take_recorded(&self) -> Vec<(ChannelId, Vec<i16>)> {
        std::mem::take(&mut *self.shared.recorded.lock().unwrap())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Lets the sound card's callback fetch audio (it must not block for long).
#[derive(Clone)]
pub struct AudioTap {
    shared: Arc<Shared>,
}

impl AudioTap {
    /// Pop one 8 kHz sample, if there is one.
    pub fn pop(&self) -> Option<i16> {
        self.shared.audio.lock().ok()?.pop_front()
    }
    pub fn buffered(&self) -> usize {
        self.shared.audio.lock().map(|a| a.len()).unwrap_or(0)
    }
}

fn front_end(mut source: Box<dyn Source>, mut options: Options, shared: Arc<Shared>) {
    let rate = source.rate();
    let block_len = (rate / 20.0) as usize;
    // No longer than about two blocks, so a narrow input (a receiver's audio)
    // still gets several waterfall rows a second.
    // Bins of about 60 Hz whatever the rate, so a 600 bit/s carrier is a
    // clear lump from a radio at several MS/s too.
    let wanted = ((rate / 60.0) as usize)
        .next_power_of_two()
        .clamp(options.fft_size, 1 << 16);
    let n = wanted.min(block_len.next_power_of_two()).max(256);
    let mut fft_in: Vec<Complex32> = Vec::new();
    let fft = FftPlanner::<f32>::new().plan_fft_forward(n);
    let window: Vec<f32> = (0..n)
        .map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())
        .collect();
    let mut buf = vec![Complex32::default(); n];
    let mut average = vec![0f32; n];
    let mut have_average = false;

    let (event_tx, event_rx) = mpsc::channel::<Event>();
    let mut workers: Vec<Worker> = Vec::new();
    // Places that turned out not to be Aero: (offset, seconds when tried).
    let mut rejected: Vec<(f64, f64)> = Vec::new();
    // Places the deep scan named where nothing then decoded.
    let mut scan_rejected: Vec<(f64, f64)> = Vec::new();
    // Decoders the user deleted: (offset, reach). Nothing is started there
    // again by itself until the user adds one or a call is assigned there.
    let mut deleted: Vec<(f64, f64)> = Vec::new();
    let mut deleted_ids: Vec<ChannelId> = Vec::new();
    let mut feeds = feed::Feeds::start(options.feeds.clone());
    let mut next_id: ChannelId = 1;
    let mut row_seq = 0u64;
    let mut log_seq = 0u64;
    let mut seconds = 0.0f64;
    let mut blocks = 0u64;
    let mut paused = false;
    let mut scan = true;
    let mut listening: Option<ChannelId> = None;
    let mut calls: Vec<Call> = Vec::new();
    let mut dropped = 0u64;
    let started = Instant::now();
    let mut load = 0f32;
    let samples_seen = Arc::new(AtomicU64::new(0));

    let mirror_below = source.real();
    let by_hand = std::cell::Cell::new(false);
    let spawn = |workers: &mut Vec<Worker>,
                 next_id: &mut ChannelId,
                 offset_hz: f64,
                 width_hz: f64,
                 rates: Vec<u32>,
                 seconds: f64,
                 known_rate: Option<u32>| {
        // The lower half of a real input is the upper half again, backwards.
        if mirror_below && offset_hz < 0.0 {
            by_hand.set(false);
            return 0;
        }
        let id = *next_id;
        *next_id += 1;
        let (tx, rx) = mpsc::sync_channel(64);
        let status = Arc::new(Mutex::new(ChannelStatus::new(id, offset_hz, seconds)));
        let stop = Arc::new(AtomicBool::new(false));
        let req = Request {
            id,
            in_rate: rate,
            offset_hz,
            rates,
            search_hz: (width_hz * 0.75).max(700.0),
            started_s: seconds,
            known_rate,
            // Set just before a decoder the user placed is started.
            persistent: by_hand.replace(false),
        };
        let (st, ev, sp, seen) = (
            status.clone(),
            event_tx.clone(),
            stop.clone(),
            samples_seen.clone(),
        );
        let join = std::thread::Builder::new()
            .name(format!("channel-{id}"))
            .spawn(move || channel::run(req, rx, st, ev, sp, seen))
            .ok();
        workers.push(Worker {
            id,
            offset_hz,
            width_hz,
            tx,
            status,
            stop,
            join,
            closed: false,
        });
        id
    };

    // The deep scan has its own thread and takes what it can keep up with.
    let (scan_tx, scan_rx) = mpsc::sync_channel::<Arc<Vec<Complex32>>>(64);
    let scan_stop = Arc::new(AtomicBool::new(false));
    let scan_join = {
        let (ev, sp) = (event_tx.clone(), scan_stop.clone());
        std::thread::Builder::new()
            .name("deep-scan".into())
            .spawn(move || scanner::run(rate, scan_rx, ev, sp))
            .ok()
    };

    while !shared.stop.load(Ordering::Relaxed) {
        // Commands from the GUI.
        for c in std::mem::take(&mut *shared.commands.lock().unwrap()) {
            match c {
                Command::Add {
                    offset_hz,
                    width_hz,
                } => {
                    // A narrow box can only hold a slow carrier; a wide one
                    // may hold anything, the fast rates being likeliest.
                    by_hand.set(true);
                    let width = width_hz.clamp(1000.0, 40_000.0);
                    deleted.retain(|(hz, reach)| (hz - offset_hz).abs() > reach + width / 2.0);
                    let rates = if width < 2500.0 {
                        vec![600, 1200]
                    } else {
                        vec![8400, 10500, 1200, 600]
                    };
                    spawn(
                        &mut workers,
                        &mut next_id,
                        offset_hz,
                        width,
                        rates,
                        seconds,
                        None,
                    );
                }
                Command::Remove(id) => {
                    for w in workers.iter().filter(|w| w.id == id) {
                        w.stop.store(true, Ordering::Relaxed);
                        // Where the carrier was found, which may be off the
                        // place the decoder was put.
                        let carrier = w.status.lock().unwrap().carrier_hz;
                        let reach = w.width_hz / 2.0 + 1000.0;
                        deleted.push((w.offset_hz, reach));
                        if (carrier - w.offset_hz).abs() > 200.0 {
                            deleted.push((carrier, reach));
                        }
                        deleted_ids.push(id);
                    }
                }
                Command::Listen(id) => {
                    listening = id;
                    scan = false;
                    shared.audio.lock().unwrap().clear();
                }
                Command::Feeds(o) => feeds.configure(o),
                Command::Scan(on) => scan = on,
                Command::Follow(on) => options.follow = on,
                Command::Pause(on) => paused = on,
                Command::SetCenter(hz) => {
                    options.center_hz = hz;
                    source.tune(hz);
                }
            }
        }
        if paused {
            shared.view.lock().unwrap().paused = true;
            std::thread::sleep(Duration::from_millis(30));
            continue;
        }
        let Some(block) = source.read(block_len) else {
            // End of a file: let the workers finish what they have.
            for w in &workers {
                w.stop.store(true, Ordering::Relaxed);
            }
            drain_events(
                &event_rx,
                &shared,
                &mut log_seq,
                &mut calls,
                &workers,
                listening,
                options.center_hz,
                &mut Vec::new(),
                &mut Vec::new(),
                &feeds,
            );
            let mut v = shared.view.lock().unwrap();
            v.ended = true;
            v.channels = workers
                .iter()
                .map(|w| w.status.lock().unwrap().clone())
                .collect();
            v.calls.clone_from(&calls);
            drop(v);
            std::thread::sleep(Duration::from_millis(50));
            if shared.stop.load(Ordering::Relaxed) {
                break;
            }
            continue;
        };
        let began = Instant::now();
        blocks += 1;
        seconds += block.len() as f64 / rate;

        // One waterfall row per block: the block's segments averaged.
        let mut power = vec![0f32; n];
        let mut segments = 0;
        fft_in.extend_from_slice(&block);
        let whole = fft_in.len() / n * n;
        for seg in fft_in[..whole].chunks_exact(n) {
            for ((b, &s), &w) in buf.iter_mut().zip(seg).zip(&window) {
                *b = s * w;
            }
            fft.process(&mut buf);
            for (p, b) in power.iter_mut().zip(&buf) {
                *p += b.norm_sqr();
            }
            segments += 1;
        }
        fft_in.drain(..whole);
        if segments > 0 {
            let scale = 1.0 / (segments as f32 * n as f32 * n as f32 * 0.375);
            let row: Vec<f32> = (0..n)
                .map(|i| 10.0 * (power[(i + n / 2) % n] * scale).max(1e-20).log10())
                .collect();
            if have_average {
                for (a, &r) in average.iter_mut().zip(&row) {
                    *a += 0.08 * (r - *a);
                }
            } else {
                average.clone_from(&row);
                have_average = true;
            }
            row_seq += 1;
            let mut rows = shared.rows.lock().unwrap();
            rows.push_back((row_seq, row));
            while rows.len() > 600 {
                rows.pop_front();
            }
        }

        // Hand the block to every worker.
        let block = Arc::new(block);
        if source.live() {
            let _ = scan_tx.try_send(block.clone());
        } else {
            let _ = scan_tx.send(block.clone());
        }
        for w in workers.iter_mut().filter(|w| !w.closed) {
            if source.live() {
                match w.tx.try_send(block.clone()) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => dropped += 1,
                    Err(TrySendError::Disconnected(_)) => w.closed = true,
                }
            } else if w.tx.send(block.clone()).is_err() {
                w.closed = true;
            }
        }

        // What the workers found.
        let mut assignments = Vec::new();
        let mut finds = Vec::new();
        drain_events(
            &event_rx,
            &shared,
            &mut log_seq,
            &mut calls,
            &workers,
            listening,
            options.center_hz,
            &mut assignments,
            &mut finds,
            &feeds,
        );

        // Voice follow, part three: what the deep scan found under the noise.
        if options.follow {
            for (offset, bit_rate) in finds {
                // A find in the mirror half of a real input is the carrier
                // at the same distance above the centre.
                let offset = if mirror_below { offset.abs() } else { offset };
                let wide = bit_rate > 1200;
                let reach = if wide { 4000.0 } else { 900.0 };
                // Two carriers squared together leave a line pair half way
                // between them: not a carrier.
                let carriers: Vec<f64> = workers
                    .iter()
                    .filter(|w| !w.closed)
                    .map(|w| w.status.lock().unwrap())
                    .filter(|s| s.locked)
                    .map(|s| s.carrier_hz)
                    .collect();
                let ghost = carriers.iter().enumerate().any(|(i, a)| {
                    carriers[i + 1..]
                        .iter()
                        .any(|b| ((a + b) / 2.0 - offset).abs() < 300.0)
                });
                // A known carrier seen again through the scan's filter bank.
                let step = scanner::alias_step(rate);
                let echo = workers.iter().filter(|w| !w.closed).any(|w| {
                    let s = w.status.lock().unwrap();
                    let same_rate = matches!(s.kind, Kind::P(r) | Kind::Rt(r) if r == bit_rate)
                        || (s.kind == Kind::C && bit_rate == 8400);
                    let steps = (offset - s.carrier_hz) / step;
                    same_rate && steps.round() != 0.0 && (steps - steps.round()).abs() * step < 8.0
                });
                if ghost || echo || deleted.iter().any(|(hz, r)| (hz - offset).abs() < *r) {
                    continue;
                }
                // A decoder still searching here gives way: the scan knows
                // what the carrier is.
                let mut busy = false;
                for w in workers.iter().filter(|w| !w.closed) {
                    if (w.offset_hz - offset).abs() < reach + w.width_hz / 2.0 {
                        if w.status.lock().unwrap().kind == Kind::Searching {
                            w.stop.store(true, Ordering::Relaxed);
                        } else {
                            busy = true;
                        }
                    }
                }
                // The survey's verdict does not count here: it gives up on a
                // weak carrier that the scan can still name.
                if busy
                    || scan_rejected
                        .iter()
                        .any(|(hz, _)| (hz - offset).abs() < 1000.0)
                {
                    continue;
                }
                spawn(
                    &mut workers,
                    &mut next_id,
                    offset,
                    if wide { 6000.0 } else { 1500.0 },
                    vec![bit_rate],
                    seconds,
                    Some(bit_rate),
                );
            }
        }

        // Voice follow, part one: go where a P channel sends an aircraft.
        if options.follow && options.center_hz > 0.0 {
            for a in assignments {
                let offset = a.rx_mhz * 1e6 - options.center_hz;
                let busy = workers
                    .iter()
                    .any(|w| !w.closed && (w.offset_hz - offset).abs() < 4000.0);
                if offset.abs() < rate / 2.0 - 5000.0 && !busy {
                    // A new call there is news: the deletion no longer holds.
                    deleted.retain(|(hz, reach)| (hz - offset).abs() > *reach);
                    spawn(
                        &mut workers,
                        &mut next_id,
                        offset,
                        6000.0,
                        vec![8400],
                        seconds,
                        None,
                    );
                }
            }
        }

        // Voice follow, part two: a decoder for every new carrier.
        if blocks.is_multiple_of(10) && have_average {
            let found = survey::carriers(&average, rate, options.carrier_db);
            rejected.retain(|(_, when)| seconds - when < 60.0);
            scan_rejected.retain(|(_, when)| seconds - when < 60.0);
            if options.follow {
                for c in &found.carriers {
                    let near = |hz: f64, reach: f64| (hz - c.center_hz).abs() < reach;
                    let busy = workers.iter().any(|w| {
                        !w.closed && near(w.offset_hz, (w.width_hz + c.width_hz) / 2.0 + 300.0)
                    });
                    if busy
                        || rejected.iter().any(|(hz, _)| near(*hz, 1000.0))
                        || deleted.iter().any(|(hz, r)| near(*hz, *r))
                        || workers.iter().filter(|w| !w.closed).count() >= 48
                    {
                        continue;
                    }
                    // Wide carriers are left to the deep scan, which names a
                    // weak one far more surely than a short look here can.
                    if c.width_hz >= 2200.0 {
                        continue;
                    }
                    let rates = vec![600, 1200];
                    spawn(
                        &mut workers,
                        &mut next_id,
                        c.center_hz,
                        c.width_hz,
                        rates,
                        seconds,
                        None,
                    );
                }
            }
            shared.view.lock().unwrap().floor_db = found.floor_db;
        }

        // Retire finished workers (their last status stays visible a while).
        for w in &mut workers {
            if w.closed
                && let Some(j) = w.join.take()
            {
                let _ = j.join();
            }
        }
        let now = seconds;
        workers.retain(|w| {
            if !w.closed {
                return true;
            }
            // Deleted by the user: gone at once.
            if let Some(i) = deleted_ids.iter().position(|id| *id == w.id) {
                deleted_ids.swap_remove(i);
                return false;
            }
            let s = w.status.lock().unwrap();
            // Nothing there, or nothing that ever decoded: leave the place
            // alone for a minute.
            if s.kind == Kind::NotAero {
                rejected.push((w.offset_hz, now));
                return false;
            }
            if s.frames == 0 {
                // Named by the scan but never decoded: leave it a minute. A
                // search that was stopped to make way is simply dropped.
                if s.kind != Kind::Ended {
                    scan_rejected.push((w.offset_hz, now));
                }
                return false;
            }
            now - s.last_good_s < 20.0
        });

        // Which call the loudspeaker gets.
        let alive = |id: ChannelId| workers.iter().any(|w| w.id == id && !w.closed);
        if scan && listening.is_none_or(|id| !alive(id)) {
            listening = workers
                .iter()
                .filter(|w| !w.closed)
                .filter(|w| {
                    let s = w.status.lock().unwrap();
                    s.kind == Kind::C && s.locked
                })
                .map(|w| w.id)
                .next_back();
        }

        load = 0.9 * load + 0.1 * (began.elapsed().as_secs_f64() * 20.0) as f32;
        {
            let mut v = shared.view.lock().unwrap();
            v.spectrum_db.clone_from(&average);
            v.row_seq = row_seq;
            v.seconds = seconds;
            v.paused = false;
            v.ended = false;
            v.center_hz = options.center_hz;
            v.channels = workers
                .iter()
                .map(|w| w.status.lock().unwrap().clone())
                .collect();
            v.calls.clone_from(&calls);
            v.log_seq = log_seq;
            v.dropped_blocks = dropped;
            v.load = load;
            v.listening = listening;
            v.feeds = feeds.counts();
            v.real = mirror_below;
            // A live source says here when its link has gone.
            if source.live() {
                v.source = source.describe();
            }
        }

        // A file is played at the speed it was recorded.
        if options.realtime && !source.live() {
            let due = Duration::from_secs_f64(blocks as f64 / 20.0);
            if let Some(wait) = due.checked_sub(started.elapsed()) {
                std::thread::sleep(wait.min(Duration::from_millis(200)));
            }
        }
    }
    scan_stop.store(true, Ordering::Relaxed);
    drop(scan_tx);
    if let Some(j) = scan_join {
        let _ = j.join();
    }
    for w in &workers {
        w.stop.store(true, Ordering::Relaxed);
    }
    for mut w in workers {
        drop(w.tx);
        if let Some(j) = w.join.take() {
            let _ = j.join();
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[allow(clippy::too_many_arguments)]
fn drain_events(
    rx: &mpsc::Receiver<Event>,
    shared: &Shared,
    log_seq: &mut u64,
    calls: &mut Vec<Call>,
    workers: &[Worker],
    listening: Option<ChannelId>,
    center_hz: f64,
    assignments: &mut Vec<decaero_core::su::CAssignment>,
    finds: &mut Vec<(f64, u32)>,
    feeds: &feed::Feeds,
) {
    let mut lines = Vec::new();
    let mut line =
        |seconds: f64, channel, kind, aes, ges, title: String, text: String, message: bool| {
            *log_seq += 1;
            lines.push(LogLine {
                seq: *log_seq,
                seconds,
                channel,
                kind,
                aes,
                ges,
                title,
                text,
                message,
            });
        };
    while let Ok(ev) = rx.try_recv() {
        match ev {
            Event::Unit {
                channel,
                kind,
                seconds,
                bytes,
                name,
            } => {
                let (aes, ges) = match decaero_core::su::p_meaning(&bytes) {
                    decaero_core::su::Meaning::Aircraft { aes, ges } => (Some(aes), Some(ges)),
                    decaero_core::su::Meaning::Assignment(a) => (Some(a.aes), Some(a.ges)),
                    _ if kind == Kind::C && matches!(bytes[0], 0x30 | 0x60) => (
                        Some(
                            (u32::from(bytes[1]) << 16)
                                | (u32::from(bytes[2]) << 8)
                                | u32::from(bytes[3]),
                        ),
                        Some(bytes[4]),
                    ),
                    _ => (None, None),
                };
                let detail = match decaero_core::su::p_meaning(&bytes) {
                    decaero_core::su::Meaning::Assignment(a) if kind != Kind::C => {
                        format!(
                            "rx {:.4} MHz  tx {:.4} MHz{}",
                            a.rx_mhz,
                            a.tx_mhz,
                            if a.spot_beam { "  spot beam" } else { "" }
                        )
                    }
                    decaero_core::su::Meaning::Channel {
                        ges, mhz, bit_rate, ..
                    } if kind != Kind::C => {
                        format!("GES {ges:02X}  {mhz:.4} MHz at {bit_rate} bit/s")
                    }
                    decaero_core::su::Meaning::Satellite {
                        id,
                        longitude,
                        psmc_mhz,
                    } if kind != Kind::C => {
                        format!(
                            "satellite {id} at {longitude:.1}°E  Psmc {:.4} MHz",
                            psmc_mhz[0]
                        )
                    }
                    _ => hex(&bytes[..10]),
                };
                if let Some(c) = calls
                    .iter_mut()
                    .rev()
                    .find(|c| c.channel == channel && c.end_s.is_none())
                {
                    c.aes = aes.or(c.aes);
                    c.ges = ges.or(c.ges);
                }
                line(
                    seconds,
                    channel,
                    kind,
                    aes,
                    ges,
                    name.to_string(),
                    detail,
                    false,
                );
            }
            Event::Acars {
                channel,
                seconds,
                link,
                acars,
            } => {
                // What an aircraft's own text says: its flight and, in an
                // ADS-C report, where it is.
                let down = link
                    .from_aircraft
                    .then(|| decaero_core::adsc::parse(&acars.text));
                feeds.acars(&link, &acars, down.as_ref());
                let title = format!(
                    "ACARS {} label {} block {}{}",
                    acars.registration,
                    acars.label,
                    acars.block_id,
                    if acars.parity_ok {
                        ""
                    } else {
                        " (parity error)"
                    }
                );
                let mut text = acars.text;
                if let Some(d) = down.filter(|d| !d.groups.is_empty()) {
                    text = format!(
                        "{}
ADS-C {}",
                        text.trim_end(),
                        d.describe()
                    );
                }
                line(
                    seconds,
                    channel,
                    Kind::P(0),
                    Some(link.aes),
                    Some(link.ges),
                    title,
                    text,
                    true,
                );
            }
            Event::Data {
                channel,
                seconds,
                aes,
                ges,
                bytes,
            } => {
                line(
                    seconds,
                    channel,
                    Kind::P(0),
                    Some(aes),
                    Some(ges),
                    format!("user data, {} octets", bytes.len()),
                    hex(&bytes),
                    true,
                );
            }
            Event::Assignment {
                channel,
                seconds,
                assignment,
            } => {
                let where_ = if center_hz > 0.0 {
                    format!(
                        "{:+.1} kHz from centre",
                        (assignment.rx_mhz * 1e6 - center_hz) / 1e3
                    )
                } else {
                    "set the centre frequency to follow it".to_string()
                };
                line(
                    seconds,
                    channel,
                    Kind::P(0),
                    Some(assignment.aes),
                    Some(assignment.ges),
                    "voice channel assigned".into(),
                    format!("rx {:.4} MHz  {where_}", assignment.rx_mhz),
                    true,
                );
                assignments.push(assignment);
            }
            Event::Voice { channel, pcm } => {
                let w = workers.iter().find(|w| w.id == channel);
                match calls
                    .iter_mut()
                    .rev()
                    .find(|c| c.channel == channel && c.end_s.is_none())
                {
                    Some(c) => c.frames += 1,
                    None => {
                        let (carrier_hz, start_s) = w
                            .map(|w| w.status.lock().unwrap())
                            .map(|s| (s.carrier_hz, s.last_good_s))
                            .unwrap_or((0.0, 0.0));
                        calls.push(Call {
                            channel,
                            carrier_hz,
                            aes: None,
                            ges: None,
                            start_s,
                            end_s: None,
                            frames: 1,
                        });
                    }
                }
                if listening == Some(channel) {
                    let mut a = shared.audio.lock().unwrap();
                    a.extend(pcm.iter().copied());
                    // Never more than two seconds behind.
                    while a.len() > 16_000 {
                        a.pop_front();
                    }
                }
                shared.recorded.lock().unwrap().push((channel, pcm));
            }
            Event::Found {
                offset_hz,
                bit_rate,
                ..
            } => finds.push((offset_hz, bit_rate)),
            Event::Log {
                channel,
                kind,
                seconds,
                aes,
                ges,
                title,
                text,
            } => line(seconds, channel, kind, aes, ges, title, text, false),
            Event::Closed { channel } => {
                if let Some(c) = calls
                    .iter_mut()
                    .rev()
                    .find(|c| c.channel == channel && c.end_s.is_none())
                {
                    let end = workers
                        .iter()
                        .find(|w| w.id == channel)
                        .map(|w| w.status.lock().unwrap().last_good_s)
                        .unwrap_or(c.start_s);
                    c.end_s = Some(end);
                }
            }
        }
    }
    if !lines.is_empty() {
        let mut log = shared.log.lock().unwrap();
        log.extend(lines);
        while log.len() > 4000 {
            log.pop_front();
        }
    }
}
