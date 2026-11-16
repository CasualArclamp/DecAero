// made by CasualArclamp
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! DecAero's window: three columns in DecSAT's colours.
//!
//! Left: the channels and, for the selected one, the constellation with a
//! margin bar and every figure of its decoder. Middle: the band (spectrum and
//! waterfall with a mark per decoder), the calls and the channel activity.
//! Right: messages and the aircraft seen.
//!
//! `decaero-gui [FILE.wav | --net URL [--rate HZ] [--iq int8|uint8|int16|float32] | --radio hackrf|airspy|pluto|sdrplay|fobos|rtl|uhd [--rate HZ] [--bias-tee]] [--center MHZ] [--fast] [--screenshot OUT.png --after SECS] [--select HZ] [--view LO_HZ:HI_HZ] [--add HZ:WIDTH] [--out-dir DIR] [--activity FILE] [--prefs FILE | --no-prefs]`

mod activity;
mod aircraft;
mod audio;
mod band;
mod prefs;
mod recorder;
mod theme;

use decaero_engine::channel::{ChannelId, ChannelStatus, Kind};
use decaero_engine::feed::FeedOptions;
use decaero_engine::{Command, Engine, FileSource, LogLine, Options, Source, View, net};
use eframe::egui::{
    self, Align2, Color32, FontId, Frame, Margin, RichText, Sense, Stroke, Ui, pos2, vec2,
};
use num_complex::Complex32;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use theme::*;

/// Points kept for the constellation (it is redrawn every frame; the trail
/// is what makes it look alive between the decoder's updates).
const TRAIL: usize = 1800;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Messages,
    Signalling,
}

/// What the space under the band shows.
#[derive(PartialEq, Clone, Copy)]
enum Lower {
    Calls,
    Activity,
}

/// A live input once it is open: the source, and a radio's gain control.
type Opened = Result<
    (
        Box<dyn Source>,
        Option<Arc<dyn decaero_radio::RadioControl>>,
    ),
    String,
>;

/// Which aircraft to list.
#[derive(PartialEq, Clone, Copy)]
enum Filter {
    All,
    Us,
    Military,
}

/// What the command line set, which the kept settings must not undo.
struct Given {
    center: bool,
    net: bool,
    feeds: bool,
    radio: bool,
}

struct Shot {
    path: PathBuf,
    after: f64,
    requested: Option<Instant>,
    /// Frames drawn and the time they took, for a line on how evenly the
    /// window was drawn: (frames, seconds, longest frame, frames over 25 ms).
    frames: (u32, f32, f32, u32),
}

struct App {
    engine: Option<Engine>,
    view: View,
    band: band::Band,
    /// The view last told to the engine.
    shown_sent: Option<(f64, f64, usize)>,
    rows_seen: u64,
    log: VecDeque<LogLine>,
    log_seen: u64,
    selected: Option<ChannelId>,
    /// Where messages and positions are sent on to, as in use and as being
    /// typed in the Feeds window.
    feeds: FeedOptions,
    feeds_edit: FeedOptions,
    feeds_open: bool,
    select_hz: Option<f64>,
    tab: Tab,
    center_text: String,
    follow: bool,
    scan: bool,
    fast: bool,
    paused: bool,
    volume: f32,
    audio: Option<audio::Output>,
    /// Saves each call as Ogg Opus; `None` on unattended runs.
    recorder: Option<recorder::Recorder>,
    trail: VecDeque<Complex32>,
    trail_of: Option<ChannelId>,
    last_scatter: Vec<Complex32>,
    shot: Option<Shot>,
    started: Instant,
    error: Option<String>,
    aircraft: aircraft::Lookup,
    /// The network input: its URL, and for a plain stream its rate (kS/s)
    /// and sample format.
    net_url: String,
    net_rate_text: String,
    net_format: net::Format,
    net_eight_bit: bool,
    connecting: Option<std::sync::mpsc::Receiver<Opened>>,
    /// The radios this build drives, which one is picked, its rate and gains.
    radios: Vec<decaero_radio::RadioInfo>,
    radio_pick: usize,
    radio_rate: f64,
    radio_gains: Vec<decaero_radio::Gain>,
    /// Power on the antenna port for an LNA, applied when the radio starts.
    radio_bias: bool,
    /// Gain control of the radio that is running.
    radio_control: Option<Arc<dyn decaero_radio::RadioControl>>,
    /// A radio was asked for on the command line: show its gains at once.
    radio_arg: bool,
    /// The engine is on a live input (no pause, no file speed).
    live: bool,
    /// Which aircraft the right-hand table shows.
    filter: Filter,
    /// Settings kept between runs, where they are kept (`None`: this run
    /// neither reads nor writes them), and when they were last looked at.
    prefs: prefs::Prefs,
    prefs_path: Option<PathBuf>,
    prefs_checked: Instant,
    /// Which frequencies were in use when: the history, its picture, the
    /// file a live input's history is kept in, and (for a recording) the
    /// clock time its first sample is taken to have.
    lower: Lower,
    activity: activity::Activity,
    activity_panel: activity::Panel,
    activity_path: Option<PathBuf>,
    activity_always: bool,
    file_began: f64,
}

impl App {
    fn center_hz(&self) -> f64 {
        self.center_text
            .trim()
            .parse::<f64>()
            .map(|m| m * 1e6)
            .unwrap_or(0.0)
    }

    /// Start the engine on a source, a recording or a live one.
    fn start(&mut self, source: Box<dyn Source>) {
        let live = source.live();
        let engine = Engine::start(
            source,
            Options {
                realtime: !self.fast,
                center_hz: self.center_hz(),
                follow: self.follow,
                feeds: self.feeds.clone(),
                ..Options::default()
            },
        );
        if let Some(a) = &mut self.audio {
            a.attach(engine.audio_tap());
        }
        self.engine = Some(engine);
        // A live input carries on the kept history; a recording gets a
        // history of its own, as if it had begun just now.
        self.file_began = recorder::unix_now();
        self.activity = match &self.activity_path {
            Some(path) if live || self.activity_always => {
                activity::Activity::kept(path, (self.file_began / 60.0) as u32)
            }
            _ => activity::Activity::new(),
        };
        // The engine starts with scan on.
        if !self.scan {
            self.send(Command::Scan(false));
        }
        self.live = live;
        self.paused = false;
        self.band.clear();
        self.shown_sent = None;
        self.rows_seen = 0;
        self.log.clear();
        self.log_seen = 0;
        self.selected = None;
        self.error = None;
    }

    fn open(&mut self, path: &std::path::Path) {
        match FileSource::open(path, true) {
            Ok(source) => self.start(Box::new(source)),
            Err(e) => self.error = Some(format!("{}: {e}", path.display())),
        }
    }

