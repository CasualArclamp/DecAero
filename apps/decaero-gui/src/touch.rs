// made by CasualArclamp
//! The layout for fingers: a phone held sideways (about 830 by 380 points).
//!
//! The band has the screen. A rail of big buttons down the left opens one
//! panel at a time beside it (channels and the decoder, the log, aircraft,
//! calls, the input, the rest); pressing the same button again, or Band,
//! closes it. A
//! row of buttons on the band says what a finger does there (move the view,
//! draw a new decoder, tune) and zooms; two fingers pinch.
//!
//! Nothing here needs a keyboard: a box that takes text opens keys of its
//! own (`Keypad`). A phone's keyboard is not reliably offered to a native
//! window, and the numbers typed here are few.
//!
//! The panels themselves are the desktop's (`lib.rs`), drawn with larger
//! controls (`style`). `--touch --size 832x384` shows this layout on a desktop.

use crate::theme::*;
use crate::{App, band};
use decaero_engine::{Command, net};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Frame, Layout, Margin, RichText, Sense, Stroke, Ui, vec2,
};

/// The panel beside the band.
#[derive(PartialEq, Clone, Copy)]
pub enum Panel {
    Channels,
    Log,
    Aircraft,
    Calls,
    Input,
    More,
}

impl Panel {
    const ALL: [(Panel, &'static str); 6] = [
        (Panel::Channels, "Chan"),
        (Panel::Log, "Log"),
        (Panel::Aircraft, "Planes"),
        (Panel::Calls, "Calls"),
        (Panel::Input, "Input"),
        (Panel::More, "More"),
    ];

    /// By the name on its button, in small letters (`--panel`).
    pub fn parse(name: &str) -> Option<Panel> {
        Self::ALL
            .iter()
            .find(|(_, n)| n.eq_ignore_ascii_case(name))
            .map(|(p, _)| *p)
    }

    /// How wide the panel would like to be, in points.
    fn width(self) -> f32 {
        match self {
            Panel::Channels => 420.0,
            Panel::Log | Panel::Input => 370.0,
            Panel::Aircraft => 390.0,
            Panel::Calls => 420.0,
            Panel::More => 350.0,
        }
    }
}

/// Which box the keys are filling in.
#[derive(PartialEq, Clone, Copy)]
pub enum Target {
    Centre,
    NetUrl,
    NetRate,
    JsonUdp,
    Sbs,
    Station,
}

/// How long frames are taking: the mean, and the longest of the last two
/// seconds or so. A frame that takes long is a sign the phone is short of
/// time (or is slowing itself down to stay cool).
#[derive(Default)]
pub struct Frames {
    mean_ms: f32,
    worst_ms: f32,
    /// The longest frame of the stretch now being measured, and its length.
    worst_now: f32,
    seconds: f32,
}

impl Frames {
    fn note(&mut self, dt: f32) {
        let ms = dt * 1e3;
        self.mean_ms += 0.05 * (ms - self.mean_ms);
        self.worst_now = self.worst_now.max(ms);
        self.seconds += dt;
        if self.seconds >= 2.0 {
            self.worst_ms = self.worst_now;
            (self.worst_now, self.seconds) = (0.0, 0.0);
        }
    }
}

/// The on-screen keys, open over everything while a box is filled in.
pub struct Keypad {
    target: Target,
    text: String,
}

impl Keypad {
    fn title(&self) -> &'static str {
        match self.target {
            Target::Centre => "Centre frequency, MHz",
            Target::NetUrl => "Network input",
            Target::NetRate => "Sample rate, kS/s",
            Target::JsonUdp => "ACARS as JSON over UDP: host:port",
            Target::Sbs => "ADS-C as SBS: listen:PORT or host:port",
            Target::Station => "Station name",
        }
    }

    /// Only digits and a point are wanted.
    fn numeric(&self) -> bool {
        matches!(self.target, Target::Centre | Target::NetRate)
    }
}

