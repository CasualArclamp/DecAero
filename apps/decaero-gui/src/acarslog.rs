// made by CasualArclamp
//! ACARS messages with text, kept in a plain text file a day (UTC), one
//! message a line so that a search for an aircraft's ICAO address (or its
//! registration, or any word) finds every message whole:
//!
//! ```text
//! 2026-11-21 16:04:12Z  ICAO 407FCD  G-XWBS A35K  GES 90  1546.1482 MHz  ACARS G-XWBS label A0 block D  |  /SNNCPXA.AFN/FMHBAW4LV
//! ```
//!
//! The lines of a message's text are joined with " | ".

use crate::aircraft::Info;
use crate::recorder;
use decaero_engine::LogLine;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

pub struct AcarsLog {
    dir: PathBuf,
    /// The day's file, and which day it is.
    file: Option<((i64, u32, u32), BufWriter<File>)>,
    /// Messages written this run.
    pub written: u64,
}

impl AcarsLog {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            file: None,
            written: 0,
        }
    }

    /// Beside the call recordings: Documents\DecAero\acars.
    pub fn default_dir() -> PathBuf {
        recorder::Recorder::default_dir().with_file_name("acars")
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Whether `line` belongs in the file: a message (not signalling) with a
    /// letter or a digit in its text.
    pub fn wanted(line: &LogLine) -> bool {
        line.message && line.text.chars().any(char::is_alphanumeric)
    }

    /// Add `line`, heard at `unix` seconds (UTC) on `mhz` (0 if unknown).
    pub fn write(
        &mut self,
        unix: f64,
        line: &LogLine,
        info: &Info,
        mhz: f64,
    ) -> Result<(), String> {
        let (y, mo, d, h, mi, s) = recorder::civil(unix as i64);
        let day = (y, mo, d);
        if self.file.as_ref().is_none_or(|(open, _)| *open != day) {
            std::fs::create_dir_all(&self.dir)
                .map_err(|e| format!("{}: {e}", self.dir.display()))?;
            let path = self.dir.join(format!("acars-{y:04}-{mo:02}-{d:02}.txt"));
            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            self.file = Some((day, BufWriter::new(file)));
        }
        let text = format_line((y, mo, d, h, mi, s), line, info, mhz);
        let (_, w) = self.file.as_mut().expect("opened above");
        // Flushed at once, so the file can be searched while it grows.
        writeln!(w, "{text}")
            .and_then(|()| w.flush())
            .map_err(|e| format!("ACARS log: {e}"))?;
        self.written += 1;
        Ok(())
    }
}

/// One message as one line.
fn format_line(
    (y, mo, d, h, mi, s): (i64, u32, u32, u32, u32, u32),
    line: &LogLine,
    info: &Info,
    mhz: f64,
) -> String {
    let mut out = format!("{y:04}-{mo:02}-{d:02} {h:02}:{mi:02}:{s:02}Z");
    match line.aes {
        Some(a) => out.push_str(&format!("  ICAO {a:06X}")),
        None => out.push_str("  ICAO ------"),
    }
    let short = [info.registration.as_str(), info.type_code.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    if !short.is_empty() {
        out.push_str(&format!("  {short}"));
    }
    if info.military {
        out.push_str("  MIL");
    }
    if let Some(g) = line.ges {
        out.push_str(&format!("  GES {g:02X}"));
    }
    if mhz > 0.0 {
        out.push_str(&format!("  {mhz:.4} MHz"));
    }
    out.push_str(&format!("  {}  |  ", line.title.trim()));
    let body: Vec<&str> = line
        .text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    out.push_str(&body.join(" | "));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use decaero_engine::channel::Kind;

    fn line(text: &str, message: bool) -> LogLine {
        LogLine {
            seq: 1,
            seconds: 0.0,
            channel: 3,
            kind: Kind::P(600),
            aes: Some(0x407FCD),
            ges: Some(0x90),
            title: "ACARS G-XWBS label A0 block D".into(),
            text: text.into(),
            message,
        }
    }

    #[test]
    fn a_message_is_one_line_with_its_icao_address() {
        let info = Info {
            registration: "G-XWBS".into(),
            type_code: "A35K".into(),
            ..Info::default()
        };
        let l = line("/SNNCPXA.AFN\r\n  NEW ETA \n\nBRGDS\n", true);
        let text = format_line((2026, 11, 21, 16, 4, 12), &l, &info, 1546.14826);
        assert_eq!(
            text,
            "2026-11-21 16:04:12Z  ICAO 407FCD  G-XWBS A35K  GES 90  1546.1483 MHz  \
             ACARS G-XWBS label A0 block D  |  /SNNCPXA.AFN | NEW ETA | BRGDS"
        );
        assert!(!text.contains('\n'));
    }

    #[test]
    fn only_messages_with_text_are_kept() {
        assert!(AcarsLog::wanted(&line("REQPOS", true)));
        assert!(!AcarsLog::wanted(&line(" = ", true)));
        assert!(!AcarsLog::wanted(&line("", true)));
        assert!(!AcarsLog::wanted(&line("call progress", false)));
    }

    #[test]
    fn the_file_is_named_by_the_day_and_appended_to() {
        let dir = std::env::temp_dir().join(format!("decaero-acars-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut log = AcarsLog::new(dir.clone());
        let info = Info::default();
        // 2026-11-21 23:59:59Z, then a second later the next day.
        let t = 1_795_305_599.0;
        log.write(t, &line("ONE", true), &info, 0.0).unwrap();
        log.write(t, &line("TWO", true), &info, 0.0).unwrap();
        log.write(t + 1.0, &line("THREE", true), &info, 0.0)
            .unwrap();
        let a = std::fs::read_to_string(dir.join("acars-2026-11-21.txt")).unwrap();
        let b = std::fs::read_to_string(dir.join("acars-2026-11-22.txt")).unwrap();
        assert_eq!(a.lines().count(), 2);
        assert!(a.lines().all(|l| l.contains("ICAO 407FCD")));
        assert!(b.ends_with("|  THREE\n"), "{b}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
