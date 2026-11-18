// made by CasualArclamp
//! The band: spectrum on top, waterfall under it, a mark for every decoder.
//! Laid out and handled as DecSAT's band view (`band_view.rs`): spectrum a
//! third of the height, an axis strip, the waterfall; the wheel zooms about
//! the cursor (with egui's smoothed scroll, so it glides), right- or
//! middle-drag pans. Left-drag draws the box of a new decoder.

use crate::theme::*;
use decaero_engine::channel::{ChannelId, Kind};
use decaero_engine::zoom::ZoomRow;
use decaero_engine::{Row, View};
use eframe::egui::{
    self, Align2, Color32, ColorImage, CursorIcon, FontId, PointerButton, Stroke, TextureHandle,
    TextureOptions, TextureWrapMode, Ui, pos2, vec2,
};
use std::collections::VecDeque;

/// Rows of history kept, which is also the height of the texture: a ring,
/// as DecSAT's (`waterfall.rs`), so a new row is one small upload and the
/// picture is moved by shifting the texture coordinates.
const RING: usize = 1024;
/// The waterfall has at most this many rows on screen; on a taller one each
/// row is two or more screen pixels high (always a whole number).
const MAX_ROWS: usize = 900;
/// Rows that may be waiting above the top edge to slide in.
const WAITING: usize = 16;
/// Unfiltered (a texel is a whole number of screen pixels), repeating
/// vertically for the ring's wrap-round.
const OPTIONS: TextureOptions = TextureOptions {
    wrap_mode: TextureWrapMode::Repeat,
    ..TextureOptions::NEAREST
};
/// Behind the waterfall.
const DARK: Color32 = Color32::from_rgb(0, 0, 4);

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
    rows: VecDeque<(f32, Row, f64)>,
    /// Columns of the texture (one a screen pixel, so nothing is resampled)
    /// and rows on screen.
    size: [usize; 2],
    /// Screen pixels to a row.
    thick: usize,
    texture: Option<TextureHandle>,
    /// The texture row holding the newest row, and how many rows have come
    /// since the texture was last brought up to date.
    head: usize,
    fresh: usize,
    /// Rows arrive in clumps (three with every block of samples), so they
    /// are let in at a steady pace instead: `waiting` rows (and a fraction)
    /// are still above the top edge, sliding in at `pace` rows a second.
    waiting: f32,
    pace: f32,
    /// For measuring the pace: rows and seconds since it was last worked out.
    counted: (f32, f32),
    palette: Vec<Color32>,
    dirty: bool,
    /// The view, in Hz from the centre of the input.
    lo: f64,
    hi: f64,
    /// The whole band (the input's sample rate).
    span: f64,
    /// Where a left-button drag began (Hz): a new decoder's box is being drawn.
    drag_from: Option<f64>,
    /// A view asked for on the command line, taken once the input is known.
    pub want_view: Option<(f64, f64)>,
    /// What is on screen (Hz from the centre, and screen pixels across),
    /// for the engine: it makes a finer spectrum of it when zoomed in.
    pub shown: Option<(f64, f64, usize)>,
    /// Where a live input is tuned, Hz (0 for a recording). Each row keeps
    /// the tuning it was taken at, so after a retune the history is drawn
    /// moved along to where its frequencies now are.
    pub tuned: f64,
    drawn_tuned: f64,
    /// Hz to move the tuning by, from a middle-button drag: for the caller
    /// to take and act on.
    pub tune_by: f64,
}

/// How one column of the picture is read from a row of bins.
#[derive(Clone, Copy)]
enum Column {
    /// Several bins under the column: the strongest (a narrow carrier must
    /// not fall between columns).
    Peak(usize, usize),
    /// Zoomed in past one bin a column: between two bins, by this weight
    /// (as DecSAT's waterfall, which the graphics card interpolates).
    Between(usize, usize, f32),
}