/// Larger controls: everything pressable is at least 38 points high (about
/// 6 mm on a phone), sliders and tick boxes are wider, lists are spaced out.
pub fn style(ctx: &egui::Context) {
    ctx.all_styles_mut(|s| {
        s.spacing.interact_size = vec2(44.0, 38.0);
        s.spacing.button_padding = vec2(10.0, 7.0);
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.slider_width = 170.0;
        s.spacing.slider_rail_height = 10.0;
        s.spacing.icon_width = 24.0;
        s.spacing.icon_width_inner = 14.0;
        s.spacing.combo_width = 140.0;
        s.spacing.scroll.bar_width = 12.0;
        // No pointer hovers on a touch screen: do not leave a tip hanging.
        s.interaction.tooltip_delay = 0.6;
        for (style, size) in [
            (egui::TextStyle::Body, 14.0),
            (egui::TextStyle::Button, 14.5),
            (egui::TextStyle::Monospace, 13.0),
        ] {
            if let Some(font) = s.text_styles.get_mut(&style) {
                font.size = size;
            }
        }
    });
}

/// A box of text that is pressed, not typed in: shows `text` (or `hint`
/// dimmed) and says when it was pressed.
fn field(ui: &mut Ui, text: &str, hint: &str, width: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(width, 38.0), Sense::click());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 4, WELL);
    p.rect_stroke(rect, 4, Stroke::new(1.0, GRID), egui::StrokeKind::Inside);
    let (shown, colour) = if text.trim().is_empty() {
        (hint, FAINT)
    } else {
        (text, TEXT)
    };
    p.text(
        rect.left_center() + vec2(8.0, 0.0),
        Align2::LEFT_CENTER,
        shown,
        FontId::monospace(13.0),
        colour,
    );
    response.clicked()
}

/// The version, and when this copy was built where the build script said
/// (`DECAERO_BUILD`, month-day-hour-minute): two APKs of one version are
/// otherwise impossible to tell apart on a phone.
pub fn build_name() -> String {
    match option_env!("DECAERO_BUILD") {
        Some(stamp) => format!("DecAero {} build {stamp}", env!("CARGO_PKG_VERSION")),
        None => format!("DecAero {}", env!("CARGO_PKG_VERSION")),
    }
}

fn heading(ui: &mut Ui, text: &str) {
    ui.add_space(2.0);
    ui.label(RichText::new(text).color(WHITE).size(14.0));
}

fn note(ui: &mut Ui, text: impl Into<String>) {
    ui.label(RichText::new(text).color(DIM).size(11.5));
}

impl App {
    pub(crate) fn ask(&mut self, target: Target) {
        let text = match target {
            Target::Centre => self.center_text.clone(),
            Target::NetUrl => self.net_url.clone(),
            Target::NetRate => self.net_rate_text.clone(),
            Target::JsonUdp => self.feeds_edit.json_udp.clone(),
            Target::Sbs => self.feeds_edit.sbs.clone(),
            Target::Station => self.feeds_edit.station.clone(),
        };
        self.keypad = Some(Keypad { target, text });
    }

    fn answer(&mut self, target: Target, text: String) {
        let text = text.trim().to_string();
        match target {
            Target::Centre => {
                if let Ok(mhz) = text.parse::<f64>() {
                    self.set_centre((mhz * 1e6).round());
                }
            }
            Target::NetUrl => self.net_url = text,
            Target::NetRate => self.net_rate_text = text,
            Target::JsonUdp => self.feeds_edit.json_udp = text,
            Target::Sbs => self.feeds_edit.sbs = text,
            Target::Station => self.feeds_edit.station = text,
        }
    }