    /// Connect to the network input in the URL box. Connecting can take a
    /// while (or fail slowly), so it is done on a thread and picked up when
    /// it is ready.
    fn connect(&mut self) {
        let url = self.net_url.trim().to_string();
        let rate = self
            .net_rate_text
            .trim()
            .parse::<f64>()
            .map(|k| k * 1e3)
            .unwrap_or(0.0);
        let format = self.net_format;
        if !url.starts_with("sdrpp://") && rate <= 0.0 {
            self.error = Some("give the sample rate in kS/s for a plain stream".into());
            return;
        }
        let options = net::SdrppOptions {
            center_hz: self.center_hz(),
            eight_bit: self.net_eight_bit,
            compressed: true,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let opened = net::open(&url, rate, format, options)
                .map(|s| (Box::new(s) as Box<dyn Source>, None))
                .map_err(|e| format!("{url}: {e}"));
            let _ = tx.send(opened);
        });
        self.connecting = Some(rx);
        self.error = None;
    }

    /// Open the picked radio. This takes the device, so it happens only on
    /// the user's click (or `--radio`), and on a thread: some take a while.
    fn start_radio(&mut self) {
        let Some(info) = self.radios.get(self.radio_pick).cloned() else {
            return;
        };
        // Let go of whatever is running first: it may be this same radio.
        self.engine = None;
        self.radio_control = None;
        let request = decaero_radio::RadioRequest {
            key: info.key.clone(),
            center_hz: self.center_hz(),
            sample_rate: self.radio_rate,
            gains: self
                .radio_gains
                .iter()
                .map(|g| (g.name.clone(), g.value))
                .collect(),
            bias_tee: self.radio_bias,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let opened = decaero_radio::open(&request)
                .map(|s| {
                    let control = s.control();
                    (Box::new(s) as Box<dyn Source>, control)
                })
                .map_err(|e| format!("{}: {e}", info.name));
            let _ = tx.send(opened);
        });
        self.connecting = Some(rx);
        self.error = None;
    }

    /// The picked radio changed: take its lowest rate and its default gains.
    fn pick_radio(&mut self, index: usize) {
        self.radio_pick = index;
        if let Some(info) = self.radios.get(index) {
            self.radio_rate = info.rates.first().copied().unwrap_or(2e6);
            self.radio_gains = info.gains.clone();
            // What was last used with this radio, where it still fits.
            if let Some(rate) = self.prefs.read::<f64>(&format!("rate.{}", info.key))
                && info.rates.iter().any(|r| (r - rate).abs() < 1.0)
            {
                self.radio_rate = rate;
            }
            for g in &mut self.radio_gains {
                if let Some(v) = self
                    .prefs
                    .read::<f64>(&format!("gain.{}.{}", info.key, g.name))
                {
                    g.value = v.clamp(g.min, g.max);
                }
            }
        }
    }

    /// Put the settings as they are now into `prefs`.
    fn remember(&mut self) {
        let p = &mut self.prefs;
        p.set("center_mhz", self.center_text.trim());
        p.set("follow", self.follow);
        p.set("scan", self.scan);
        p.set("volume", format!("{:.2}", self.volume));
        p.set(
            "tab",
            match self.tab {
                Tab::Messages => "messages",
                Tab::Signalling => "signalling",
            },
        );
        p.set(
            "aircraft",
            match self.filter {
                Filter::All => "all",
                Filter::Us => "us",
                Filter::Military => "military",
            },
        );
        p.set(
            "lower",
            match self.lower {
                Lower::Calls => "calls",
                Lower::Activity => "activity",
            },
        );
        p.set("activity_span", self.activity_panel.span.label());
        p.set("net_url", &self.net_url);
        p.set("net_rate_ks", self.net_rate_text.trim());
        p.set("net_format", self.net_format.label());
        p.set("net_8bit", self.net_eight_bit);
        p.set("json_udp", &self.feeds.json_udp);
        p.set("sbs", &self.feeds.sbs);
        p.set("station", &self.feeds.station);
        p.set("bias_tee", self.radio_bias);
        if let Some(info) = self.radios.get(self.radio_pick) {
            p.set("radio", &info.key);
            p.set(&format!("rate.{}", info.key), self.radio_rate);
            for g in &self.radio_gains {
                p.set(&format!("gain.{}.{}", info.key, g.name), g.value);
            }
        }
    }

    /// Take the settings of the last run. What the command line gave stays.
    fn recall(&mut self, given: &Given) {
        let p = self.prefs.clone();
        if !given.center
            && let Some(v) = p.get("center_mhz")
        {
            self.center_text = v.to_string();
        }
        self.follow = p.read("follow").unwrap_or(self.follow);
        self.scan = p.read("scan").unwrap_or(self.scan);
        self.volume = p
            .read::<f32>("volume")
            .unwrap_or(self.volume)
            .clamp(0.0, 1.0);
        if let Some(a) = &self.audio {
            a.set_volume(self.volume);
        }
        if p.get("tab") == Some("signalling") {
            self.tab = Tab::Signalling;
        }
        self.filter = match p.get("aircraft") {
            Some("us") => Filter::Us,
            Some("military") => Filter::Military,
            _ => Filter::All,
        };
        if p.get("lower") == Some("activity") {
            self.lower = Lower::Activity;
        }
        if let Some(span) = p.get("activity_span").and_then(activity::Span::parse) {
            self.activity_panel.span = span;
        }
        if !given.net {
            if let Some(v) = p.get("net_url").filter(|v| !v.is_empty()) {
                self.net_url = v.to_string();
            }
            if let Some(v) = p.get("net_rate_ks").filter(|v| !v.is_empty()) {
                self.net_rate_text = v.to_string();
            }
            if let Some(f) = p.get("net_format").and_then(net::Format::parse) {
                self.net_format = f;
            }
            self.net_eight_bit = p.read("net_8bit").unwrap_or(false);
        }
        if !given.feeds {
            self.feeds = FeedOptions {
                json_udp: p.get("json_udp").unwrap_or_default().to_string(),
                sbs: p.get("sbs").unwrap_or_default().to_string(),
                station: p.get("station").unwrap_or_default().to_string(),
            };
            self.feeds_edit = self.feeds.clone();
        }
        if !given.radio {
            // Only a choice: no radio is opened until Start radio is pressed.
            self.radio_bias = p.read("bias_tee").unwrap_or(false);
            if let Some(i) = p
                .get("radio")
                .and_then(|key| self.radios.iter().position(|r| r.key == key))
            {
                self.pick_radio(i);
            }
        }
    }

    /// Once a second: write the settings if they have changed.
    fn keep_prefs(&mut self) {
        let Some(path) = self.prefs_path.clone() else {
            return;
        };
        if self.prefs_checked.elapsed().as_secs_f32() < 1.0 {
            return;
        }
        self.prefs_checked = Instant::now();
        let before = self.prefs.clone();
        self.remember();
        if self.prefs != before
            && let Err(e) = self.prefs.save(&path)
        {
            self.error = Some(format!("settings not saved to {}: {e}", path.display()));
            // Do not try again every second.
            self.prefs_path = None;
        }
    }

    /// The time on the activity history's clock: the real one for a live
    /// input, the recording's own for a file (so a fast or paused playback
    /// is laid out as it was recorded).
    fn activity_now(&self) -> f64 {
        if self.live {
            recorder::unix_now()
        } else {
            self.file_began + self.view.seconds
        }
    }

    fn send(&self, c: Command) {
        if let Some(e) = &self.engine {
            e.send(c);
        }
    }

    fn pull(&mut self) {
        // A connection that was being made.
        if let Some(rx) = &self.connecting
            && let Ok(result) = rx.try_recv()
        {
            self.connecting = None;
            match result {
                Ok((source, control)) => {
                    self.radio_control = control;
                    self.start(source);
                }
                Err(e) => self.error = Some(e),
            }
        }
        let Some(engine) = &self.engine else { return };
        self.view = engine.view();
        let rows = engine.rows_after(self.rows_seen);
        if let Some((seq, _)) = rows.last() {
            self.rows_seen = *seq;
        }
        self.band
            .push_rows(rows.into_iter().map(|(_, r)| r), self.view.floor_db);
        // Tell the engine what is on screen when that changes: zoomed in,
        // it makes a finer spectrum of just that part.
        if self.band.shown != self.shown_sent
            && let Some((lo_hz, hi_hz, pixels)) = self.band.shown
        {
            self.shown_sent = self.band.shown;
            engine.send(Command::Show {
                lo_hz,
                hi_hz,
                pixels,
            });
        }
        for line in engine.log_after(self.log_seen) {
            self.log_seen = line.seq;
            self.log.push_back(line);
        }
        while self.log.len() > 3000 {
            self.log.pop_front();
        }
        // Every call goes to its own Ogg Opus file.
        let voice = engine.take_recorded();
        if let Some(rec) = &mut self.recorder {
            for (channel, pcm) in &voice {
                let hz = self
                    .view
                    .channels
                    .iter()
                    .find(|c| c.id == *channel)
                    .map_or(0.0, |c| c.carrier_hz);
                rec.feed(*channel, hz, pcm);
            }
            rec.center_hz = self.view.center_hz;
            let ended: Vec<_> = self
                .view
                .calls
                .iter()
                .filter(|c| c.end_s.is_some() && rec.recording(c.channel))
                .map(|c| recorder::Ended {
                    channel: c.channel,
                    aes: c.aes,
                    ges: c.ges,
                    aircraft: c
                        .aes
                        .map(|a| self.aircraft.get(a).short())
                        .unwrap_or_default(),
                })
                .collect();
            rec.tidy(&ended);
        }
        // Which frequencies are in use, for the activity map.
        let now = self.activity_now();
        self.activity
            .note(now, self.view.center_hz, &self.view.channels);
        if let Some(e) = self.activity.error.take() {
            self.error = Some(e);
        }

        // Keep a selection: the one asked for on the command line, else the
        // call being listened to, else the first channel.
        if let Some(hz) = self.select_hz
            && let Some(c) = self.view.channels.iter().min_by(|a, b| {
                (a.carrier_hz - hz)
                    .abs()
                    .total_cmp(&(b.carrier_hz - hz).abs())
            })
            && (c.carrier_hz - hz).abs() < 3000.0
        {
            self.selected = Some(c.id);
        }
        if self
            .selected
            .is_none_or(|id| !self.view.channels.iter().any(|c| c.id == id))
        {
            self.selected = self
                .view
                .listening
                .or_else(|| self.view.channels.first().map(|c| c.id));
        }
        if self.trail_of != self.selected {
            self.trail.clear();
            self.trail_of = self.selected;
            self.last_scatter.clear();
        }
        if let Some(c) = self
            .selected
            .and_then(|id| self.view.channels.iter().find(|c| c.id == id))
            && c.scatter != self.last_scatter
        {
            self.last_scatter.clone_from(&c.scatter);
            self.trail.extend(c.scatter.iter().copied());
            while self.trail.len() > TRAIL {
                self.trail.pop_front();
            }
        }
    }

    fn screenshot(&mut self, ctx: &egui::Context) {
        let Some(shot) = &mut self.shot else { return };
        // The first seconds (start-up, textures being made) are left out.
        let dt = ctx.input(|i| i.unstable_dt);
        if self.started.elapsed().as_secs_f32() > 3.0 && shot.requested.is_none() {
            let f = &mut shot.frames;
            *f = (f.0 + 1, f.1 + dt, f.2.max(dt), f.3 + u32::from(dt > 0.025));
        }
        let image = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            let rgba: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
            match image::save_buffer(
                &shot.path,
                &rgba,
                image.size[0] as u32,
                image.size[1] as u32,
                image::ColorType::Rgba8,
            ) {
                Ok(()) => {
                    let (n, total, worst, slow) = shot.frames;
                    eprintln!(
                        "frames: {n}, mean {:.1} ms, longest {:.1} ms, {slow} over 25 ms",
                        total / n.max(1) as f32 * 1e3,
                        worst * 1e3
                    );
                    eprintln!("screenshot saved to {}", shot.path.display())
                }
                Err(e) => eprintln!("cannot save {}: {e}", shot.path.display()),
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if shot.requested.is_none() && self.started.elapsed().as_secs_f64() >= shot.after {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            shot.requested = Some(Instant::now());
        } else if shot.requested.is_some_and(|t| t.elapsed().as_secs() > 3) {
            eprintln!("screenshot not delivered; quitting");
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn selected(&self) -> Option<&ChannelStatus> {
        self.selected
            .and_then(|id| self.view.channels.iter().find(|c| c.id == id))
    }

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let running = self.engine.is_some() && !self.paused;
            let label = if running { "⏸ Pause" } else { "▶ Play" };
            let button = egui::Button::new(RichText::new(label).size(15.0).color(if running { GOOD } else { TEXT }))
                .fill(if running { GREEN_FILL } else { PANEL2 });
            if ui.add(button).clicked() && self.engine.is_some() {
                self.paused = !self.paused;
                self.send(Command::Pause(self.paused));
            }
            if ui.button("Open…").clicked()
                && let Some(path) = rfd::FileDialog::new().add_filter("recordings", &["wav"]).pick_file()
            {
                self.open(&path);
            }
            ui.separator();
            ui.add(egui::TextEdit::singleline(&mut self.net_url).desired_width(170.0).hint_text("sdrpp://host:5259"))
                .on_hover_text("sdrpp://HOST[:5259] for an SDR++ server; tcp://HOST:PORT, listen://ADDR:PORT or udp://ADDR:PORT for plain I/Q");
            if self.net_url.trim().starts_with("sdrpp://") {
                ui.checkbox(&mut self.net_eight_bit, "8 bit").on_hover_text("Half the traffic; each block is scaled to its peak");
            } else {
                ui.add(egui::TextEdit::singleline(&mut self.net_rate_text).desired_width(46.0)).on_hover_text("sample rate, kS/s");
                egui::ComboBox::from_id_salt("net-format").selected_text(self.net_format.label()).width(64.0).show_ui(ui, |ui| {
                    for f in [net::Format::I8, net::Format::U8, net::Format::I16, net::Format::F32] {
                        ui.selectable_value(&mut self.net_format, f, f.label());
                    }
                });
            }
            if self.connecting.is_some() {
                ui.label(RichText::new("connecting…").color(WARN));
            } else if ui.button("Connect").clicked() {
                self.connect();
            }
            ui.separator();
            // A radio of this machine.
            if !self.radios.is_empty() {
                let picked = self.radios.get(self.radio_pick).map_or(String::new(), |r| r.name.clone());
                let mut pick = self.radio_pick;
                egui::ComboBox::from_id_salt("radio").selected_text(picked).width(150.0).show_ui(ui, |ui| {
                    for (i, r) in self.radios.iter().enumerate() {
                        ui.selectable_value(&mut pick, i, &r.name);
                    }
                });
                if pick != self.radio_pick {
                    self.pick_radio(pick);
                }
                let rates = self.radios.get(self.radio_pick).map(|r| r.rates.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt("radio-rate")
                    .selected_text(format!("{:.3} MS/s", self.radio_rate / 1e6))
                    .width(86.0)
                    .show_ui(ui, |ui| {
                        for r in rates {
                            ui.selectable_value(&mut self.radio_rate, r, format!("{:.3} MS/s", r / 1e6));
                        }
                    });
                // Only for radios that have one, and never on by itself.
                if let Some(volts) = self.radios.get(self.radio_pick).and_then(|r| r.bias_tee) {
                    ui.checkbox(&mut self.radio_bias, "bias-T").on_hover_text(format!(
                        "{volts} on the antenna port for an LNA. Takes effect when the radio is started."
                    ));
                } else {
                    self.radio_bias = false;
                }
                if self.connecting.is_none()
                    && ui.button("Start radio").on_hover_text("Opens the radio, receive only, tuned to the centre frequency").clicked()
                {
                    self.start_radio();
                }
                ui.separator();
            }
            ui.label(RichText::new("centre").color(DIM));
            let edit = ui.add(egui::TextEdit::singleline(&mut self.center_text).desired_width(92.0).hint_text("MHz"));
            if edit.lost_focus() {
                let hz = self.center_text.trim().parse::<f64>().map(|m| m * 1e6).unwrap_or(0.0);
                self.send(Command::SetCenter(hz));
            }
            ui.label(RichText::new("MHz").color(DIM));
            ui.separator();
            if ui.toggle_value(&mut self.follow, "Follow voice").on_hover_text("Start a decoder on every new carrier and every voice channel a P channel assigns").changed() {
                self.send(Command::Follow(self.follow));
            }
            if ui.toggle_value(&mut self.scan, "Scan").on_hover_text("Listen to the newest call").changed() {
                self.send(Command::Scan(self.scan));
            }
            ui.toggle_value(&mut self.feeds_open, "Feeds").on_hover_text("Send ACARS messages (JSON over UDP) and ADS-C positions (SBS) to other programs");
            ui.label(RichText::new("vol").color(DIM));
            if ui.add(egui::Slider::new(&mut self.volume, 0.0..=1.0).show_value(false)).changed()
                && let Some(a) = &self.audio
            {
                a.set_volume(self.volume);
            }
            ui.separator();
            ui.label(RichText::new(&self.view.source).color(DIM));
            if let Some(e) = &self.error {
                ui.label(RichText::new(e).color(BAD));
            }
        });
    }

    /// Gain sliders of the picked radio; they act at once on a running one.
    fn gain_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("gain").color(DIM));
            let mut changed = Vec::new();
            for g in &mut self.radio_gains {
                if g.max - g.min <= 1.0 && g.step >= 1.0 {
                    // A switch.
                    let mut on = g.value >= 0.5;
                    if ui.checkbox(&mut on, &g.name).changed() {
                        g.value = f64::from(u8::from(on));
                        changed.push((g.name.clone(), g.value));
                    }
                } else {
                    ui.label(RichText::new(&g.name).color(DIM));
                    let slider = egui::Slider::new(&mut g.value, g.min..=g.max).step_by(g.step);
                    if ui.add(slider).changed() {
                        changed.push((g.name.clone(), g.value));
                    }
                }
            }
            if let Some(control) = &self.radio_control {
                for (name, value) in changed {
                    if let Err(e) = control.set_gain(&name, value) {
                        self.error = Some(e.to_string());
                    }
                }
            }
        });
    }

    fn feeds_window(&mut self, ui: &mut Ui) {
        let mut open = self.feeds_open;
        egui::Window::new("Feeds")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ui.ctx(), |ui| {
                egui::Grid::new("feeds").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("ACARS as JSON over UDP");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.feeds_edit.json_udp)
                            .hint_text("host:port, host:port")
                            .desired_width(260.0),
                    )
                    .on_hover_text("One datagram a message, in JAERO's JSONdump layout. Several collectors: separate them with commas.");
                    ui.end_row();
                    ui.label("ADS-C positions as SBS");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.feeds_edit.sbs)
                            .hint_text("listen:30003  or  host:port")
                            .desired_width(260.0),
                    )
                    .on_hover_text("BaseStation MSG,3 lines over TCP. listen:PORT lets a map connect to us (listen:127.0.0.1:PORT for this computer only); host:port connects to the map.");
                    ui.end_row();
                    ui.label("Station name");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.feeds_edit.station)
                            .hint_text("optional, goes in the JSON")
                            .desired_width(260.0),
                    );
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    let changed = self.feeds_edit != self.feeds;
                    if ui.add_enabled(changed, egui::Button::new("Apply")).clicked() {
                        self.feeds = self.feeds_edit.clone();
                        self.send(Command::Feeds(self.feeds.clone()));
                    }
                    let f = &self.view.feeds;
                    let state = if !f.error.is_empty() {
                        RichText::new(f.error.clone()).color(BAD)
                    } else {
                        RichText::new(format!(
                            "JSON: {} sent to {} · SBS: {} sent, {} connected",
                            f.json_sent, f.json_targets, f.sbs_sent, f.sbs_peers
                        ))
                        .color(DIM)
                    };
                    ui.label(state.size(11.0));
                });
                ui.label(
                    RichText::new("Positions come from aircraft (R and T channels, C-band); the L-band P channels carry the ground's side.")
                        .color(DIM)
                        .size(11.0),
                );
            });
        self.feeds_open = open;
    }

    fn status_bar(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let v = &self.view;
            let state = if self.engine.is_none() {
                "no input: open a recording"
            } else if v.paused {
                "paused"
            } else if v.ended {
                "ended"
            } else {
                "playing"
            };
            let live = v.channels.iter().filter(|c| !c.closed).count();
            let locked = v.channels.iter().filter(|c| c.locked).count();
            let calls = v
                .channels
                .iter()
                .filter(|c| c.kind == Kind::C && c.locked)
                .count();
            let cpu: f32 = v.channels.iter().map(|c| c.cpu).sum::<f32>() + v.load;
            for part in [
                state.to_string(),
                format!("{:.0} s", v.seconds),
                format!("{live} decoders, {locked} locked"),
                format!("{calls} calls"),
                format!("CPU {:.0}% of a core", cpu * 100.0),
                format!(
                    "{} · {}",
                    decaero_engine::vector_level(),
                    format_args!("{} blocks lost", v.dropped_blocks)
                ),
                match &self.recorder {
                    Some(r) if r.open_calls() > 0 => format!(
                        "recording {} call(s) to {}",
                        r.open_calls(),
                        r.dir().display()
                    ),
                    Some(r) => r.error.clone().unwrap_or_else(|| match &r.last {
                        Some(p) => format!("last call saved: {}", p.display()),
                        None => format!("calls are saved to {}", r.dir().display()),
                    }),
                    None => "not recording".to_string(),
                },
                {
                    let f = &v.feeds;
                    if !f.error.is_empty() {
                        f.error.clone()
                    } else if f.json_targets == 0 && !f.sbs_on {
                        "feeds off".to_string()
                    } else {
                        let mut parts = Vec::new();
                        if f.json_targets > 0 {
                            parts.push(format!("JSON {} sent", f.json_sent));
                        }
                        if f.sbs_on {
                            parts.push(format!(
                                "SBS {} sent, {} connected",
                                f.sbs_sent, f.sbs_peers
                            ));
                        }
                        parts.join(" · ")
                    }
                },
            ] {
                ui.label(RichText::new(part).color(DIM));
                ui.separator();
            }
        });
    }

    fn left(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Channels").color(WHITE).size(14.0));
        let mut channels: Vec<&ChannelStatus> = self.view.channels.iter().collect();
        channels.sort_by(|a, b| a.carrier_hz.total_cmp(&b.carrier_hz));
        let mut pick = None;
        let mut listen = None;
        let mut delete = None;
        egui::ScrollArea::vertical()
            .id_salt("channels")
            .max_height(230.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("channel-table")
                    .num_columns(6)
                    .spacing([8.0, 3.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for h in ["", "kHz", "mode", "Eb/N0", "FEC", ""] {
                            ui.label(RichText::new(h).color(DIM).size(11.0));
                        }
                        ui.end_row();
                        for c in channels {
                            let (tag, colour) = tag(c);
                            chip(ui, tag, colour);
                            let hz = format!("{:+.2}", c.carrier_hz / 1e3);
                            let on = self.selected == Some(c.id);
                            if ui
                                .selectable_label(on, RichText::new(hz).monospace())
                                .clicked()
                            {
                                pick = Some(c.id);
                            }
                            let listening = self.view.listening == Some(c.id);
                            let mode =
                                format!("{}{}", c.kind.label(), if listening { " 🔊" } else { "" });
                            if ui.selectable_label(listening, mode).clicked() && c.kind == Kind::C {
                                listen = Some(if listening { None } else { Some(c.id) });
                            }
                            if has_figures(c) {
                                ui.label(format!("{:.1}", c.eb_n0_db));
                                load_bar(ui, c.fec_load, 46.0);
                            } else {
                                ui.label(RichText::new("–").color(DIM));
                                ui.label(
                                    RichText::new(if c.closed { "" } else { "search" })
                                        .color(ORANGE)
                                        .size(11.0),
                                );
                            }
                            if ui
                                .small_button(RichText::new("×").color(DIM))
                                .on_hover_text("Delete this decoder")
                                .clicked()
                            {
                                delete = Some(c.id);
                            }
                            ui.end_row();
                        }
                    });
            });
        // Del deletes the selected decoder, unless a text box has the keys.
        if delete.is_none()
            && !ui.ctx().egui_wants_keyboard_input()
            && ui.input(|i| i.key_pressed(egui::Key::Delete))
        {
            delete = self.selected;
        }
        if let Some(id) = delete {
            self.send(Command::Remove(id));
        }
        if let Some(id) = pick {
            self.selected = Some(id);
            self.select_hz = None;
        }
        if let Some(l) = listen {
            self.scan = false;
            self.send(Command::Listen(l));
        }
        ui.separator();
        let Some(c) = self.selected().cloned() else {
            ui.label(RichText::new("No channel yet.").color(DIM));
            return;
        };
        egui::ScrollArea::vertical()
            .id_salt("decoder")
            .auto_shrink([false, false])
            .show(ui, |ui| self.decoder_panel(ui, &c));
    }

    fn decoder_panel(&mut self, ui: &mut Ui, c: &ChannelStatus) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "{:+.2} kHz · Aero {}",
                    c.carrier_hz / 1e3,
                    c.kind.label()
                ))
                .color(WHITE)
                .size(14.0),
            );
            let (tag, colour) = tag(c);
            chip(ui, tag, colour);
            if c.kind == Kind::C {
                let listening = self.view.listening == Some(c.id);
                if ui.selectable_label(listening, "🔊 Listen").clicked() {
                    self.scan = false;
                    self.send(Command::Listen(if listening { None } else { Some(c.id) }));
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button(RichText::new("× Delete").color(BAD))
                    .on_hover_text("Delete this decoder (Del). It is not started here again until you add one.")
                    .clicked()
                {
                    self.send(Command::Remove(c.id));
                }
            });
        });
        // The constellation is square, with the margin bar at its side.
        let bar_w = 50.0;
        let side = (ui.available_width() - bar_w - 8.0).clamp(120.0, 300.0);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
            constellation(ui, rect, &self.trail);
            let (bar, _) = ui.allocate_exact_size(vec2(bar_w, side), Sense::hover());
            margin_bar(ui, bar, c);
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Margin").color(DIM));
            if has_figures(c) {
                let colour = margin_colour(c.margin_db);
                ui.label(
                    RichText::new(format!("{:+.1} dB", c.margin_db))
                        .color(colour)
                        .size(16.0)
                        .monospace(),
                );
                ui.label(RichText::new("above the decode limit").color(DIM));
            } else {
                ui.label(RichText::new("not locked").color(ORANGE));
            }
        });
        ui.label(
            RichText::new("Eb/N0 · last two minutes")
                .color(DIM)
                .size(11.0),
        );
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
        trend(ui, rect, &c.history);

        let code = match c.kind {
            Kind::C => "Viterbi K=7 · rate 2/3",
            _ => "Viterbi K=7 · rate 1/2",
        };
        section(ui, "Signal");
        rows(
            ui,
            "signal",
            &[
                (
                    "Eb/N0",
                    format!("{:.1} dB  (Es/N0 {:.1} dB)", c.eb_n0_db, c.es_n0_db),
                    None,
                ),
                (
                    "Carrier",
                    format!(
                        "{:+.1} Hz  drift {:+.1} Hz since acquired",
                        c.carrier_hz,
                        c.carrier_hz - c.acquired_hz
                    ),
                    None,
                ),
                ("Clock", format!("{:+.0} ppm", c.clock_ppm), None),
                ("Coherence", format!("{:.2}", c.coherence), None),
            ],
        );
        section(ui, "FEC");
        let bad = c.crc_bad > 0;
        rows(
            ui,
            "fec",
            &[
                ("Code", code.to_string(), None),
                (
                    "FEC load",
                    format!("{:.0}%", c.fec_load * 100.0),
                    Some(load_colour(c.fec_load)),
                ),
                (
                    "Pre-FEC BER",
                    format!("{:.1e}", c.pre_fec_ber),
                    Some(load_colour(c.fec_load)),
                ),
                ("Corrected", format!("{} bits", c.corrected_bits), None),
                (
                    "Unique word",
                    format!("{} of {} bits wrong", c.uw_wrong, c.uw_bits),
                    None,
                ),
                (
                    "CRC",
                    format!("{} good · {} bad", c.crc_good, c.crc_bad),
                    Some(if bad { WARN } else { GOOD }),
                ),
                (
                    "Frames",
                    format!("{} · {} lock losses", c.frames, c.lock_losses),
                    None,
                ),
                ("Bursts", format!("{} decoded", c.bursts), None),
            ],
        );
        if c.kind == Kind::C {
            section(ui, "Voice");
            rows(
                ui,
                "voice",
                &[
                    ("Codec", "AMBE 4.8k".to_string(), None),
                    (
                        "Frames",
                        format!("{} voiced · {} repeated", c.voice[0], c.voice[1]),
                        None,
                    ),
                    (
                        "",
                        format!("{} silent · {} tone", c.voice[2], c.voice[3]),
                        None,
                    ),
                    ("Audio", format!("peak {:.0}%", c.audio_level * 100.0), None),
                ],
            );
        }
        section(ui, if c.kind == Kind::C { "Call" } else { "Channel" });
        let centre = self.view.center_hz;
        rows(
            ui,
            "call",
            &[
                (
                    "Aircraft",
                    c.aes.map_or("–".into(), |a| self.who(a)),
                    Some(GOOD),
                ),
                (
                    "Type",
                    c.aes
                        .map_or(String::new(), |a| self.aircraft.get(a).description),
                    None,
                ),
                (
                    "Operator",
                    c.aes.map_or(String::new(), |a| self.aircraft.get(a).owner),
                    None,
                ),
                (
                    "Ground",
                    c.ges.map_or("–".into(), |g| format!("GES {g:02X}")),
                    None,
                ),
                (
                    "Frequency",
                    if centre > 0.0 {
                        format!("{:.4} MHz", (centre + c.carrier_hz) / 1e6)
                    } else {
                        "set the centre frequency".into()
                    },
                    None,
                ),
                (
                    "Running",
                    format!(
                        "{} · since {}",
                        clock(self.view.seconds - c.started_s),
                        clock(c.started_s)
                    ),
                    None,
                ),
                (
                    "Last unit",
                    if c.last_unit.is_empty() {
                        "–".into()
                    } else {
                        c.last_unit.clone()
                    },
                    None,
                ),
                ("CPU", format!("{:.1}% of a core", c.cpu * 100.0), None),
            ],
        );
    }

    fn right(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Log").color(WHITE).size(14.0));
            ui.selectable_value(&mut self.tab, Tab::Messages, "Messages");
            ui.selectable_value(&mut self.tab, Tab::Signalling, "Signalling");
        });
        let want_messages = self.tab == Tab::Messages;
        let height = (ui.available_height() * 0.62).max(120.0);
        egui::ScrollArea::vertical()
            .id_salt("log")
            .max_height(height)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut shown = 0;
                for l in self.log.iter().rev().filter(|l| l.message == want_messages) {
                    shown += 1;
                    if shown > 300 {
                        break;
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new(clock(l.seconds))
                                .color(DIM)
                                .monospace()
                                .size(11.0),
                        );
                        if let Some(a) = l.aes {
                            ui.label(
                                RichText::new(format!("{a:06X}"))
                                    .color(GOOD)
                                    .monospace()
                                    .size(11.0),
                            );
                            let info = self.aircraft.get(a);
                            if !info.short().is_empty() {
                                ui.label(RichText::new(info.short()).color(TEXT).size(11.0));
                            }
                            if info.military {
                                chip(ui, "MIL", AMBER);
                            }
                        }
                        ui.label(
                            RichText::new(&l.title)
                                .color(if l.message { VFO } else { TEXT })
                                .size(12.0),
                        );
                    });
                    if !l.text.trim().is_empty() {
                        ui.label(
                            RichText::new(l.text.trim_end())
                                .monospace()
                                .size(11.0)
                                .color(if l.message { WHITE } else { DIM }),
                        );
                    }
                    ui.add_space(3.0);
                }
                if shown == 0 {
                    ui.label(RichText::new("Nothing yet.").color(DIM));
                }
            });
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Aircraft seen").color(WHITE).size(14.0));
            ui.selectable_value(&mut self.filter, Filter::All, "All");
            ui.selectable_value(&mut self.filter, Filter::Us, "US");
            ui.selectable_value(&mut self.filter, Filter::Military, "Military");
        });
        // The database is fetched only when asked for.
        match self.aircraft.state() {
            aircraft::State::Missing => {
                ui.horizontal(|ui| {
                    if ui
                        .button("Get the aircraft database")
                        .on_hover_text(aircraft::DATABASE_URL)
                        .clicked()
                    {
                        self.aircraft.download();
                    }
                    ui.label(
                        RichText::new("9 MB, for registrations and types")
                            .color(DIM)
                            .size(11.0),
                    );
                });
            }
            aircraft::State::Downloading => {
                ui.label(
                    RichText::new("fetching the aircraft database…")
                        .color(WARN)
                        .size(11.0),
                );
            }
            aircraft::State::Loading => {
                ui.label(
                    RichText::new("reading the aircraft database…")
                        .color(DIM)
                        .size(11.0),
                );
            }
            aircraft::State::Failed(e) => {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("aircraft database: {e}"))
                            .color(BAD)
                            .size(11.0),
                    );
                    if ui.small_button("retry").clicked() {
                        self.aircraft.download();
                    }
                });
            }
            aircraft::State::Ready { .. } => {}
        }
        // Newest first, one line per aircraft.
        let mut seen: Vec<(u32, f64, &str, usize)> = Vec::new();
        for l in self.log.iter().rev() {
            let Some(aes) = l.aes else { continue };
            match seen.iter_mut().find(|s| s.0 == aes) {
                Some(s) => s.3 += 1,
                None => seen.push((aes, l.seconds, l.title.as_str(), 1)),
            }
        }
        let filter = self.filter;
        egui::ScrollArea::vertical()
            .id_salt("aircraft")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("aircraft-table")
                    .num_columns(6)
                    .spacing([8.0, 2.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for h in ["AES", "reg", "type", "", "last", "what"] {
                            ui.label(RichText::new(h).color(DIM).size(11.0));
                        }
                        ui.end_row();
                        let mut shown = 0;
                        for (aes, seconds, what, _n) in &seen {
                            let info = self.aircraft.get(*aes);
                            let keep = match filter {
                                Filter::All => true,
                                Filter::Us => info.country == "US",
                                Filter::Military => info.military,
                            };
                            if !keep {
                                continue;
                            }
                            shown += 1;
                            if shown > 200 {
                                break;
                            }
                            ui.label(RichText::new(format!("{aes:06X}")).color(GOOD).monospace());
                            ui.label(&info.registration);
                            ui.label(RichText::new(&info.type_code).size(11.0))
                                .on_hover_text(format!("{}\n{}", info.description, info.owner));
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(info.country).color(DIM).size(11.0));
                                if info.military {
                                    chip(ui, "MIL", AMBER);
                                }
                            });
                            ui.label(RichText::new(clock(*seconds)).monospace().size(11.0));
                            ui.label(RichText::new(*what).size(11.0));
                            ui.end_row();
                        }
                    });
            });
    }

    /// An aircraft in one line: address, registration, type, state.
    fn who(&self, aes: u32) -> String {
        let info = self.aircraft.get(aes);
        let mut text = format!("{aes:06X}");
        for part in [info.short(), info.country.to_string()] {
            if !part.is_empty() {
                text.push_str("  ");
                text.push_str(&part);
            }
        }
        if info.military {
            text.push_str("  MIL");
        }
        text
    }

    fn centre(&mut self, ui: &mut Ui) {
        let total = ui.available_height();
        let band_h = (total * 0.62).max(220.0);
        let (rect, response) =
            ui.allocate_exact_size(vec2(ui.available_width(), band_h), Sense::click_and_drag());
        let action = self
            .band
            .show(ui, rect, &response, &self.view, self.selected);
        match action {
            band::Action::Select(id) => {
                self.selected = Some(id);
                self.select_hz = None;
            }
            band::Action::Add {
                offset_hz,
                width_hz,
            } => self.send(Command::Add {
                offset_hz,
                width_hz,
            }),
            band::Action::None => {}
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.lower, Lower::Calls, "Calls");
            ui.selectable_value(&mut self.lower, Lower::Activity, "Activity")
                .on_hover_text("Which frequencies were in use, and when");
            if self.lower == Lower::Activity {
                ui.separator();
                for span in activity::Span::ALL {
                    ui.selectable_value(&mut self.activity_panel.span, span, span.label());
                }
                let kept = (self.live || self.activity_always) && self.activity_path.is_some();
                ui.label(
                    RichText::new(if kept {
                        "kept between runs"
                    } else if self.live {
                        "this run only"
                    } else {
                        "this recording only, timed as if it began when opened"
                    })
                    .color(DIM)
                    .size(11.0),
                );
            } else {
                ui.label(
                    RichText::new("click a carrier to select it · drag across one to add a decoder (it finds the mode itself)")
                        .color(DIM)
                        .size(11.0),
                );
            }
        });
        if self.lower == Lower::Activity {
            let now = self.activity_now();
            self.activity_panel.show(ui, &self.activity, now);
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("calls")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Grid::new("calls-table")
                    .num_columns(6)
                    .spacing([14.0, 3.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for h in ["start", "kHz", "aircraft", "ground", "length", ""] {
                            ui.label(RichText::new(h).color(DIM).size(11.0));
                        }
                        ui.end_row();
                        for call in self.view.calls.iter().rev().take(100) {
                            let live = call.end_s.is_none();
                            ui.label(RichText::new(clock(call.start_s)).monospace());
                            ui.label(
                                RichText::new(format!("{:+.2}", call.carrier_hz / 1e3)).monospace(),
                            );
                            ui.label(
                                RichText::new(call.aes.map_or("–".into(), |a| self.who(a)))
                                    .color(GOOD)
                                    .monospace(),
                            );
                            ui.label(call.ges.map_or("–".into(), |g| format!("{g:02X}")));
                            ui.label(clock(
                                call.end_s.unwrap_or(self.view.seconds) - call.start_s,
                            ));
                            if live {
                                let on = self.view.listening == Some(call.channel);
                                if ui
                                    .selectable_label(
                                        on,
                                        if on { "🔊 listening" } else { "Listen" },
                                    )
                                    .clicked()
                                {
                                    self.scan = false;
                                    self.send(Command::Listen(if on {
                                        None
                                    } else {
                                        Some(call.channel)
                                    }));
                                }
                            } else {
                                ui.label(RichText::new("ended").color(DIM));
                            }
                            ui.end_row();
                        }
                    });
                if self.view.calls.is_empty() {
                    ui.label(RichText::new("No voice call yet.").color(DIM));
                }
            });
    }
}