/// The columns for `cols` across the part of a row of `n` bins between the
/// fractions `from` and `to` of its length.
fn plan(n: usize, cols: usize, from: f64, to: f64) -> Vec<Column> {
    (0..cols).map(|c| column(n, cols, from, to, c)).collect()
}

/// Column `c` of `plan`.
fn column(n: usize, cols: usize, from: f64, to: f64, c: usize) -> Column {
    let per_col = (to - from) * n as f64 / cols as f64;
    if per_col < 1.0 {
        // The column's middle, counted in bins from the first bin's middle.
        let at = ((from * n as f64 + (c as f64 + 0.5) * per_col) - 0.5).clamp(0.0, (n - 1) as f64);
        let a = at as usize;
        Column::Between(a, (a + 1).min(n - 1), (at - a as f64) as f32)
    } else {
        let a = ((from * n as f64 + c as f64 * per_col).max(0.0) as usize).min(n - 1);
        let b = (from * n as f64 + (c + 1) as f64 * per_col).ceil().max(0.0) as usize;
        Column::Peak(a, b.clamp(a + 1, n))
    }
}

/// The columns of the view `lo..hi` (Hz) that a finer row covers, and how
/// each is read from it; `None` where the view runs past the row.
fn overlay(zoom: &ZoomRow, cols: usize, lo: f64, hi: f64) -> Vec<Option<Column>> {
    let n = zoom.db.len();
    // The row as a stretch of bins, each `step_hz` wide about its own middle.
    let left = zoom.lo_hz - zoom.step_hz / 2.0;
    let width = zoom.step_hz * n as f64;
    let (from, to) = ((lo - left) / width, (hi - left) / width);
    (0..cols)
        .map(|c| {
            let middle = from + (to - from) * (c as f64 + 0.5) / cols as f64;
            (n > 1 && (0.0..1.0).contains(&middle)).then(|| column(n, cols, from, to, c))
        })
        .collect()
}

fn read(row: &[f32], column: Column) -> f32 {
    match column {
        Column::Peak(a, b) => row[a..b].iter().copied().fold(f32::MIN, f32::max),
        Column::Between(a, b, w) => row[a] + (row[b] - row[a]) * w,
    }
}

/// `cols` values across part of a row (see `plan`).
fn columns(row: &[f32], cols: usize, from: f64, to: f64) -> Vec<f32> {
    plan(row.len(), cols, from, to)
        .into_iter()
        .map(|c| read(row, c))
        .collect()
}

/// To the nearest screen pixel edge.
fn snap(v: f32, ppp: f32) -> f32 {
    (v * ppp).round() / ppp
}

/// To the middle of a screen pixel: where a one-pixel line is sharp.
fn mid(v: f32, ppp: f32) -> f32 {
    ((v * ppp).floor() + 0.5) / ppp
}

impl Band {
    pub fn new() -> Self {
        Self {
            rows: VecDeque::new(),
            size: [0, 0],
            thick: 1,
            head: 0,
            fresh: 0,
            waiting: 0.0,
            pace: 60.0,
            counted: (0.0, 0.0),
            texture: None,
            palette: palette(),
            dirty: true,
            lo: -0.5,
            hi: 0.5,
            span: 1.0,
            drag_from: None,
            want_view: None,
            shown: None,
            tuned: 0.0,
            drawn_tuned: 0.0,
            tune_by: 0.0,
        }
    }

    pub fn clear(&mut self) {
        self.rows.clear();
        self.span = 1.0;
        self.dirty = true;
    }

    /// New rows, taken with the input tuned to `tuned_hz` (0 for a recording).
    pub fn push_rows(&mut self, rows: impl Iterator<Item = Row>, floor_db: f32, tuned_hz: f64) {
        for row in rows.filter(|r| !r.db.is_empty()) {
            self.rows.push_front((floor_db, row, tuned_hz));
            self.fresh += 1;
            self.waiting += 1.0;
            self.counted.0 += 1.0;
        }
        self.rows.truncate(RING);
    }