    pub(crate) fn ui_touch(&mut self, ui: &mut Ui) {
        // The server's own copy of its controls replaces ours, but not under
        // a slider that is being dragged (as on the desktop).
        if self.view.menu_seq != self.menu_seq && !ui.input(|i| i.pointer.any_down()) {
            self.menu.clone_from(&self.view.menu);
            self.menu_seq = self.view.menu_seq;
        }
        let plain = |fill: Color32, x: i8, y: i8| {
            Frame::new()
                .fill(fill)
                .inner_margin(Margin::symmetric(x, y))
        };
        let screen = ui.available_width();
        egui::Panel::left("touch-rail")
            .frame(plain(PANEL, 4, 4))
            .resizable(false)
            .show_separator_line(false)
            .exact_size(62.0)
            .show(ui, |ui| self.rail(ui));
        egui::Panel::top("touch-top")
            .frame(plain(PANEL, 6, 3))
            .show(ui, |ui| self.touch_bar(ui));
        egui::Panel::bottom("touch-figures")
            .frame(plain(WELL, 6, 1))
            .show_separator_line(false)
            .show(ui, |ui| self.figures(ui));
        if let Some(panel) = self.panel {
            // The band keeps at least 240 points.
            let width = panel.width().min(screen - 62.0 - 240.0).max(220.0);
            egui::Panel::right("touch-side")
                .frame(plain(PANEL, 8, 4))
                .resizable(false)
                .exact_size(width)
                .show(ui, |ui| self.side(ui, panel));
        }
        egui::CentralPanel::default()
            .frame(plain(BG, 0, 0))
            .show(ui, |ui| {
                let area = ui.available_rect_before_wrap();
                self.band_area(ui, area.height());
                self.band_buttons(ui, area);
            });
        self.keys(ui);
    }

