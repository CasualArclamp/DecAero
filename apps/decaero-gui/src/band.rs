// made by CasualArclamp
//! The band: spectrum on top, waterfall under it, a mark for every decoder.
//! Laid out and handled as DecSAT's band view (`band_view.rs`): spectrum a
//! third of the height, an axis strip, the waterfall; the wheel zooms about
//! the cursor (with egui's smoothed scroll, so it glides), right- or
//! middle-drag pans. Left-drag draws the box of a new decoder.

use crate::theme::*;
use decaero_engine::View;
use decaero_engine::channel::{ChannelId, Kind};
use eframe::egui::{
    self, Align2, Color32, ColorImage, CursorIcon, FontId, PointerButton, Stroke, TextureHandle,
    TextureOptions, Ui, pos2, vec2,
};
use std::collections::VecDeque;

/// Waterfall texture size: columns across the view, rows of history.
const COLS: usize = 2048;
const ROWS: usize = 300;

pub enum Action {
    None,
    Select(ChannelId),
    /// Start a decoder on the carrier inside this box.
    Add {
        offset_hz: f64,
        width_hz: f64,
    },
}

pub struct Band {
    /// History at full resolution, newest first, each with the noise floor
    /// it was taken at: zooming in redraws from these, so it stays sharp.
    rows: VecDeque<(f32, Vec<f32>)>,
    pixels: Vec<Color32>,
    texture: Option<TextureHandle>,
    palette: Vec<Color32>,
    dirty: bool,
    /// The view, in Hz from the centre of the input.
    lo: f64,
    hi: f64,
    /// The whole band (the input's sample rate).
    span: f64,
    /// Where a left-button drag began (Hz): a new decoder's box is being drawn.
    drag_from: Option<f64>,
}

/// The strongest bin under each of `cols` columns, for the part of `row`
/// between the fractions `from` and `to` of its length.
fn columns(row: &[f32], cols: usize, from: f64, to: f64) -> Vec<f32> {
    let n = row.len() as f64;
    (0..cols)
        .map(|c| {
            let a = ((from + (to - from) * c as f64 / cols as f64) * n) as usize;
            let b = ((from + (to - from) * (c + 1) as f64 / cols as f64) * n).ceil() as usize;
            let a = a.min(row.len() - 1);
            let b = b.clamp(a + 1, row.len());
            row[a..b].iter().copied().fold(f32::MIN, f32::max)
        })
        .collect()
}

impl Band {
    pub fn new() -> Self {
        Self {
            rows: VecDeque::new(),
            pixels: vec![Color32::from_rgb(0, 0, 4); COLS * ROWS],
            texture: None,
            palette: palette(),
            dirty: true,
            lo: -0.5,
            hi: 0.5,
            span: 1.0,
            drag_from: None,
        }
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.span = 1.0;
        self.dirty = true;
    }

    pub fn push_rows(&mut self, rows: impl Iterator<Item = Vec<f32>>, floor_db: f32) {
        for row in rows.filter(|r| !r.is_empty()) {
            self.rows.push_front((floor_db, row));
            self.dirty = true;
        }
        self.rows.truncate(ROWS);
    }

    fn clamp_view(&mut self) {
        let w = (self.hi - self.lo).min(self.span);
        self.lo = self.lo.clamp(-self.span / 2.0, self.span / 2.0 - w);
        self.hi = self.lo + w;
    }

    /// Redraw the waterfall's pixels for the current view.
    fn render(&mut self) {
        let (from, to) = ((self.lo / self.span + 0.5), (self.hi / self.span + 0.5));
        self.pixels.fill(Color32::from_rgb(0, 0, 4));
        for (r, (floor, row)) in self.rows.iter().enumerate() {
            // From a little under the floor to 22 dB over it.
            let (lo, hi) = (floor - 3.0, floor + 22.0);
            for (px, v) in self.pixels[r * COLS..(r + 1) * COLS]
                .iter_mut()
                .zip(columns(row, COLS, from, to))
            {
                let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
                *px = self.palette[(t * 255.0) as usize];
            }
        }
    }