    fn clamp_view(&mut self) {
        let w = (self.hi - self.lo).min(self.span);
        self.lo = self.lo.clamp(-self.span / 2.0, self.span / 2.0 - w);
        self.hi = self.lo + w;
    }

    /// The colours of `count` rows, newest first, for the current view.
    fn render(&self, count: usize) -> Vec<Color32> {
        let cols = self.size[0];
        let (from, to) = ((self.lo / self.span + 0.5), (self.hi / self.span + 0.5));
        let mut pixels = vec![DARK; cols * count];
        // Rows are all one length, so the columns are worked out once.
        let mut planned: Vec<Column> = Vec::new();
        let mut planned_for = (0, 0.0);
        let mut mixed: Vec<f32> = Vec::new();
        // The same for the finer rows, which change shape as the zoom does.
        let mut fine: Vec<Option<Column>> = Vec::new();
        let mut fine_for = (0.0, 0.0, 0);
        for (r, (line, (floor, both, at))) in
            pixels.chunks_exact_mut(cols).zip(&self.rows).enumerate()
        {
            // A row from before a retune sits `shift` Hz along from where it
            // was taken: the view is moved the other way to read it.
            let shift = at - self.tuned;
            let (from, to) = (from - shift / self.span, to - shift / self.span);
            let row = &both.db;
            let n = row.len();
            // A finer row made while zoomed in is laid over the band's own
            // wherever it reaches; the rest (older rows, or a view since
            // moved) is drawn from the band's own, so nothing ever blanks.
            let zoom = both.zoom.as_ref();
            if let Some(z) = zoom {
                let key = (z.lo_hz + shift, z.step_hz, z.db.len());
                if key != fine_for || fine.len() != cols {
                    fine = overlay(z, cols, self.lo - shift, self.hi - shift);
                    fine_for = key;
                }
            }
            let whole = zoom.is_some() && fine.iter().all(Option::is_some);
            if (n, shift) != planned_for {
                planned = plan(n, cols, from, to);
                planned_for = (n, shift);
            }
            // Zoomed in, a bin is many pixels wide but a row is still one
            // high, and every bin of every row is its own draw of the noise:
            // the picture turns into dashes. So rows are averaged in time
            // there, more the wider a bin is drawn (a quarter of its width).
            let bin_px = cols as f64 / ((to - from) * n as f64);
            let mix = ((bin_px / 4.0 / self.thick as f64).round() as usize).clamp(1, 12);
            let row = if mix > 1 && !whole {
                let b = (((to * n as f64).ceil().max(0.0) as usize) + 1).min(n);
                let a = ((from * n as f64).max(0.0) as usize)
                    .saturating_sub(1)
                    .min(b);
                mixed.clear();
                mixed.resize(n, 0.0);
                let mut used = 0.0;
                for (_, older, older_at) in self.rows.iter().skip(r).take(mix) {
                    let older = &older.db;
                    if older.len() == n && older_at == at {
                        for (m, v) in mixed[a..b].iter_mut().zip(&older[a..b]) {
                            *m += v;
                        }
                        used += 1.0;
                    }
                }
                for m in &mut mixed[a..b] {
                    *m /= f32::max(used, 1.0);
                }
                &mixed
            } else {
                row
            };
            // From a little under the floor to 22 dB over it.
            let (lo, scale) = (floor - 3.0, 255.0 / 25.0);
            for (c, (px, column)) in line.iter_mut().zip(&planned).enumerate() {
                // A moved row does not reach all the way across: the rest
                // stays dark.
                if shift != 0.0 {
                    let middle = from + (to - from) * (c as f64 + 0.5) / cols as f64;
                    if !(0.0..1.0).contains(&middle) {
                        continue;
                    }
                }
                let db = match zoom {
                    Some(z) => match fine[c] {
                        Some(f) => read(&z.db, f),
                        None => read(row, *column),
                    },
                    None => read(row, *column),
                };
                *px = self.palette[((db - lo) * scale).clamp(0.0, 255.0) as usize];
            }
        }
        pixels
    }

