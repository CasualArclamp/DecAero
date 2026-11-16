// made by CasualArclamp
//! Channel activity history: how much of each minute every frequency was in
//! use, drawn as a heat map (frequencies down, time across).
//!
//! A live input's history is kept between runs in a text file of
//! `minute hertz mode seconds` lines (minutes counted from 1970, UTC). Lines
//! are only ever added while running; the file is tidied when it is read.

use crate::recorder::civil;
use crate::theme::*;
use decaero_engine::channel::{ChannelId, ChannelStatus, Kind};
use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense, Ui, pos2, vec2};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Frequencies are rounded to the Aero channel raster, so a carrier a few
/// hertz off from one day to the next stays on one row.
const RASTER_HZ: f64 = 2500.0;
/// How far back the file goes.
const KEEP_MINUTES: u32 = 8 * 24 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mode {
    P(u32),
    Rt(u32),
    C,
}

impl Mode {
    fn of(kind: Kind) -> Option<Self> {
        match kind {
            Kind::P(r) => Some(Mode::P(r)),
            Kind::Rt(r) => Some(Mode::Rt(r)),
            Kind::C => Some(Mode::C),
            _ => None,
        }
    }

    /// As written in the file.
    fn code(self) -> String {
        match self {
            Mode::P(r) => format!("P{r}"),
            Mode::Rt(r) => format!("RT{r}"),
            Mode::C => "C".into(),
        }
    }

    fn parse(s: &str) -> Option<Self> {
        if s == "C" {
            Some(Mode::C)
        } else if let Some(r) = s.strip_prefix("RT") {
            r.parse().ok().map(Mode::Rt)
        } else {
            s.strip_prefix('P')?.parse().ok().map(Mode::P)
        }
    }

    fn label(self) -> String {
        match self {
            Mode::P(r) => format!("P {r}"),
            Mode::Rt(r) => format!("R/T {r}"),
            Mode::C => "voice".into(),
        }
    }
}

/// One line of the map: a frequency in one use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Row {
    pub hz: i64,
    pub mode: Mode,
}

impl Row {
    fn label(&self) -> String {
        if self.hz.abs() >= 1_000_000 {
            format!("{:.4}  {}", self.hz as f64 / 1e6, self.mode.label())
        } else {
            // No centre frequency given: offsets only.
            format!("{:+.1} kHz  {}", self.hz as f64 / 1e3, self.mode.label())
        }
    }
}

/// How much time the map shows, and so how long one column is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    Hour,
    SixHours,
    Day,
    Week,
}

impl Span {
    pub const ALL: [Span; 4] = [Span::Hour, Span::SixHours, Span::Day, Span::Week];

    /// Columns, minutes in each, and minutes between labels.
    fn shape(self) -> (u32, u32, u32) {
        match self {
            Span::Hour => (60, 1, 10),
            Span::SixHours => (72, 5, 60),
            Span::Day => (96, 15, 180),
            Span::Week => (84, 120, 1440),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Span::Hour => "1 h",
            Span::SixHours => "6 h",
            Span::Day => "24 h",
            Span::Week => "7 d",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|x| x.label() == s)
    }
}

/// The map as drawn: a share of the time (0 to 1) per row and column.
pub struct Grid {
    span: Span,
    start_min: u32,
    step: u32,
    cols: u32,
    rows: Vec<(Row, Vec<f32>)>,
}

pub struct Activity {
    /// Seconds in use, per row and minute.
    rows: BTreeMap<Row, BTreeMap<u32, f32>>,
    /// What has been counted but not yet written.
    pending: BTreeMap<(u32, Row), f32>,
    path: Option<PathBuf>,
    last_t: Option<f64>,
    /// Bursts each burst channel had when last looked at.
    bursts: HashMap<ChannelId, u64>,
    pub error: Option<String>,
}

impl Activity {
    /// A history of this run only.
    pub fn new() -> Self {
        Self {
            rows: BTreeMap::new(),
            pending: BTreeMap::new(),
            path: None,
            last_t: None,
            bursts: HashMap::new(),
            error: None,
        }
    }

