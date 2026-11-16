// made by CasualArclamp
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! DecAero's window: three columns in DecSAT's colours.
//!
//! Left: the channels and, for the selected one, the constellation with a
//! margin bar and every figure of its decoder. Middle: the band (spectrum and
//! waterfall with a mark per decoder), the calls and the channel activity.
//! Right: messages and the aircraft seen.
//!
//! `decaero-gui [FILE.wav] [--center MHZ] [--fast] [--screenshot OUT.png --after SECS] [--select HZ] [--out-dir DIR]`

mod aircraft;
mod audio;
mod band;
mod recorder;
mod theme;

use decaero_engine::channel::{ChannelId, ChannelStatus, Kind};
use decaero_engine::{Command, Engine, FileSource, LogLine, Options, View};
use eframe::egui::{
    self, Align2, Color32, FontId, Frame, Margin, RichText, Sense, Stroke, Ui, pos2, vec2,
};
use num_complex::Complex32;
use std::collections::VecDeque;
use std::path::PathBuf;
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

/// Which aircraft to list.
#[derive(PartialEq, Clone, Copy)]
enum Filter {
    All,
    Us,
    Military,
}

struct Shot {
    path: PathBuf,
    after: f64,
    requested: Option<Instant>,
}

struct App {
    engine: Option<Engine>,
    view: View,
    band: band::Band,
    rows_seen: u64,
    log: VecDeque<LogLine>,
    log_seen: u64,
    selected: Option<ChannelId>,
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
    /// Which aircraft the right-hand table shows.
    filter: Filter,
}

impl App {
    fn open(&mut self, path: &std::path::Path) {
        match FileSource::open(path, true) {
            Ok(source) => {
                let center_hz = self
                    .center_text
                    .trim()
                    .parse::<f64>()
                    .map(|m| m * 1e6)
                    .unwrap_or(0.0);
                let engine = Engine::start(
                    Box::new(source),
                    Options {
                        realtime: !self.fast,
                        center_hz,
                        follow: self.follow,
                        ..Options::default()
                    },
                );
                if let Some(a) = &mut self.audio {
                    a.attach(engine.audio_tap());
                }
                self.engine = Some(engine);
                self.band.clear();
                self.rows_seen = 0;
                self.log.clear();
                self.log_seen = 0;
                self.selected = None;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{}: {e}", path.display())),
        }
    }

    fn send(&self, c: Command) {
        if let Some(e) = &self.engine {
            e.send(c);
        }
    }

    fn pull(&mut self) {
        let Some(engine) = &self.engine else { return };
        self.view = engine.view();
        let rows = engine.rows_after(self.rows_seen);
        if let Some((seq, _)) = rows.last() {
            self.rows_seen = *seq;
        }
        self.band
            .push_rows(rows.into_iter().map(|(_, r)| r), self.view.floor_db);
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
            let ended: Vec<_> = self
                .view
                .calls
                .iter()
                .filter(|c| c.end_s.is_some())
                .map(|c| (c.channel, c.aes))
                .collect();
            rec.tidy(&ended);
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
                Ok(()) => eprintln!("screenshot saved to {}", shot.path.display()),
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
                format!("{} blocks lost", v.dropped_blocks),
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
        egui::ScrollArea::vertical()
            .id_salt("channels")
            .max_height(230.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                egui::Grid::new("channel-table")
                    .num_columns(5)
                    .spacing([8.0, 3.0])
                    .striped(true)
                    .show(ui, |ui| {
                        for h in ["", "kHz", "mode", "Eb/N0", "FEC"] {
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
                            if c.locked {
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
                            ui.end_row();
                        }
                    });
            });
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
            if c.locked {
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
        if ui.button("Remove decoder").clicked() {
            self.send(Command::Remove(c.id));
        }
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
            band::Action::Add { offset_hz, wide } => self.send(Command::Add { offset_hz, wide }),
            band::Action::None => {}
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Calls").color(WHITE).size(14.0));
            ui.label(
                RichText::new("click a carrier to select it · double-click the band to add a decoder (shift: a wide one)")
                    .color(DIM)
                    .size(11.0),
            );
        });
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

fn tag(c: &ChannelStatus) -> (&'static str, Color32) {
    match (c.kind, c.locked) {
        (Kind::NotAero, _) => ("no", DIM),
        _ if c.closed => ("end", DIM),
        (Kind::Ended, _) => ("end", DIM),
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
    if c.locked {
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
        egui::Panel::top("top")
            .frame(plain(PANEL))
            .show(ui, |ui| self.top_bar(ui));
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
    let mut out_dir = None;
    let mut aircraft_db = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--center" => center_text = args.next().unwrap_or_default(),
            "--fast" => fast = true,
            "--screenshot" => shot_path = args.next().map(PathBuf::from),
            "--after" => after = args.next().and_then(|v| v.parse().ok()).unwrap_or(8.0),
            "--select" => select_hz = args.next().and_then(|v| v.parse().ok()),
            "--out-dir" => out_dir = args.next().map(PathBuf::from),
            "--aircraft-db" => aircraft_db = args.next().map(PathBuf::from),
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
                rows_seen: 0,
                log: VecDeque::new(),
                log_seen: 0,
                selected: None,
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
                }),
                started: Instant::now(),
                error: None,
                aircraft: aircraft::Lookup::new(
                    aircraft_db.unwrap_or_else(aircraft::Lookup::default_path),
                ),
                filter: Filter::All,
            };
            if let Some(f) = &file {
                app.open(f);
            }
            Ok(Box::new(app))
        }),
    )
}