    /// Bring the ring texture up to date.
    fn upload(&mut self, ctx: &egui::Context) {
        let [cols, shown] = self.size;
        if self.dirty || self.texture.is_none() || self.fresh >= RING {
            // The view changed: every row on screen is drawn again.
            let mut pixels = self.render((shown + WAITING).min(RING));
            pixels.resize(cols * RING, DARK);
            let image = ColorImage::new([cols, RING], pixels);
            match &mut self.texture {
                Some(t) => t.set(image, OPTIONS),
                None => self.texture = Some(ctx.load_texture("waterfall", image, OPTIONS)),
            }
            self.head = 0;
        } else if self.fresh > 0 {
            // Only the new rows, each written just above the last.
            let pixels = self.render(self.fresh);
            if let Some(t) = &mut self.texture {
                for line in pixels.chunks_exact(cols).rev() {
                    self.head = (self.head + RING - 1) % RING;
                    t.set_partial(
                        [0, self.head],
                        ColorImage::new([cols, 1], line.to_vec()),
                        OPTIONS,
                    );
                }
            }
        }
        self.fresh = 0;
        self.dirty = false;
    }

    /// Let waiting rows in at a steady pace; returns how many are still to
    /// come. `dt` is the time since the last frame.
    fn pacing(&mut self, dt: f32) -> f32 {
        // The pace follows how fast rows really come (a recording played
        // fast brings many more).
        self.counted.1 += dt;
        if self.counted.1 >= 0.5 {
            let measured = self.counted.0 / self.counted.1;
            self.pace += 0.5 * (measured - self.pace);
            self.counted = (0.0, 0.0);
        }
        if self.waiting > WAITING as f32 {
            // Far behind (the window was hidden, say): catch up at once.
            self.waiting = 3.0;
        }
        // A little faster with more than three waiting, a little slower with
        // fewer: it settles one clump behind.
        let speed = self.pace * (1.0 + 0.15 * (self.waiting - 3.0)).max(0.0);
        self.waiting = (self.waiting - speed * dt).max(0.0);
        self.waiting
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
            if let Some((lo, hi)) = self.want_view.take() {
                (self.lo, self.hi) = (lo, hi.max(lo + 1.0));
                self.clamp_view();
            }
            self.dirty = true;
        }
        if self.tuned != self.drawn_tuned {
            self.drawn_tuned = self.tuned;
            self.dirty = true;
        }
        // Everything sits on whole screen pixels (the window may be scaled:
        // `ppp` screen pixels to one of egui's points).
        let ppp = ui.ctx().pixels_per_point();
        let rect = egui::Rect::from_min_max(
            pos2(snap(rect.left(), ppp), snap(rect.top(), ppp)),
            pos2(snap(rect.right(), ppp), snap(rect.bottom(), ppp)),
        );
        let (lo, hi) = (self.lo, self.hi);
        let hz_at =
            |x: f32, lo: f64, hi: f64| lo + f64::from((x - rect.left()) / rect.width()) * (hi - lo);