    /// The history kept in `path`, carried on from where it stopped.
    pub fn kept(path: &Path, now_min: u32) -> Self {
        let mut a = Self::new();
        a.path = Some(path.to_path_buf());
        let Ok(text) = std::fs::read_to_string(path) else {
            return a;
        };
        let oldest = now_min.saturating_sub(KEEP_MINUTES);
        let mut lines = 0usize;
        for line in text.lines() {
            let mut parts = line.split_whitespace();
            let (Some(minute), Some(hz), Some(mode), Some(seconds)) = (
                parts.next().and_then(|v| v.parse::<u32>().ok()),
                parts.next().and_then(|v| v.parse::<i64>().ok()),
                parts.next().and_then(Mode::parse),
                parts.next().and_then(|v| v.parse::<f32>().ok()),
            ) else {
                continue;
            };
            lines += 1;
            if minute < oldest {
                continue;
            }
            let cell = a
                .rows
                .entry(Row { hz, mode })
                .or_default()
                .entry(minute)
                .or_default();
            *cell = (*cell + seconds).min(60.0);
        }
        // Old minutes gone, or a minute written in two parts: write it afresh.
        let cells: usize = a.rows.values().map(BTreeMap::len).sum();
        if cells != lines
            && let Err(e) = a.rewrite(path)
        {
            a.error = Some(format!("{}: {e}", path.display()));
        }
        a
    }

    fn rewrite(&self, path: &Path) -> std::io::Result<()> {
        let mut out = String::new();
        for (row, minutes) in &self.rows {
            for (minute, seconds) in minutes {
                out.push_str(&line(*minute, row, *seconds));
            }
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, path)
    }

    /// Count what is in use now. `t` is seconds since 1970; call it as often
    /// as you like, it counts once a second.
    pub fn note(&mut self, t: f64, center_hz: f64, channels: &[ChannelStatus]) {
        let Some(last) = self.last_t else {
            self.last_t = Some(t);
            return;
        };
        let dt = t - last;
        if dt < 0.0 {
            self.last_t = Some(t);
            return;
        }
        if dt < 1.0 {
            return;
        }
        self.last_t = Some(t);
        // After a pause or a stall only the last moments count.
        let dt = dt.min(2.0) as f32;
        let minute = (t / 60.0) as u32;
        for c in channels {
            let Some(mode) = Mode::of(c.kind) else {
                continue;
            };
            // A burst channel has no carrier to hold: a burst is its sign of life.
            let active = match mode {
                Mode::Rt(_) => {
                    let before = self.bursts.insert(c.id, c.bursts);
                    before.is_some_and(|b| c.bursts > b)
                }
                _ => c.locked && !c.closed,
            };
            if !active {
                continue;
            }
            let hz = (((center_hz + c.carrier_hz) / RASTER_HZ).round() * RASTER_HZ) as i64;
            let row = Row { hz, mode };
            let cell = self.rows.entry(row).or_default().entry(minute).or_default();
            let add = dt.min(60.0 - *cell);
            *cell += add;
            if self.path.is_some() {
                *self.pending.entry((minute, row)).or_default() += add;
            }
        }
        // Minutes that are over go to the file.
        if self.pending.keys().next().is_some_and(|(m, _)| *m < minute) {
            self.flush(minute);
        }
    }

    /// Write what was counted in the minutes before `before`.
    fn flush(&mut self, before: u32) {
        let Some(path) = &self.path else { return };
        let mut out = String::new();
        self.pending.retain(|(minute, row), seconds| {
            if *minute >= before {
                return true;
            }
            out.push_str(&line(*minute, row, *seconds));
            false
        });
        if out.is_empty() {
            return;
        }
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| {
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(path)
            })
            .and_then(|mut f| f.write_all(out.as_bytes()));
        if let Err(e) = written {
            self.error = Some(format!("activity not kept in {}: {e}", path.display()));
            self.path = None;
        }
    }

    pub fn grid(&self, now_min: u32, span: Span) -> Grid {
        let (cols, step, _) = span.shape();
        // The newest column is the one now filling.
        let end = (now_min / step + 1) * step;
        let start_min = end.saturating_sub(cols * step);
        let mut rows = Vec::new();
        // Highest frequency at the top.
        for (row, minutes) in self.rows.iter().rev() {
            let mut values = vec![0.0f32; cols as usize];
            let mut any = false;
            for (minute, seconds) in minutes.range(start_min..end) {
                values[((minute - start_min) / step) as usize] += seconds / (step as f32 * 60.0);
                any = true;
            }
            if any {
                rows.push((*row, values));
            }
        }
        Grid {
            span,
            start_min,
            step,
            cols,
            rows,
        }
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        self.flush(u32::MAX);
    }
}

fn line(minute: u32, row: &Row, seconds: f32) -> String {
    format!("{minute} {} {} {seconds:.1}\n", row.hz, row.mode.code())
}