/// Seconds as m:ss.
fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    format!("{}:{:02}", s / 60, s % 60)
}

/// A channel whose quality figures mean something: a locked one, or a burst
/// channel that has decoded a burst.
fn has_figures(c: &ChannelStatus) -> bool {
    c.locked || (matches!(c.kind, Kind::Rt(_)) && c.bursts > 0)
}

fn tag(c: &ChannelStatus) -> (&'static str, Color32) {
    match (c.kind, c.locked) {
        (Kind::NotAero, _) => ("no", DIM),
        _ if c.closed => ("end", DIM),
        (Kind::Ended, _) => ("end", DIM),
        (Kind::Rt(_), _) => ("R/T", PINK),
        (Kind::Searching, _) => ("?", ORANGE),
        (_, false) => ("srch", ORANGE),
        (Kind::C, true) => ("C", GOOD),
        (Kind::P(_), true) => ("P", VFO),
    }
}

fn chip(ui: &mut Ui, text: &str, colour: Color32) {
    Frame::new()
        .fill(colour.gamma_multiply(0.18))
        .corner_radius(2)
        .inner_margin(Margin::symmetric(5, 0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(colour).size(11.0));
        });
}

fn section(ui: &mut Ui, title: &str) {
    ui.add_space(4.0);
    ui.separator();
    ui.label(RichText::new(title).color(WHITE));
}

