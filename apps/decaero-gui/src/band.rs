// made by CasualArclamp
//! The band: spectrum on top, waterfall under it, a mark for every decoder.
//! Laid out as DecSAT's band view (spectrum a third of the height, an axis
//! strip, the waterfall), drawn by hand.

use crate::theme::*;
use decaero_engine::View;
use decaero_engine::channel::{ChannelId, Kind};
use eframe::egui::{
    self, Align2, Color32, ColorImage, FontId, Stroke, TextureHandle, TextureOptions, Ui, pos2,
    vec2,
};

/// Waterfall texture size: columns across the band, rows of history.
const COLS: usize = 2048;
const ROWS: usize = 300;

pub enum Action {
    None,
    Select(ChannelId),
    Add { offset_hz: f64, wide: bool },
}

pub struct Band {
    pixels: Vec<Color32>,
    texture: Option<TextureHandle>,
    palette: Vec<Color32>,
    dirty: bool,
}

/// The strongest bin under each of `cols` columns.
fn columns(row: &[f32], cols: usize) -> Vec<f32> {
    (0..cols)
        .map(|c| {
            let a = c * row.len() / cols;
            let b = ((c + 1) * row.len() / cols).max(a + 1).min(row.len());
            row[a..b].iter().copied().fold(f32::MIN, f32::max)
        })
        .collect()
}

impl Band {
    pub fn new() -> Self {
        Self {
            pixels: vec![Color32::from_rgb(0, 0, 4); COLS * ROWS],
            texture: None,
            palette: palette(),
            dirty: true,
        }
    }

    pub fn clear(&mut self) {
        self.pixels.fill(Color32::from_rgb(0, 0, 4));
        self.dirty = true;
    }

    /// New rows go in at the top; older ones move down.
    pub fn push_rows(&mut self, rows: impl Iterator<Item = Vec<f32>>, floor_db: f32) {
        // From a little under the floor to 22 dB over it.
        let (lo, hi) = (floor_db - 3.0, floor_db + 22.0);
        for row in rows {
            if row.is_empty() {
                continue;
            }
            self.pixels.copy_within(..COLS * (ROWS - 1), COLS);
            for (px, v) in self.pixels[..COLS].iter_mut().zip(columns(&row, COLS)) {
                let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
                *px = self.palette[(t * 255.0) as usize];
            }
            self.dirty = true;
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
        let painter = ui.painter_at(rect);
        let spec_h = (rect.height() * 0.34).clamp(90.0, 320.0);
        let spec = egui::Rect::from_min_size(rect.min, vec2(rect.width(), spec_h));
        let axis = egui::Rect::from_min_size(spec.left_bottom(), vec2(rect.width(), 18.0));
        let wf = egui::Rect::from_min_max(axis.left_bottom(), rect.max);
        painter.rect_filled(spec, 0, WELL);
        painter.rect_filled(axis, 0, AXIS);
        painter.rect_filled(wf, 0, Color32::from_rgb(0, 0, 4));

        if self.dirty {
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
        let rate = view.rate.max(1.0);
        let x_of = |hz: f64| rect.left() + ((hz / rate + 0.5) as f32) * rect.width();
        let hz_of = |x: f32| (f64::from((x - rect.left()) / rect.width()) - 0.5) * rate;

        // Spectrum trace: floor - 5 dB at the bottom, 30 dB of range.
        let font = FontId::proportional(10.0);
        if !view.spectrum_db.is_empty() {
            let (lo, hi) = (view.floor_db - 5.0, view.floor_db + 25.0);
            let y_of =
                |db: f32| spec.bottom() - ((db - lo) / (hi - lo)).clamp(0.0, 1.0) * spec.height();
            for step in [0.0f32, 10.0, 20.0] {
                let y = y_of(view.floor_db + step);
                painter.line_segment(
                    [pos2(spec.left(), y), pos2(spec.right(), y)],
                    Stroke::new(1.0, GRID),
                );
                painter.text(
                    pos2(spec.left() + 4.0, y - 1.0),
                    Align2::LEFT_BOTTOM,
                    format!("{:.0} dB", view.floor_db + step),
                    font.clone(),
                    FAINT,
                );
            }
            let n = (spec.width() as usize).clamp(16, 4096);
            let points: Vec<egui::Pos2> = columns(&view.spectrum_db, n)
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

        // Frequency axis: about eight labels.
        let step = [1e3, 2e3, 5e3, 1e4, 2e4, 5e4, 1e5, 2e5, 5e5, 1e6]
            .into_iter()
            .find(|s| rate / s <= 9.0)
            .unwrap_or(1e6);
        let mut hz = (-rate / 2.0 / step).ceil() * step;
        while hz < rate / 2.0 {
            let x = x_of(hz);
            painter.line_segment(
                [pos2(x, axis.top()), pos2(x, axis.top() + 4.0)],
                Stroke::new(1.0, FAINT),
            );
            let label = if view.center_hz > 0.0 {
                format!("{:.3}", (view.center_hz + hz) / 1e6)
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
                Kind::P(600) => 900.0,
                Kind::P(1200) => 1700.0,
                Kind::Searching | Kind::NotAero => 1500.0,
                _ => 7000.0,
            };
            let colour = match (c.kind, c.locked) {
                _ if c.closed => FAINT,
                (Kind::NotAero | Kind::Ended, _) => FAINT,
                (Kind::Searching, _) | (_, false) => ORANGE,
                (Kind::C, true) if view.listening == Some(c.id) => GOOD,
                (Kind::C, true) => VFO,
                (Kind::P(_), true) => AMBER,
            };
            let (a, b) = (
                x_of(c.carrier_hz - width / 2.0),
                x_of(c.carrier_hz + width / 2.0).max(x_of(c.carrier_hz - width / 2.0) + 3.0),
            );
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

        // Clicks.
        if let Some(p) = response.interact_pointer_pos() {
            let hz = hz_of(p.x);
            if response.double_clicked() {
                return Action::Add {
                    offset_hz: hz,
                    wide: ui.input(|i| i.modifiers.shift),
                };
            }
            if response.clicked() {
                let reach = f64::from(8.0 / rect.width()) * rate + 4000.0;
                if let Some(c) = view
                    .channels
                    .iter()
                    .min_by(|a, b| {
                        (a.carrier_hz - hz)
                            .abs()
                            .total_cmp(&(b.carrier_hz - hz).abs())
                    })
                    .filter(|c| (c.carrier_hz - hz).abs() < reach)
                {
                    return Action::Select(c.id);
                }
            }
        }
        Action::None
    }
}
