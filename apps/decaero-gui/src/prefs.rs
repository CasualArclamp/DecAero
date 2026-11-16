// made by CasualArclamp
//! Settings kept between runs: a small text file of `key = value` lines in
//! the user's settings folder (`%APPDATA%\DecAero\prefs.txt` on Windows).
//! It can be read and edited by hand; unknown keys are kept as they are.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Prefs {
    // A BTreeMap keeps its keys in order, so the file reads the same each
    // time it is written.
    values: BTreeMap<String, String>,
}

impl Prefs {
    /// Where the settings live unless `--prefs` says otherwise.
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from))
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_default();
        base.join("DecAero").join("prefs.txt")
    }

    /// Read the file; a missing or unreadable one gives no settings.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    pub fn parse(text: &str) -> Self {
        let mut values = BTreeMap::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                values.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
        Self { values }
    }

    pub fn text(&self) -> String {
        let mut out = String::from("# DecAero settings, written when they change.\n");
        for (key, value) in &self.values {
            out.push_str(&format!("{key} = {value}\n"));
        }
        out
    }

    /// Write the file, making its folder if need be.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        // Whole file to a neighbour first, then swapped in: a crash while
        // writing cannot leave half a file.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, self.text())?;
        std::fs::rename(&tmp, path)
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.values.get(key).map(String::as_str)
    }

    /// A value read as a number, a bool or anything else that parses.
    pub fn read<T: std::str::FromStr>(&self, key: &str) -> Option<T> {
        self.get(key)?.parse().ok()
    }

    /// Values are one line each: line breaks in one become spaces.
    pub fn set(&mut self, key: &str, value: impl ToString) {
        let value = value.to_string().replace(['\r', '\n'], " ");
        self.values
            .insert(key.to_string(), value.trim().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let mut p = Prefs::default();
        p.set("center_mhz", "1545.0");
        p.set("follow", true);
        p.set("gain.airspy.LNA", 11.0);
        p.set("sbs", "listen:127.0.0.1:30003");
        p.set("station", " two\nlines ");
        let back = Prefs::parse(&p.text());
        assert_eq!(back, p);
        assert_eq!(back.read::<bool>("follow"), Some(true));
        assert_eq!(back.read::<f64>("gain.airspy.LNA"), Some(11.0));
        // A value may itself hold "=" or ":".
        assert_eq!(back.get("sbs"), Some("listen:127.0.0.1:30003"));
        assert_eq!(back.get("station"), Some("two lines"));
        assert_eq!(back.read::<f64>("missing"), None);
    }

    #[test]
    fn hand_edits_are_tolerated() {
        let p = Prefs::parse("# note\n\n  volume=0.4  \nnonsense line\nurl = sdrpp://h:1?a=b\n");
        assert_eq!(p.read::<f32>("volume"), Some(0.4));
        assert_eq!(p.get("url"), Some("sdrpp://h:1?a=b"));
    }

    #[test]
    fn saves_and_loads() {
        let dir = std::env::temp_dir().join(format!("decaero-prefs-{}", std::process::id()));
        let path = dir.join("deep").join("prefs.txt");
        let mut p = Prefs::default();
        p.set("radio", "airspy");
        p.save(&path).unwrap();
        assert_eq!(Prefs::load(&path), p);
        assert_eq!(Prefs::load(&dir.join("absent.txt")), Prefs::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