fn rows(ui: &mut Ui, id: &str, rows: &[(&str, String, Option<Color32>)]) {
    egui::Grid::new(id)
        .num_columns(2)
        .spacing([10.0, 2.0])
        .min_col_width(78.0)
        .show(ui, |ui| {
            for (label, value, colour) in rows {
                ui.label(RichText::new(*label).color(DIM));
                ui.label(RichText::new(value).color(colour.unwrap_or(TEXT)));
                ui.end_row();
            }
        });
}

fn load_colour(load: f32) -> Color32 {
    if load > 0.85 {
        BAD
    } else if load > 0.6 {
        WARN
    } else {
        GOOD
    }
}

fn margin_colour(db: f32) -> Color32 {
    if db < 0.0 {
        BAD
    } else if db < 2.0 {
        WARN
    } else {
        GOOD
    }
}

fn load_bar(ui: &mut Ui, load: f32, width: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(width, 8.0), Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2, WELL_BAR);
    let mut fill = rect;
    fill.set_width(width * load.clamp(0.02, 1.0));
    p.rect_filled(fill, 2, load_colour(load));
}

/// The constellation: square, small dots, redrawn every frame.
fn constellation(ui: &Ui, rect: egui::Rect, trail: &VecDeque<Complex32>) {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0, WELL);
    let c = rect.center();
    let grid = Stroke::new(1.0, GRID);
    p.line_segment([pos2(rect.left(), c.y), pos2(rect.right(), c.y)], grid);
    p.line_segment([pos2(c.x, rect.top()), pos2(c.x, rect.bottom())], grid);
    // The decoder scales the samples to unit power: +-1.8 fits the noise.
    let scale = rect.width() / 2.0 / 1.8;
    let n = trail.len().max(1) as f32;
    for (i, v) in trail.iter().enumerate() {
        // Older points fade.
        let age = 1.0 - i as f32 / n;
        let colour = DOT.gamma_multiply(1.0 - 0.75 * age);
        let at = pos2(c.x + v.re * scale, c.y - v.im * scale);
        p.rect_filled(egui::Rect::from_center_size(at, vec2(1.5, 1.5)), 0, colour);
    }
}