        // Zoom about the cursor (DecSAT's: the smoothed wheel, so it glides).
        // It stops at 300 Hz across (the finest spectrum the engine makes
        // has 1 Hz bins), or at 32 of the band's own bins if that is less.
        let bins = view.spectrum_db.len().max(64) as f64;
        let min_view = (rate / bins * 32.0).clamp(1.0, 300.0);
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
        // The middle button tunes a live input: the band follows the mouse
        // and the radio is moved to suit. (A recording has no tuning, so
        // there it pans like the right button.)
        if view.live && response.dragged_by(PointerButton::Middle) {
            self.tune_by -= f64::from(response.drag_delta().x / rect.width()) * (hi - lo);
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else if response.dragged_by(PointerButton::Secondary)
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
        let spec_h = snap((rect.height() * 0.34).clamp(90.0, 320.0), ppp);
        let spec = egui::Rect::from_min_size(rect.min, vec2(rect.width(), spec_h));
        let axis =
            egui::Rect::from_min_size(spec.left_bottom(), vec2(rect.width(), snap(18.0, ppp)));
        let wf = egui::Rect::from_min_max(axis.left_bottom(), rect.max);
        painter.rect_filled(spec, 0, WELL);
        painter.rect_filled(axis, 0, AXIS);
        painter.rect_filled(wf, 0, DARK);

        // One column of the picture to a screen pixel, and each row a whole
        // number of them, drawn unfiltered: nothing is stretched or blurred.
        let cols = ((wf.width() * ppp).round() as usize).clamp(16, 8192);
        let high = (wf.height() * ppp).round().max(1.0) as usize;
        let thick = high.div_ceil(MAX_ROWS);
        let shown = (high / thick).max(1);
        self.shown = Some((lo, hi, cols));
        if self.size != [cols, shown] || self.thick != thick {
            self.size = [cols, shown];
            self.thick = thick;
            self.dirty = true;
        }
        self.upload(ui.ctx());
        // The picture starts above the top edge by the rows still to come,
        // in whole screen pixels, so it glides down instead of jumping a
        // clump of rows at a time.
        let waiting = self.pacing(ui.input(|i| i.stable_dt).min(0.1));
        let above = (waiting * thick as f32).round();
        if let Some(t) = &self.texture {
            let rows = shown + WAITING;
            let v0 = self.head as f32 / RING as f32;
            ui.painter_at(wf).image(
                t.id(),
                egui::Rect::from_min_size(
                    pos2(wf.left(), wf.top() - above / ppp),
                    vec2(cols as f32 / ppp, (rows * thick) as f32 / ppp),
                ),
                egui::Rect::from_min_max(pos2(0.0, v0), pos2(1.0, v0 + rows as f32 / RING as f32)),
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
                let y = mid(y_of(view.floor_db + step), ppp);
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
            // A point over the middle of every screen pixel, so the trace
            // stands exactly on the waterfall's columns.
            let mut levels = columns(&view.spectrum_db, cols, from, to);
            // Zoomed in: the finer average where it reaches.
            if let Some(z) = &view.zoom {
                for (level, f) in levels.iter_mut().zip(overlay(z, cols, lo, hi)) {
                    if let Some(f) = f {
                        *level = read(&z.db, f);
                    }
                }
            }
            let points: Vec<egui::Pos2> = levels
                .into_iter()
                .enumerate()
                .map(|(i, db)| pos2(spec.left() + (i as f32 + 0.5) / ppp, y_of(db)))
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
            let x = mid(x_of(hz), ppp);
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
            let a = mid(x_of(c.carrier_hz - width / 2.0), ppp);
            let b = mid(x_of(c.carrier_hz + width / 2.0), ppp).max(a + 3.0);
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
                    pos2(mid(x_of(a), ppp), spec.top()),
                    pos2(mid(x_of(b), ppp), rect.bottom()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoomed_out_keeps_the_peak() {
        let mut row = vec![0.0f32; 1000];
        row[333] = 20.0;
        let out = columns(&row, 100, 0.0, 1.0);
        assert_eq!(out.iter().filter(|v| **v == 20.0).count(), 1);
        assert_eq!(out[33], 20.0);
    }

    #[test]
    fn zoomed_in_runs_smoothly_between_bins() {
        // Bins 0, 10, 20 ... seen eight columns to a bin.
        let row: Vec<f32> = (0..64).map(|i| i as f32 * 10.0).collect();
        let out = columns(&row, 64, 0.25, 0.375);
        // The view begins at bin 16's left edge: half a bin before its middle.
        assert!((out[0] - 155.625).abs() < 1e-3, "{}", out[0]);
        for pair in out.windows(2) {
            assert!((pair[1] - pair[0] - 1.25).abs() < 1e-3);
        }
        // The ends of the row hold their last bin.
        assert_eq!(columns(&row, 640, 0.0, 1.0)[0], 0.0);
        assert_eq!(columns(&row, 640, 0.0, 1.0)[639], 630.0);
    }

    #[test]
    fn rows_come_in_steadily() {
        let mut band = Band::new();
        band.size = [16, 100];
        // Three rows every twentieth of a second, drawn sixty times a second.
        let mut steps = Vec::new();
        let mut shown_before = 0.0;
        let mut came = 0.0f32;
        for frame in 0..600 {
            if frame % 3 == 0 {
                band.push_rows(
                    (0..3).map(|_| Row {
                        db: vec![0.0; 64],
                        zoom: None,
                    }),
                    0.0,
                    0.0,
                );
                came += 3.0;
            }
            let shown = came - band.pacing(1.0 / 60.0);
            if frame >= 300 {
                steps.push(shown - shown_before);
            }
            shown_before = shown;
        }
        // Once settled: a row a frame, not three at once and then nothing.
        for step in steps {
            assert!((0.8..1.2).contains(&step), "{step}");
        }
        assert!(band.waiting < 5.0);
    }

    #[test]
    fn a_finer_row_is_laid_over_where_it_reaches() {
        // Bins at 100, 101 ... 199 Hz; the view is 150 to 250 Hz in 100 columns.
        let zoom = ZoomRow {
            lo_hz: 100.0,
            step_hz: 1.0,
            db: (0..100).map(|i| i as f32).collect(),
        };
        let over = overlay(&zoom, 100, 150.0, 250.0);
        // Covered up to 199.5 Hz: the first 49 columns (middles 150.5 to 198.5 Hz).
        assert!(over[..49].iter().all(Option::is_some));
        assert!(over[49..].iter().all(Option::is_none));
        // Each sits halfway between two bins.
        assert!((read(&zoom.db, over[0].unwrap()) - 50.5).abs() < 0.01);
        assert!((read(&zoom.db, over[48].unwrap()) - 98.5).abs() < 0.01);
        // Zoomed further in it is interpolated.
        let over = overlay(&zoom, 100, 150.0, 160.0);
        let v = read(&zoom.db, over[50].unwrap());
        assert!((v - 55.05).abs() < 1e-3, "{v}");
    }

    #[test]
    fn history_moves_along_after_a_retune() {
        // 64 bins over 64 kHz, one column a bin. One bright bin, 10 kHz up.
        let mut band = Band::new();
        (band.span, band.lo, band.hi) = (64_000.0, -32_000.0, 32_000.0);
        band.size = [64, 4];
        let mut db = vec![0.0; 64];
        db[42] = 30.0;
        let row = Row { db, zoom: None };
        band.push_rows(std::iter::once(row.clone()), 0.0, 1_545_000_000.0);
        // Retuned 5 kHz up: the same carrier is now 5 kHz above the centre,
        // in the new row and in the old one alike.
        band.tuned = 1_545_005_000.0;
        let mut db = vec![0.0; 64];
        db[37] = 30.0;
        band.push_rows(std::iter::once(Row { db, zoom: None }), 0.0, band.tuned);
        let pixels = band.render(2);
        let bright = |line: &[Color32]| {
            let top = band.palette[255];
            line.iter().position(|p| *p == top)
        };
        assert_eq!(bright(&pixels[..64]), Some(37));
        assert_eq!(bright(&pixels[64..]), Some(37));
        // The old row ends 5 kHz short of the top of the band now.
        assert!(pixels[64 + 59..].iter().all(|p| *p == DARK));
        assert!(pixels[64 + 58] != DARK);
    }

    #[test]
    fn pixel_snapping() {
        assert_eq!(snap(10.3, 1.25), 10.4);
        assert_eq!(mid(10.3, 1.0), 10.5);
        assert_eq!(mid(10.0, 2.0), 10.25);
    }
}