    pub fn show(
        &mut self,
        ui: &Ui,
        rect: egui::Rect,
        response: &egui::Response,
        view: &View,
        selected: Option<ChannelId>,
    ) -> Action {
        // A new input (or the first one): show all of it.
        let rate = view.rate.max(1.0);
        if (self.span - rate).abs() > 1e-6 {
            self.span = rate;
            // A real input shows its upper half: the lower is its mirror.
            self.lo = if view.real { 0.0 } else { -rate / 2.0 };
            self.hi = rate / 2.0;
            self.dirty = true;
        }
        let (lo, hi) = (self.lo, self.hi);
        let hz_at =
            |x: f32, lo: f64, hi: f64| lo + f64::from((x - rect.left()) / rect.width()) * (hi - lo);

        // Zoom about the cursor (DecSAT's: the smoothed wheel, so it glides).
        // It stops at 32 bins across the view: past that the waterfall is
        // all blocks.
        let bins = view.spectrum_db.len().max(64) as f64;
        let min_view = (rate / bins * 32.0).max(1.0);
        if let Some(p) = response.hover_pos() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let f = (-f64::from(scroll) * 0.002).exp();
                let c = hz_at(p.x, lo, hi);
                let new_w = ((hi - lo) * f).clamp(min_view, self.span);
                let frac = (c - lo) / (hi - lo);
                self.lo = c - frac * new_w;
                self.hi = self.lo + new_w;
                self.clamp_view();
                self.dirty = true;
            }
        }
        // Pan: the spectrum follows the mouse.
        if response.dragged_by(PointerButton::Secondary)
            || response.dragged_by(PointerButton::Middle)
        {
            let d = f64::from(response.drag_delta().x / rect.width()) * (hi - lo);
            self.lo -= d;
            self.hi -= d;
            self.clamp_view();
            self.dirty = true;
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
        // The view may have just moved: draw with the new one.
        let (lo, hi) = (self.lo, self.hi);
        let x_of = |hz: f64| rect.left() + ((hz - lo) / (hi - lo)) as f32 * rect.width();
        let (from, to) = (lo / rate + 0.5, hi / rate + 0.5);

        let painter = ui.painter_at(rect);
        let spec_h = (rect.height() * 0.34).clamp(90.0, 320.0);
        let spec = egui::Rect::from_min_size(rect.min, vec2(rect.width(), spec_h));
        let axis = egui::Rect::from_min_size(spec.left_bottom(), vec2(rect.width(), 18.0));
        let wf = egui::Rect::from_min_max(axis.left_bottom(), rect.max);
        painter.rect_filled(spec, 0, WELL);
        painter.rect_filled(axis, 0, AXIS);
        painter.rect_filled(wf, 0, Color32::from_rgb(0, 0, 4));

        if self.dirty {
            self.render();
            let image = ColorImage::new([COLS, ROWS], self.pixels.clone());
            match &mut self.texture {
                Some(t) => t.set(image, TextureOptions::LINEAR),
                None => {
                    self.texture = Some(ui.ctx().load_texture(
                        "waterfall",
                        image,
                        TextureOptions::LINEAR,
                    ))
                }
            }
            self.dirty = false;
        }
        if let Some(t) = &self.texture {
            painter.image(
                t.id(),
                wf,
                egui::Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        // Spectrum trace: floor - 5 dB at the bottom, 30 dB of range.
        let font = FontId::proportional(10.0);
        if !view.spectrum_db.is_empty() {
            let (bottom, top) = (view.floor_db - 5.0, view.floor_db + 25.0);
            let y_of = |db: f32| {
                spec.bottom() - ((db - bottom) / (top - bottom)).clamp(0.0, 1.0) * spec.height()
            };
            for step in [0.0f32, 10.0, 20.0] {
                let y = y_of(view.floor_db + step);
                painter.line_segment(
                    [pos2(spec.left(), y), pos2(spec.right(), y)],
                    Stroke::new(1.0, GRID),
                );
                let label = format!("{:.0} dB", view.floor_db + step);
                painter.text(
                    pos2(spec.left() + 4.0, y - 1.0),
                    Align2::LEFT_BOTTOM,
                    label,
                    font.clone(),
                    FAINT,
                );
            }
            let n = (spec.width() as usize).clamp(16, 4096);
            let points: Vec<egui::Pos2> = columns(&view.spectrum_db, n, from, to)
                .into_iter()
                .enumerate()
                .map(|(i, db)| {
                    pos2(
                        spec.left() + (i as f32 + 0.5) * spec.width() / n as f32,
                        y_of(db),
                    )
                })
                .collect();
            let fill = Color32::from_rgba_unmultiplied(70, 150, 255, 36);
            let mut mesh = egui::Mesh::default();
            for p in &points {
                mesh.colored_vertex(*p, fill);
                mesh.colored_vertex(pos2(p.x, spec.bottom()), fill);
            }
            for i in 0..points.len().saturating_sub(1) as u32 {
                mesh.add_triangle(2 * i, 2 * i + 1, 2 * i + 2);
                mesh.add_triangle(2 * i + 1, 2 * i + 3, 2 * i + 2);
            }
            painter.add(egui::Shape::mesh(mesh));
            painter.add(egui::Shape::line(points, Stroke::new(1.2, TRACE)));
        }

        // Frequency axis: about eight labels across the view.
        let width_hz = hi - lo;
        let step = [
            100.0, 200.0, 500.0, 1e3, 2e3, 5e3, 1e4, 2e4, 5e4, 1e5, 2e5, 5e5, 1e6,
        ]
        .into_iter()
        .find(|s| width_hz / s <= 9.0)
        .unwrap_or(1e6);
        let mut hz = (lo / step).ceil() * step;
        while hz < hi {
            let x = x_of(hz);
            painter.line_segment(
                [pos2(x, axis.top()), pos2(x, axis.top() + 4.0)],
                Stroke::new(1.0, FAINT),
            );
            let label = if view.center_hz > 0.0 {
                let decimals = if step < 1e3 { 4 } else { 3 };
                format!("{:.*}", decimals, (view.center_hz + hz) / 1e6)
            } else if step < 1e3 {
                format!("{:+.1}k", hz / 1e3)
            } else {
                format!("{:+.0}k", hz / 1e3)
            };
            if x > axis.left() + 28.0 && x < axis.right() - 28.0 {
                painter.text(
                    pos2(x, axis.center().y + 2.0),
                    Align2::CENTER_CENTER,
                    label,
                    font.clone(),
                    DIM,
                );
            }
            hz += step;
        }

        // A mark per decoder.
        let mut labels: Vec<(f32, String, Color32)> = Vec::new();
        for c in &view.channels {
            let width = match c.kind {
                Kind::P(600) | Kind::Rt(600) => 900.0,
                Kind::P(1200) | Kind::Rt(1200) => 1700.0,
                Kind::Searching | Kind::NotAero => 1500.0,
                _ => 7000.0,
            };
            if c.carrier_hz + width / 2.0 < lo || c.carrier_hz - width / 2.0 > hi {
                continue;
            }
            let colour = match (c.kind, c.locked) {
                _ if c.closed => FAINT,
                (Kind::NotAero | Kind::Ended, _) => FAINT,
                (Kind::Rt(_), _) => PINK,
                (Kind::Searching, _) | (_, false) => ORANGE,
                (Kind::C, true) if view.listening == Some(c.id) => GOOD,
                (Kind::C, true) => VFO,
                (Kind::P(_), true) => AMBER,
            };
            let a = x_of(c.carrier_hz - width / 2.0);
            let b = x_of(c.carrier_hz + width / 2.0).max(a + 3.0);
            let on = selected == Some(c.id);
            // Edges on the spectrum only, a tint over the waterfall: a narrow
            // carrier must stay visible under its mark.
            let over_spec = egui::Rect::from_min_max(pos2(a, spec.top()), pos2(b, axis.bottom()));
            let over_wf =
                egui::Rect::from_min_max(pos2(a - 2.0, wf.top()), pos2(b + 2.0, wf.bottom()));
            painter.rect_filled(
                over_spec,
                0,
                colour.gamma_multiply(if on { 0.22 } else { 0.10 }),
            );
            if on {
                painter.rect_filled(over_wf, 0, colour.gamma_multiply(0.12));
            }
            let edge = Stroke::new(
                if on { 1.6 } else { 1.0 },
                colour.gamma_multiply(if on { 1.0 } else { 0.7 }),
            );
            painter.line_segment([over_spec.left_top(), over_spec.left_bottom()], edge);
            painter.line_segment([over_spec.right_top(), over_spec.right_bottom()], edge);
            painter.rect_filled(
                egui::Rect::from_min_max(pos2(a, axis.top()), pos2(b, axis.top() + 3.0)),
                0,
                colour,
            );
            if on || c.kind == Kind::C || b - a > 26.0 {
                let text = match c.kind {
                    Kind::C => c.aes.map_or("C".to_string(), |a| format!("C {a:06X}")),
                    Kind::P(r) => format!(
                        "P{}",
                        if r >= 10_000 {
                            "10k".to_string()
                        } else {
                            r.to_string()
                        }
                    ),
                    Kind::Rt(r) => format!(
                        "R/T {}",
                        if r >= 10_000 {
                            "10k".to_string()
                        } else {
                            r.to_string()
                        }
                    ),
                    k => k.label(),
                };
                labels.push(((a + b) / 2.0, text, colour));
            }
        }
        // Labels on two lines so neighbours do not sit on each other.
        let mut last_right = [f32::MIN; 2];
        for (x, text, colour) in labels {
            let galley = painter.layout_no_wrap(text, font.clone(), Color32::from_rgb(6, 20, 28));
            let w = galley.size().x + 8.0;
            let line = if x - w / 2.0 > last_right[0] { 0 } else { 1 };
            if x - w / 2.0 <= last_right[line] {
                continue;
            }
            last_right[line] = x + w / 2.0 + 2.0;
            let at = egui::Rect::from_min_size(
                pos2(x - w / 2.0, spec.top() + 2.0 + 15.0 * line as f32),
                vec2(w, 13.0),
            );
            painter.rect_filled(at, 2, colour);
            painter.galley(pos2(at.left() + 4.0, at.top()), galley, Color32::BLACK);
        }
        // Left-drag draws the box of a new decoder (as dropping a VFO in
        // DecSAT); letting go starts it, and it names the carrier itself.
        if response.drag_started_by(PointerButton::Primary) {
            let origin = ui
                .input(|i| i.pointer.press_origin())
                .unwrap_or(rect.center());
            self.drag_from = Some(hz_at(origin.x, lo, hi));
        }
        let mut made = None;
        if let Some(from) = self.drag_from {
            let now = ui
                .input(|i| i.pointer.latest_pos())
                .map_or(from, |p| hz_at(p.x, lo, hi));
            let (a, b) = (from.min(now), from.max(now));
            if response.dragged_by(PointerButton::Primary) {
                let box_ = egui::Rect::from_min_max(
                    pos2(x_of(a), spec.top()),
                    pos2(x_of(b), rect.bottom()),
                );
                painter.rect_filled(box_, 0, VFO.gamma_multiply(0.15));
                painter.line_segment([box_.left_top(), box_.left_bottom()], Stroke::new(1.0, VFO));
                painter.line_segment(
                    [box_.right_top(), box_.right_bottom()],
                    Stroke::new(1.0, VFO),
                );
                let label = format!("new decoder · {:.1} kHz", (b - a) / 1e3);
                painter.text(
                    box_.center_top() + vec2(0.0, 34.0),
                    Align2::CENTER_TOP,
                    label,
                    font.clone(),
                    WHITE,
                );
            } else {
                // Released. A drag of a few pixels was a click that slipped.
                self.drag_from = None;
                if x_of(b) - x_of(a) >= 6.0 {
                    made = Some(Action::Add {
                        offset_hz: (a + b) / 2.0,
                        width_hz: b - a,
                    });
                }
            }
        }
        if let Some(action) = made {
            return action;
        }
        // How far in the view is, when it is not the whole band.
        if width_hz < self.span * 0.999 {
            let text = format!(
                "{:.1} kHz across · wheel zooms, right-drag pans",
                width_hz / 1e3
            );
            painter.text(
                pos2(spec.right() - 6.0, spec.bottom() - 3.0),
                Align2::RIGHT_BOTTOM,
                text,
                font,
                DIM,
            );
        }

        // Clicks.
        if let Some(p) = response.interact_pointer_pos() {
            let hz = hz_at(p.x, lo, hi);
            if response.double_clicked() {
                // A box of the usual width; shift for a wide carrier.
                let width_hz = if ui.input(|i| i.modifiers.shift) {
                    9000.0
                } else {
                    1500.0
                };
                return Action::Add {
                    offset_hz: hz,
                    width_hz,
                };
            }
            if response.clicked() {
                // Within eight pixels of the carrier, or inside a wide one's mark.
                let reach = 8.0 / f64::from(rect.width()) * width_hz;
                if let Some(c) = view
                    .channels
                    .iter()
                    .min_by(|a, b| {
                        (a.carrier_hz - hz)
                            .abs()
                            .total_cmp(&(b.carrier_hz - hz).abs())
                    })
                    .filter(|c| {
                        (c.carrier_hz - hz).abs()
                            < reach + if c.kind == Kind::C { 3500.0 } else { 600.0 }
                    })
                {
                    return Action::Select(c.id);
                }
            }
        }
        Action::None
    }
}