/// Margin over the decode limit as a vertical bar: red below the limit,
/// amber for the first 2 dB, green above; a white line where the signal is.
fn margin_bar(ui: &Ui, rect: egui::Rect, c: &ChannelStatus) {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0, WELL);
    // -4 dB at the bottom to +12 dB at the top.
    let (lo, hi) = (-4.0f32, 12.0f32);
    let y = |db: f32| rect.bottom() - (db.clamp(lo, hi) - lo) / (hi - lo) * rect.height();
    let strip = |from: f32, to: f32, colour: Color32| {
        p.rect_filled(
            egui::Rect::from_min_max(pos2(rect.left(), y(to)), pos2(rect.left() + 8.0, y(from))),
            0,
            colour,
        );
    };
    strip(lo, 0.0, BAD);
    strip(0.0, 2.0, WARN);
    strip(2.0, hi, GREEN_STRIP);
    let font = FontId::proportional(10.0);
    p.line_segment(
        [pos2(rect.left(), y(0.0)), pos2(rect.right(), y(0.0))],
        Stroke::new(1.0, BAD),
    );
    p.text(
        pos2(rect.right() - 2.0, y(0.0) + 1.0),
        Align2::RIGHT_TOP,
        "limit",
        font.clone(),
        DIM,
    );
    for db in [4.0, 8.0, 12.0] {
        p.text(
            pos2(rect.right() - 2.0, y(db)),
            Align2::RIGHT_CENTER,
            format!("+{db:.0}"),
            font.clone(),
            FAINT,
        );
    }
    if has_figures(c) {
        let at = y(c.margin_db);
        p.line_segment(
            [pos2(rect.left(), at), pos2(rect.right(), at)],
            Stroke::new(2.0, WHITE),
        );
        p.text(
            pos2(rect.left() + 11.0, at - 2.0),
            Align2::LEFT_BOTTOM,
            format!("{:+.1}", c.margin_db),
            font,
            WHITE,
        );
    }
}