    /// One small line along the bottom for seeing what is wrong when
    /// something is: how the drawing keeps up, what the receiver costs,
    /// samples lost on the way, and how the selected carrier is being held.
    fn figures(&mut self, ui: &mut Ui) {
        self.frames.note(ui.input(|i| i.unstable_dt));
        let v = &self.view;
        let decoders: f32 = v.channels.iter().map(|c| c.cpu).sum();
        // The radio's own count of blocks it could not hand on is in its
        // description ("12 blocks lost"); the engine's is `dropped_blocks`.
        let radio_lost = v
            .source
            .split(" · ")
            .find(|part| part.ends_with("blocks lost"))
            .and_then(|part| part.split(' ').next())
            .unwrap_or("0");
        let mut parts = vec![
            (
                format!(
                    "draw {:.0} ms, worst {:.0}",
                    self.frames.mean_ms, self.frames.worst_ms
                ),
                self.frames.worst_ms > 50.0,
            ),
            (
                format!(
                    "CPU front {:.0}% + {} dec {:.0}% of a core",
                    v.load * 100.0,
                    v.channels.iter().filter(|c| !c.closed).count(),
                    decoders * 100.0
                ),
                // The front end is one thread: near a whole core it cannot
                // keep up with the radio.
                v.load > 0.8,
            ),
            (
                format!("lost: radio {radio_lost}, engine {}", v.dropped_blocks),
                radio_lost != "0" || v.dropped_blocks > 0,
            ),
        ];
        if v.source.contains("PLL NOT LOCKED") {
            parts.push(("tuner PLL NOT LOCKED".to_string(), true));
        }
        if let Some(c) = self.selected() {
            parts.push((
                format!(
                    "sel {:+.1} Hz from acquired, clock {:+.0} ppm, coherence {:.2}, {} lock losses",
                    c.carrier_hz - c.acquired_hz,
                    c.clock_ppm,
                    c.coherence,
                    c.lock_losses
                ),
                false,
            ));
        }
        parts.push((decaero_engine::vector_level().to_string(), false));
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            for (text, bad) in parts {
                let colour = if bad { WARN } else { DIM };
                ui.add(egui::Label::new(RichText::new(text).color(colour).size(10.0)).truncate());
            }
        });
    }

    /// The buttons down the left edge.
    fn rail(&mut self, ui: &mut Ui) {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.spacing_mut().button_padding.x = 2.0;
        let count = Panel::ALL.len() as f32 + 1.0;
        let each = ((ui.available_height() - 4.0 * (count - 1.0)) / count).clamp(34.0, 56.0);
        let size = vec2(ui.available_width(), each);
        let label = |text: &str| RichText::new(text).size(12.5);
        // The band alone.
        let alone = self.panel.is_none();
        if ui
            .add_sized(size, egui::Button::new(label("Band")).selected(alone))
            .clicked()
        {
            self.panel = None;
        }
        for (panel, name) in Panel::ALL {
            let on = self.panel == Some(panel);
            if ui
                .add_sized(size, egui::Button::new(label(name)).selected(on))
                .clicked()
            {
                self.panel = if on { None } else { Some(panel) };
            }
        }
    }

    /// The strip along the top: play, the frequency, follow, scan, volume.
    fn touch_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            // A recording can be paused; a live input cannot.
            if self.engine.is_some() && !self.live {
                let running = !self.paused;
                let label = if running { "⏸" } else { "▶" };
                let button = egui::Button::new(RichText::new(label).size(17.0).color(if running {
                    GOOD
                } else {
                    TEXT
                }))
                .fill(if running { GREEN_FILL } else { PANEL2 })
                .min_size(vec2(44.0, 38.0));
                if ui.add(button).clicked() {
                    self.paused = !self.paused;
                    self.send(Command::Pause(self.paused));
                }
            }
            // The centre frequency: press it for the keys.
            let mhz = self.center_hz() / 1e6;
            let text = RichText::new(format!("{mhz:.6}"))
                .monospace()
                .size(19.0)
                .color(WHITE);
            if ui
                .add(egui::Button::new(text).fill(WELL).min_size(vec2(0.0, 38.0)))
                .clicked()
            {
                self.ask(Target::Centre);
            }
            ui.label(RichText::new("MHz").color(DIM).size(11.5));
            if ui.toggle_value(&mut self.follow, "Follow").changed() {
                // As the desktop's: the next drag on the band says where.
                self.follow_pick = self.follow;
                self.follow_range = None;
                self.band.follow_range = None;
                self.send(Command::Follow(false));
                self.send(Command::FollowRange(None));
                if !self.follow {
                    self.band.drag = band::Drag::Pan;
                }
            }
            if ui.toggle_value(&mut self.scan, "Scan").changed() {
                self.send(Command::Scan(self.scan));
            }
            ui.label(RichText::new("🔊").size(15.0).color(DIM));
            ui.spacing_mut().slider_width = 90.0;
            if ui
                .add(egui::Slider::new(&mut self.volume, 0.0..=1.0).show_value(false))
                .changed()
                && let Some(a) = &self.audio
            {
                a.set_volume(self.volume);
            }
            // What is left of the strip: how it is going, or what went wrong.
            let v = &self.view;
            let (text, colour) = if let Some(e) = &self.error {
                (e.clone(), BAD)
            } else if self.connecting.is_some() {
                ("connecting…".to_string(), WARN)
            } else if self.follow_pick {
                (
                    "drag across the band where Follow should look".to_string(),
                    WARN,
                )
            } else if self.engine.is_none() {
                (format!("{} · no input: press Input", build_name()), DIM)
            } else {
                let locked = v.channels.iter().filter(|c| c.locked).count();
                let cpu: f32 = v.channels.iter().map(|c| c.cpu).sum::<f32>() + v.load;
                let lost = if v.dropped_blocks > 0 {
                    format!(" · {} lost", v.dropped_blocks)
                } else {
                    String::new()
                };
                (
                    format!(
                        "{} · {locked} locked · CPU {:.0}%{lost}",
                        if v.paused {
                            "paused"
                        } else if v.ended {
                            "ended"
                        } else {
                            v.source.as_str()
                        },
                        cpu * 100.0
                    ),
                    DIM,
                )
            };
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add(egui::Label::new(RichText::new(text).color(colour).size(11.5)).truncate());
            });
        });
    }

    /// The row of buttons on the band: what a finger does, and the zoom.
    fn band_buttons(&mut self, ui: &mut Ui, area: egui::Rect) {
        // With a panel open the band can be narrower than the row: the
        // buttons are then made narrower to suit, so none is pushed off.
        let fit = ((area.width() - 12.0) / 372.0).clamp(0.6, 1.0);
        let (h, gap) = (36.0, 4.0 * fit);
        // Rust note: a `Cell` lets the closure below and the code around it
        // both move this along without a mutable borrow held between calls.
        let right = std::cell::Cell::new(area.right() - 6.0);
        let top = area.bottom() - h - 6.0;
        let see_through = |c: Color32| c.gamma_multiply(0.82);
        // Laid out from the right, so the list is in reverse.
        let button = |ui: &mut Ui, text: &str, w: f32, on: bool| {
            let w = w * fit;
            let rect = egui::Rect::from_min_size(egui::pos2(right.get() - w, top), vec2(w, h));
            right.set(right.get() - w - gap);
            let fill = see_through(if on { GREEN_FILL } else { PANEL2 });
            let text =
                RichText::new(text)
                    .size(13.0 * fit.max(0.8))
                    .color(if on { GOOD } else { TEXT });
            ui.spacing_mut().button_padding.x = 2.0;
            ui.put(rect, egui::Button::new(text).fill(fill)).clicked()
        };
        // Minus and plus keep their places; All comes and goes to their left.
        if button(ui, "−", 44.0, false) {
            self.band.zoom(2.0);
        }
        if button(ui, "+", 44.0, false) {
            self.band.zoom(0.5);
        }
        if self.band.zoomed() && button(ui, "All", 44.0, false) {
            self.band.zoom_out();
        }
        right.set(right.get() - 8.0);
        let drag = self.band.drag;
        if self.live && button(ui, "Tune", 52.0, drag == band::Drag::Tune) {
            self.band.drag = band::Drag::Tune;
        }
        if button(ui, "+ Decoder", 86.0, drag == band::Drag::Add) {
            // Pressed again: never mind.
            self.band.drag = if drag == band::Drag::Add && !self.follow_pick {
                band::Drag::Pan
            } else {
                band::Drag::Add
            };
        }
        if button(ui, "Move", 54.0, drag == band::Drag::Pan) && !self.follow_pick {
            self.band.drag = band::Drag::Pan;
        }
        // A recording is not tuned.
        if !self.live && self.band.drag == band::Drag::Tune {
            self.band.drag = band::Drag::Pan;
        }
    }

    /// The panel beside the band.
    fn side(&mut self, ui: &mut Ui, panel: Panel) {
        match panel {
            Panel::Channels => self.chan_panel(ui),
            Panel::Log => {
                let height = ui.available_height() - 44.0;
                self.log_panel(ui, height.max(80.0));
            }
            Panel::Aircraft => self.aircraft_panel(ui),
            Panel::Calls => self.lower_panel(ui),
            Panel::Input => {
                egui::ScrollArea::vertical()
                    .id_salt("touch-input")
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.input_panel(ui));
            }
            Panel::More => {
                egui::ScrollArea::vertical()
                    .id_salt("touch-more")
                    .auto_shrink([false, false])
                    .show(ui, |ui| self.more_panel(ui));
            }
        }
    }

    /// Channels and the decoder on a low screen: the constellation, kept
    /// small enough to leave room under it, with the margin bar and the
    /// list of channels at its side; the decoder's figures scroll below.
    fn chan_panel(&mut self, ui: &mut Ui) {
        // A third of the height, and never so wide the list is squeezed out.
        let side = (ui.available_height() * 0.38)
            .min(ui.available_width() - 44.0 - 250.0)
            .clamp(90.0, 170.0);
        ui.horizontal_top(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(side, side), Sense::hover());
            crate::constellation(ui, rect, &self.trail);
            let (bar, _) = ui.allocate_exact_size(vec2(44.0, side), Sense::hover());
            match self.selected().cloned() {
                Some(c) => crate::margin_bar(ui, bar, &c),
                None => {
                    ui.painter().rect_filled(bar, 0, WELL);
                }
            }
            ui.vertical(|ui| {
                ui.set_height(side);
                ui.spacing_mut().button_padding.x = 6.0;
                self.channel_list(ui);
            });
        });
        ui.separator();
        self.decoder_scroll(ui);
    }

    /// Where the signal comes from: a radio on the USB socket, the network,
    /// or a recording.
    fn input_panel(&mut self, ui: &mut Ui) {
        let wide = ui.available_width() - 14.0;
        let busy = self.connecting.is_some();
        if !self.radios.is_empty() {
            heading(ui, "Radio");
            let picked = self
                .radios
                .get(self.radio_pick)
                .map_or(String::new(), |r| r.name.clone());
            let mut pick = self.radio_pick;
            egui::ComboBox::from_id_salt("touch-radio")
                .selected_text(picked)
                .width(wide)
                .show_ui(ui, |ui| {
                    for (i, r) in self.radios.iter().enumerate() {
                        ui.selectable_value(&mut pick, i, &r.name);
                    }
                });
            if pick != self.radio_pick {
                self.pick_radio(pick);
            }
            let info = self.radios.get(self.radio_pick).cloned();
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("touch-rate")
                    .selected_text(format!("{:.3} MS/s", self.radio_rate / 1e6))
                    .width(120.0)
                    .show_ui(ui, |ui| {
                        for r in info.iter().flat_map(|i| i.rates.iter().copied()) {
                            ui.selectable_value(
                                &mut self.radio_rate,
                                r,
                                format!("{:.3} MS/s", r / 1e6),
                            );
                        }
                    });
                // Only for radios that have one, and never on by itself.
                match info.as_ref().and_then(|r| r.bias_tee) {
                    Some(volts) => {
                        ui.checkbox(&mut self.radio_bias, format!("bias-T {volts}"));
                    }
                    None => self.radio_bias = false,
                }
            });
            ui.horizontal(|ui| {
                let running = self.radio_control.is_some() && self.engine.is_some();
                let start = egui::Button::new(if running {
                    "Restart radio"
                } else {
                    "Start radio"
                })
                .min_size(vec2(130.0, 40.0));
                if ui.add_enabled(!busy, start).clicked() {
                    self.start_radio();
                }
                if running && ui.button("Stop").clicked() {
                    self.engine = None;
                    self.radio_control = None;
                    self.live = false;
                    self.view.source = "stopped".into();
                }
            });
            if self.radio_bias {
                note(
                    ui,
                    "Bias-T puts DC on the antenna socket when the radio starts.",
                );
            }
            self.gain_list(ui);
            ui.separator();
        }

        heading(ui, "Network");
        if field(ui, &self.net_url, "sdrpp://host:5259", wide) {
            self.ask(Target::NetUrl);
        }
        ui.horizontal(|ui| {
            if self.net_url.trim().starts_with("sdrpp://") {
                ui.checkbox(&mut self.net_eight_bit, "8 bit");
            } else {
                if field(ui, &self.net_rate_text, "kS/s", 90.0) {
                    self.ask(Target::NetRate);
                }
                note(ui, "kS/s");
                egui::ComboBox::from_id_salt("touch-format")
                    .selected_text(self.net_format.label())
                    .width(84.0)
                    .show_ui(ui, |ui| {
                        for f in [
                            net::Format::I8,
                            net::Format::U8,
                            net::Format::I16,
                            net::Format::F32,
                        ] {
                            ui.selectable_value(&mut self.net_format, f, f.label());
                        }
                    });
            }
            if busy {
                ui.label(RichText::new("connecting…").color(WARN));
            } else if self.net_on {
                if ui.button("Disconnect").clicked() {
                    self.disconnect();
                }
            } else if ui.button("Connect").clicked() {
                self.connect();
            }
        });
        if !self.menu.is_empty() {
            self.server_bar(ui);
        }
        ui.separator();

        heading(ui, "Recording");
        ui.horizontal(|ui| {
            if ui
                .add(egui::Button::new("Open a WAV…").min_size(vec2(130.0, 40.0)))
                .clicked()
            {
                self.ask_file();
            }
            ui.checkbox(&mut self.fast, "fast")
                .on_hover_text("Play the next recording as fast as it decodes");
        });
        note(
            ui,
            "An I/Q or audio recording, as the desktop program plays.",
        );
    }

    /// The picked radio's gains, one under the other; they act at once on a
    /// running radio.
    fn gain_list(&mut self, ui: &mut Ui) {
        let mut changed = Vec::new();
        for g in &mut self.radio_gains {
            ui.horizontal(|ui| {
                if g.max - g.min <= 1.0 && g.step >= 1.0 {
                    // A switch.
                    let mut on = g.value >= 0.5;
                    if ui.checkbox(&mut on, &g.name).changed() {
                        g.value = f64::from(u8::from(on));
                        changed.push((g.name.clone(), g.value));
                    }
                } else {
                    ui.add_sized(
                        vec2(64.0, 20.0),
                        egui::Label::new(RichText::new(&g.name).color(DIM)),
                    );
                    let slider = egui::Slider::new(&mut g.value, g.min..=g.max).step_by(g.step);
                    if ui.add(slider).changed() {
                        changed.push((g.name.clone(), g.value));
                    }
                }
            });
        }
        if let Some(control) = &self.radio_control {
            for (name, value) in changed {
                if let Err(e) = control.set_gain(&name, value) {
                    self.error = Some(e.to_string());
                }
            }
        }
    }

    /// I/Q recording, the feeds out, and how things stand.
    fn more_panel(&mut self, ui: &mut Ui) {
        let wide = ui.available_width() - 14.0;
        heading(ui, "Record I/Q");
        let recording = self.view.iq_recording.is_some();
        ui.horizontal(|ui| {
            let label = if recording { "■ Stop" } else { "● Record" };
            let button = egui::Button::new(label)
                .selected(recording)
                .min_size(vec2(110.0, 40.0));
            if ui.add_enabled(self.engine.is_some(), button).clicked() {
                if recording {
                    self.send(Command::RecordIq(None));
                } else if let Some((lo, hi, _)) = self.band.shown {
                    self.send(Command::RecordIq(Some((self.iq_path(lo, hi), lo, hi))));
                }
            }
            note(ui, "the part of the band on screen");
        });
        ui.separator();

        heading(ui, "Feeds out");
        note(ui, "ACARS as JSON over UDP");
        if field(ui, &self.feeds_edit.json_udp, "host:port, host:port", wide) {
            self.ask(Target::JsonUdp);
        }
        note(ui, "ADS-C positions as SBS");
        if field(
            ui,
            &self.feeds_edit.sbs,
            "listen:30003  or  host:port",
            wide,
        ) {
            self.ask(Target::Sbs);
        }
        note(ui, "Station name");
        if field(ui, &self.feeds_edit.station, "optional", wide) {
            self.ask(Target::Station);
        }
        let changed = self.feeds_edit != self.feeds;
        if ui
            .add_enabled(changed, egui::Button::new("Apply"))
            .clicked()
        {
            self.feeds = self.feeds_edit.clone();
            self.send(Command::Feeds(self.feeds.clone()));
        }
        ui.separator();

        heading(ui, "Status");
        for part in self.status_parts() {
            ui.add(egui::Label::new(RichText::new(part).color(DIM).size(11.5)).wrap());
        }
        note(ui, build_name());
    }

    /// The on-screen keys, while a box is being filled in.
    fn keys(&mut self, ui: &mut Ui) {
        let Some(mut pad) = self.keypad.take() else {
            return;
        };
        // A real keyboard, where there is one, types here too.
        let mut done = false;
        let mut cancel = false;
        ui.input(|i| {
            for e in &i.events {
                match e {
                    egui::Event::Text(t) => pad.text.push_str(t),
                    egui::Event::Key {
                        key, pressed: true, ..
                    } => match key {
                        egui::Key::Backspace => {
                            pad.text.pop();
                        }
                        egui::Key::Enter => done = true,
                        egui::Key::Escape => cancel = true,
                        _ => {}
                    },
                    _ => {}
                }
            }
        });
        let numeric = pad.numeric();
        let mut step = 0.0;
        egui::Window::new("touch-keys")
            .title_bar(false)
            .resizable(false)
            .collapsible(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                Frame::new()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0, FAINT))
                    .corner_radius(6)
                    .inner_margin(Margin::same(8)),
            )
            .show(ui.ctx(), |ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                let key = if numeric {
                    vec2(62.0, 40.0)
                } else {
                    vec2(48.0, 36.0)
                };
                ui.label(RichText::new(pad.title()).color(DIM).size(11.5));
                // What has been typed.
                let width = if numeric {
                    4.0 * (key.x + 4.0) - 4.0
                } else {
                    10.0 * (key.x + 4.0) - 4.0
                };
                let (rect, _) = ui.allocate_exact_size(vec2(width, 34.0), Sense::hover());
                ui.painter().rect_filled(rect, 4, WELL);
                ui.painter().text(
                    rect.left_center() + vec2(8.0, 0.0),
                    Align2::LEFT_CENTER,
                    format!("{}▏", pad.text),
                    FontId::monospace(if numeric { 20.0 } else { 15.0 }),
                    WHITE,
                );
                let row = |ui: &mut Ui, keys: &[&str], text: &mut String| {
                    ui.horizontal(|ui| {
                        for k in keys {
                            let label =
                                RichText::new(*k).size(if k.len() > 1 { 12.0 } else { 16.0 });
                            if ui.add_sized(key, egui::Button::new(label)).clicked() {
                                text.push_str(k);
                            }
                        }
                    });
                };
                if numeric {
                    row(ui, &["7", "8", "9"], &mut pad.text);
                    row(ui, &["4", "5", "6"], &mut pad.text);
                    row(ui, &["1", "2", "3"], &mut pad.text);
                    row(ui, &["0", "."], &mut pad.text);
                } else {
                    row(
                        ui,
                        &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0"],
                        &mut pad.text,
                    );
                    row(
                        ui,
                        &["q", "w", "e", "r", "t", "y", "u", "i", "o", "p"],
                        &mut pad.text,
                    );
                    row(
                        ui,
                        &["a", "s", "d", "f", "g", "h", "j", "k", "l", ":"],
                        &mut pad.text,
                    );
                    row(
                        ui,
                        &["z", "x", "c", "v", "b", "n", "m", ".", ",", "-"],
                        &mut pad.text,
                    );
                    if pad.target == Target::NetUrl {
                        row(
                            ui,
                            &["sdrpp://", "tcp://", "udp://", "listen://", "/", "_"],
                            &mut pad.text,
                        );
                    } else {
                        row(ui, &["listen:", "/", "_", " "], &mut pad.text);
                    }
                }
                // Tuning in steps, without typing: acts at once.
                if pad.target == Target::Centre {
                    ui.horizontal(|ui| {
                        for (label, hz) in [
                            ("−100k", -1e5),
                            ("−10k", -1e4),
                            ("+10k", 1e4),
                            ("+100k", 1e5),
                        ] {
                            if ui
                                .add_sized(key, egui::Button::new(RichText::new(label).size(12.0)))
                                .clicked()
                            {
                                step = hz;
                            }
                        }
                    });
                }
                ui.horizontal(|ui| {
                    let wide = vec2(if numeric { key.x } else { 86.0 }, key.y);
                    if ui.add_sized(wide, egui::Button::new("Del")).clicked() {
                        pad.text.pop();
                    }
                    if ui.add_sized(wide, egui::Button::new("Clear")).clicked() {
                        pad.text.clear();
                    }
                    if ui.add_sized(wide, egui::Button::new("Cancel")).clicked() {
                        cancel = true;
                    }
                    let ok = egui::Button::new(RichText::new("OK").color(GOOD)).fill(GREEN_FILL);
                    if ui.add_sized(wide, ok).clicked() {
                        done = true;
                    }
                });
            });
        if step != 0.0 {
            self.set_centre((self.center_hz() + step).round());
            pad.text.clone_from(&self.center_text);
        }
        if done {
            self.answer(pad.target, pad.text);
        } else if !cancel {
            self.keypad = Some(pad);
        }
    }
}