/// A minute since 1970 as (month, day, hour, minute), UTC.
fn when(minute: u32) -> (u32, u32, u32, u32) {
    let (_, month, day, h, m, _) = civil(i64::from(minute) * 60);
    (month, day, h, m)
}

/// The map on screen: the span picked and the grid last worked out.
pub struct Panel {
    pub span: Span,
    grid: Option<Grid>,
    built: Instant,
    palette: Vec<Color32>,
}

impl Panel {
    pub fn new() -> Self {
        Self {
            span: Span::Hour,
            grid: None,
            built: Instant::now(),
            palette: palette(),
        }
    }

    /// Draw the map. `now` is seconds since 1970 on the history's own clock.
    pub fn show(&mut self, ui: &mut Ui, activity: &Activity, now: f64) {
        const LEFT: f32 = 136.0;
        const ROW: f32 = 13.0;
        // Summing a week of minutes sixty times a second would be a waste.
        if self.grid.as_ref().is_none_or(|g| g.span != self.span)
            || self.built.elapsed().as_secs_f32() > 1.0
        {
            self.grid = Some(activity.grid((now / 60.0) as u32, self.span));
            self.built = Instant::now();
        }
        let Some(grid) = &self.grid else { return };
        if grid.rows.is_empty() {
            ui.label(RichText::new("Nothing decoded in this time yet.").color(DIM));
            return;
        }
        let (_, _, every) = self.span.shape();
        let minutes = (grid.cols * grid.step) as f32;
        let width = ui.available_width();
        let font = FontId::monospace(10.0);

        // Times along the top, UTC.
        let (axis, _) = ui.allocate_exact_size(vec2(width, 14.0), Sense::hover());
        let x0 = axis.left() + LEFT;
        let w = (axis.right() - x0 - 6.0).max(50.0);
        let painter = ui.painter_at(axis);
        painter.text(
            pos2(axis.left(), axis.center().y),
            Align2::LEFT_CENTER,
            "MHz · UTC →",
            font.clone(),
            FAINT,
        );
        let mut tick = grid.start_min.div_ceil(every) * every;
        while tick < grid.start_min + grid.cols * grid.step {
            let x = x0 + (tick - grid.start_min) as f32 / minutes * w;
            let (month, day, h, m) = when(tick);
            let text = if self.span == Span::Week {
                format!("{month:02}-{day:02}")
            } else {
                format!("{h:02}:{m:02}")
            };
            painter.line_segment(
                [pos2(x, axis.bottom() - 3.0), pos2(x, axis.bottom())],
                (1.0, FAINT),
            );
            painter.text(
                pos2(x + 2.0, axis.center().y),
                Align2::LEFT_CENTER,
                text,
                font.clone(),
                DIM,
            );
            tick += every;
        }

        egui::ScrollArea::vertical()
            .id_salt("activity")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let (rect, response) = ui.allocate_exact_size(
                    vec2(width, ROW * grid.rows.len() as f32),
                    Sense::hover(),
                );
                let painter = ui.painter_at(rect);
                let cell_w = w / grid.cols as f32;
                for (i, (row, values)) in grid.rows.iter().enumerate() {
                    let y = rect.top() + i as f32 * ROW;
                    painter.text(
                        pos2(rect.left(), y + ROW / 2.0),
                        Align2::LEFT_CENTER,
                        row.label(),
                        FontId::monospace(11.0),
                        if row.mode == Mode::C { AMBER } else { TEXT },
                    );
                    painter.rect_filled(
                        egui::Rect::from_min_size(pos2(x0, y), vec2(w, ROW - 1.0)),
                        0.0,
                        WELL,
                    );
                    for (c, v) in values.iter().enumerate() {
                        if *v <= 0.0 {
                            continue;
                        }
                        // A square root lifts the short uses (a burst, a
                        // brief call) out of the dark end.
                        let shade = (v.clamp(0.0, 1.0).sqrt() * 215.0) as usize + 40;
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                pos2(x0 + c as f32 * cell_w, y),
                                vec2(cell_w.ceil(), ROW - 1.0),
                            ),
                            0.0,
                            self.palette[shade.min(255)],
                        );
                    }
                }
                // What is under the pointer.
                if let Some(p) = response.hover_pos()
                    && p.x >= x0
                    && p.x < x0 + w
                {
                    let r = ((p.y - rect.top()) / ROW) as usize;
                    let c = ((p.x - x0) / cell_w) as usize;
                    if let Some((row, values)) = grid.rows.get(r)
                        && let Some(v) = values.get(c)
                    {
                        let from = grid.start_min + c as u32 * grid.step;
                        let (month, day, h, m) = when(from);
                        let (_, _, h2, m2) = when(from + grid.step);
                        response.on_hover_text_at_pointer(format!(
                            "{}\n{month:02}-{day:02}  {h:02}:{m:02} to {h2:02}:{m2:02} UTC\nin use {:.0} % of the time",
                            row.label(),
                            v * 100.0
                        ));
                    }
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn channel(id: ChannelId, kind: Kind, carrier_hz: f64, locked: bool) -> ChannelStatus {
        let mut c = ChannelStatus::new(id, carrier_hz, 0.0);
        c.kind = kind;
        c.locked = locked;
        c
    }

    #[test]
    fn counts_what_is_locked() {
        let mut a = Activity::new();
        let t0 = 1_000_000.0 * 60.0;
        let mut channels = vec![
            channel(1, Kind::P(600), 2400.0, true),
            channel(2, Kind::C, 30_100.0, false),
            channel(3, Kind::Rt(600), -5000.0, false),
            channel(4, Kind::Searching, 9000.0, true),
        ];
        for s in 0..=120 {
            // A call for 30 s, and one burst at 90 s.
            channels[1].locked = (30..=59).contains(&s);
            if s == 90 {
                channels[2].bursts += 1;
            }
            a.note(t0 + f64::from(s), 1545e6, &channels);
        }
        let grid = a.grid(1_000_001, Span::Hour);
        assert_eq!(grid.rows.len(), 3);
        let find = |mode: Mode| {
            let (row, values) = grid.rows.iter().find(|(r, _)| r.mode == mode).unwrap();
            (row.hz, values[58], values[59])
        };
        // On the raster, and the whole of both minutes for the P channel.
        let (hz, first, second) = find(Mode::P(600));
        assert_eq!(hz, 1_545_002_500);
        assert!((first - 1.0).abs() < 0.02 && (second - 1.0).abs() < 0.02);
        let (hz, first, second) = find(Mode::C);
        assert_eq!(hz, 1_545_030_000);
        assert!(
            (first - 0.5).abs() < 0.03 && second == 0.0,
            "{first} {second}"
        );
        let (_, first, second) = find(Mode::Rt(600));
        assert!(first == 0.0 && second > 0.0 && second < 0.05);
        // Highest frequency first.
        assert!(grid.rows[0].0.hz > grid.rows[2].0.hz);
    }

    #[test]
    fn kept_between_runs() {
        let dir = std::env::temp_dir().join(format!("decaero-activity-{}", std::process::id()));
        let path = dir.join("activity.txt");
        let now = 2_000_000u32;
        let channels = [channel(1, Kind::P(1200), 0.0, true)];
        {
            let mut a = Activity::kept(&path, now);
            for s in 0..=90 {
                a.note(f64::from(now) * 60.0 + f64::from(s), 1545e6, &channels);
            }
            // The first minute is written when it is over, the rest on drop.
        }
        // Something too old to keep, and a line that is not one.
        let mut text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        text.push_str("100 1545000000 C 60.0\nrubbish\n");
        std::fs::write(&path, text).unwrap();
        // A second run in the same minute adds to it.
        {
            let mut a = Activity::kept(&path, now + 1);
            for s in 90..=100 {
                a.note(f64::from(now) * 60.0 + f64::from(s), 1545e6, &channels);
            }
        }
        let a = Activity::kept(&path, now + 1);
        assert!(a.error.is_none());
        let minutes = &a.rows[&Row {
            hz: 1_545_000_000,
            mode: Mode::P(1200),
        }];
        assert_eq!(a.rows.len(), 1);
        assert!((minutes[&now] - 59.0).abs() < 0.5);
        assert!(
            (minutes[&(now + 1)] - 40.0).abs() < 1.5,
            "{}",
            minutes[&(now + 1)]
        );
        // Tidied: one line a cell.
        drop(a);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2, "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn modes_round_trip() {
        for m in [Mode::P(600), Mode::P(10500), Mode::Rt(1200), Mode::C] {
            assert_eq!(Mode::parse(&m.code()), Some(m));
        }
        assert_eq!(Mode::parse("X"), None);
        assert_eq!(Span::parse("24 h"), Some(Span::Day));
    }
}