fn trend(ui: &Ui, rect: egui::Rect, history: &VecDeque<f32>) {
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 0, WELL);
    if history.len() < 2 {
        return;
    }
    let (lo, hi) = (0.0f32, 16.0f32);
    let points: Vec<egui::Pos2> = history
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let x = rect.left() + rect.width() * i as f32 / 119.0;
            pos2(
                x,
                rect.bottom() - (v.clamp(lo, hi) - lo) / (hi - lo) * rect.height(),
            )
        })
        .collect();
    p.add(egui::Shape::line(points, Stroke::new(1.2, GOOD)));
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pull();
        self.keep_prefs();
        self.screenshot(ctx);
        // Every frame: the waterfall and the constellation move at the
        // display's rate (60 frames a second on most screens).
        ctx.request_repaint();
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let dropped: Vec<PathBuf> = ui.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if let Some(path) = dropped.first() {
            self.open(path);
        }
        let plain = |fill: Color32| {
            Frame::new()
                .fill(fill)
                .inner_margin(Margin::symmetric(8, 6))
        };
        egui::Panel::top("top").frame(plain(PANEL)).show(ui, |ui| {
            self.top_bar(ui);
            if self.radio_control.is_some() || self.radio_arg {
                self.gain_bar(ui);
            }
        });
        self.feeds_window(ui);
        egui::Panel::bottom("status")
            .frame(plain(PANEL))
            .show(ui, |ui| self.status_bar(ui));
        egui::Panel::left("left")
            .frame(plain(PANEL))
            .resizable(true)
            .default_size(360.0)
            .min_size(300.0)
            .show(ui, |ui| self.left(ui));
        egui::Panel::right("right")
            .frame(plain(PANEL))
            .resizable(true)
            .default_size(350.0)
            .min_size(260.0)
            .show(ui, |ui| self.right(ui));
        egui::CentralPanel::default()
            .frame(plain(BG))
            .show(ui, |ui| self.centre(ui));
    }
}

fn main() -> eframe::Result {
    let mut args = std::env::args().skip(1);
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("decaero-gui {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let mut file = None;
    let mut center_text = String::new();
    let mut fast = false;
    let mut shot_path = None;
    let mut after = 8.0;
    let mut select_hz = None;
    let mut view_arg: Option<(f64, f64)> = None;
    let mut out_dir = None;
    let mut aircraft_db = None;
    let mut net_url = None;
    let mut radio_key: Option<String> = None;
    let mut rate_given = false;
    let mut bias_arg = false;
    let mut feeds = FeedOptions::default();
    let mut prefs_arg: Option<PathBuf> = None;
    let mut no_prefs = false;
    let mut activity_arg: Option<PathBuf> = None;
    let mut adds: Vec<(f64, f64)> = Vec::new();
    let mut net_rate = 625_000.0f64;
    let mut net_format = net::Format::I16;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--center" => center_text = args.next().unwrap_or_default(),
            "--fast" => fast = true,
            "--screenshot" => shot_path = args.next().map(PathBuf::from),
            "--after" => after = args.next().and_then(|v| v.parse().ok()).unwrap_or(8.0),
            "--select" => select_hz = args.next().and_then(|v| v.parse().ok()),
            "--view" => {
                view_arg = args.next().and_then(|v| {
                    let (lo, hi) = v.split_once(':')?;
                    Some((lo.parse().ok()?, hi.parse().ok()?))
                })
            }
            "--out-dir" => out_dir = args.next().map(PathBuf::from),
            "--aircraft-db" => aircraft_db = args.next().map(PathBuf::from),
            "--json-udp" => feeds.json_udp = args.next().unwrap_or_default(),
            "--sbs" => feeds.sbs = args.next().unwrap_or_default(),
            "--station" => feeds.station = args.next().unwrap_or_default(),
            "--prefs" => prefs_arg = args.next().map(PathBuf::from),
            "--no-prefs" => no_prefs = true,
            "--activity" => activity_arg = args.next().map(PathBuf::from),
            "--net" => net_url = args.next(),
            "--rate" => {
                net_rate = args.next().and_then(|v| v.parse().ok()).unwrap_or(net_rate);
                rate_given = true;
            }
            "--radio" => radio_key = args.next(),
            "--bias-tee" => bias_arg = true,
            // A decoder placed by hand, as a drag on the band: HZ:WIDTH.
            "--add" => {
                if let Some((hz, width)) = args.next().as_deref().and_then(|a| a.split_once(':'))
                    && let (Ok(offset_hz), Ok(width_hz)) = (hz.parse::<f64>(), width.parse::<f64>())
                {
                    adds.push((offset_hz, width_hz));
                }
            }
            "--iq" => {
                net_format = args
                    .next()
                    .and_then(|v| net::Format::parse(&v))
                    .unwrap_or(net_format)
            }
            _ => file = Some(PathBuf::from(a)),
        }
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1500.0, 900.0])
            .with_min_inner_size([1000.0, 600.0])
            .with_title("DecAero")
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "DecAero",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            // No sound on an unattended screenshot run.
            let audio = if shot_path.is_some() {
                None
            } else {
                audio::Output::start().ok()
            };
            let mut app = App {
                engine: None,
                view: View::default(),
                band: band::Band::new(),
                shown_sent: None,
                rows_seen: 0,
                log: VecDeque::new(),
                log_seen: 0,
                selected: None,
                feeds_edit: feeds.clone(),
                feeds,
                feeds_open: false,
                select_hz,
                tab: Tab::Messages,
                center_text,
                follow: true,
                scan: true,
                fast,
                paused: false,
                volume: 0.7,
                audio,
                // An unattended run records only where it was told to.
                recorder: match (&out_dir, shot_path.is_some()) {
                    (Some(dir), _) => Some(recorder::Recorder::new(dir.clone())),
                    (None, false) => {
                        Some(recorder::Recorder::new(recorder::Recorder::default_dir()))
                    }
                    (None, true) => None,
                },
                trail: VecDeque::new(),
                trail_of: None,
                last_scatter: Vec::new(),
                shot: shot_path.map(|path| Shot {
                    path,
                    after,
                    requested: None,
                    frames: (0, 0.0, 0.0, 0),
                }),
                started: Instant::now(),
                error: None,
                aircraft: aircraft::Lookup::new(
                    aircraft_db.unwrap_or_else(aircraft::Lookup::default_path),
                ),
                filter: Filter::All,
                net_url: net_url
                    .clone()
                    .unwrap_or_else(|| "sdrpp://127.0.0.1:5259".to_string()),
                net_rate_text: format!("{}", net_rate / 1e3),
                net_format,
                net_eight_bit: false,
                connecting: None,
                radios: decaero_radio::list(),
                radio_pick: 0,
                radio_rate: 2e6,
                radio_gains: Vec::new(),
                radio_bias: bias_arg,
                radio_control: None,
                radio_arg: radio_key.is_some(),
                live: false,
                prefs: prefs::Prefs::default(),
                prefs_path: None,
                prefs_checked: Instant::now(),
                lower: Lower::Calls,
                activity: activity::Activity::new(),
                activity_panel: activity::Panel::new(),
                activity_path: None,
                activity_always: activity_arg.is_some(),
                file_began: 0.0,
            };
            // Settings of the last run. An unattended screenshot run keeps
            // to its command line: it neither reads nor writes them, unless
            // it is given a file of its own with `--prefs`.
            let given = Given {
                center: !app.center_text.is_empty(),
                net: net_url.is_some() || rate_given,
                feeds: app.feeds != FeedOptions::default(),
                radio: radio_key.is_some() || bias_arg,
            };
            if !no_prefs && (app.shot.is_none() || prefs_arg.is_some()) {
                let path = prefs_arg.clone().unwrap_or_else(prefs::Prefs::default_path);
                app.prefs = prefs::Prefs::load(&path);
                app.prefs_path = Some(path);
            }
            // The activity history lives beside the settings. Given a file
            // of its own it is kept for recordings too.
            app.activity_path = activity_arg.clone().or_else(|| {
                app.prefs_path
                    .as_ref()
                    .map(|p| p.with_file_name("activity.txt"))
            });
            // The HydraSDR (the Airspy driver) is the usual radio here.
            let usual = app
                .radios
                .iter()
                .position(|r| r.key == "airspy")
                .unwrap_or(0);
            app.band.want_view = view_arg;
            app.pick_radio(usual);
            app.recall(&given);
            if let Some(f) = &file {
                app.open(f);
            } else if net_url.is_some() {
                app.connect();
            } else if let Some(key) = &radio_key {
                match app.radios.iter().position(|r| r.key == *key) {
                    Some(i) => {
                        app.pick_radio(i);
                        if rate_given {
                            app.radio_rate = net_rate;
                        }
                        app.start_radio();
                    }
                    None => app.error = Some(format!("no radio called {key}")),
                }
            }
            for (offset_hz, width_hz) in &adds {
                app.send(Command::Add {
                    offset_hz: *offset_hz,
                    width_hz: *width_hz,
                });
            }
            Ok(Box::new(app))
        }),
    )
}
