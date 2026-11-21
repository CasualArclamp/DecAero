// made by CasualArclamp
//! Who an aircraft is: registration, type and operator from a downloaded
//! database, the state of registry from its 24-bit address, and whether it
//! is military.
//!
//! The database is tar1090-db's `aircraft.csv.gz` (the Mictronics aircraft
//! database), one line per aircraft:
//! `address;registration;type;flags;description;year;owner;`. It is fetched
//! at run time into the settings folder and never shipped or committed.
//! The address ranges are the main allocations of ICAO Annex 10 volume III,
//! chapter 9, table 9-1.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const DATABASE_URL: &str =
    "https://raw.githubusercontent.com/wiedehopf/tar1090-db/csv/aircraft.csv.gz";

/// What is known about one aircraft.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Info {
    pub registration: String,
    /// ICAO type designator, such as B744.
    pub type_code: String,
    pub description: String,
    pub owner: String,
    pub military: bool,
    /// Two-letter state of registry, or "" when the address is in no range here.
    pub country: &'static str,
}

impl Info {
    /// "HL7646 · B744", or "" when nothing is known.
    pub fn short(&self) -> String {
        match (self.registration.is_empty(), self.type_code.is_empty()) {
            (true, true) => String::new(),
            (false, true) => self.registration.clone(),
            (true, false) => self.type_code.clone(),
            (false, false) => format!("{} · {}", self.registration, self.type_code),
        }
    }
}

/// (first address, last address, state).
#[rustfmt::skip]
const RANGES: &[(u32, u32, &str)] = &[
    (0x008000, 0x00FFFF, "ZA"), (0x010000, 0x017FFF, "EG"), (0x020000, 0x027FFF, "MA"),
    (0x040000, 0x040FFF, "ET"), (0x04C000, 0x04CFFF, "KE"), (0x06A000, 0x06A3FF, "QA"),
    (0x0AC000, 0x0ACFFF, "CO"), (0x0D0000, 0x0D7FFF, "MX"), (0x100000, 0x1FFFFF, "RU"),
    (0x300000, 0x33FFFF, "IT"), (0x340000, 0x37FFFF, "ES"), (0x380000, 0x3BFFFF, "FR"),
    (0x3C0000, 0x3FFFFF, "DE"), (0x400000, 0x43FFFF, "GB"), (0x440000, 0x447FFF, "AT"),
    (0x448000, 0x44FFFF, "BE"), (0x458000, 0x45FFFF, "DK"), (0x460000, 0x467FFF, "FI"),
    (0x468000, 0x46FFFF, "GR"), (0x470000, 0x477FFF, "HU"), (0x478000, 0x47FFFF, "NO"),
    (0x480000, 0x487FFF, "NL"), (0x488000, 0x48FFFF, "PL"), (0x490000, 0x497FFF, "PT"),
    (0x498000, 0x49FFFF, "CZ"), (0x4A0000, 0x4A7FFF, "RO"), (0x4A8000, 0x4AFFFF, "SE"),
    (0x4B0000, 0x4B7FFF, "CH"), (0x4B8000, 0x4BFFFF, "TR"), (0x4CA000, 0x4CAFFF, "IE"),
    (0x4CC000, 0x4CCFFF, "IS"), (0x4D0000, 0x4D03FF, "LU"), (0x4D2000, 0x4D23FF, "MT"),
    (0x508000, 0x50FFFF, "UA"), (0x683000, 0x6833FF, "KZ"), (0x702000, 0x702FFF, "BD"),
    (0x706000, 0x706FFF, "KW"), (0x70C000, 0x70C3FF, "OM"), (0x710000, 0x717FFF, "SA"),
    (0x718000, 0x71FFFF, "KR"), (0x730000, 0x737FFF, "IR"), (0x738000, 0x73FFFF, "IL"),
    (0x740000, 0x747FFF, "JO"), (0x750000, 0x757FFF, "MY"), (0x758000, 0x75FFFF, "PH"),
    (0x760000, 0x767FFF, "PK"), (0x768000, 0x76FFFF, "SG"), (0x770000, 0x777FFF, "LK"),
    (0x780000, 0x7BFFFF, "CN"), (0x7C0000, 0x7FFFFF, "AU"), (0x800000, 0x83FFFF, "IN"),
    (0x840000, 0x87FFFF, "JP"), (0x880000, 0x887FFF, "TH"), (0x888000, 0x88FFFF, "VN"),
    (0x894000, 0x894FFF, "BH"), (0x895000, 0x8953FF, "BN"), (0x896000, 0x896FFF, "AE"),
    (0x898000, 0x898FFF, "PG"), (0x899000, 0x8993FF, "TW"), (0x8A0000, 0x8A7FFF, "ID"),
    (0xA00000, 0xAFFFFF, "US"), (0xC00000, 0xC3FFFF, "CA"), (0xC80000, 0xC87FFF, "NZ"),
    (0xC88000, 0xC88FFF, "FJ"), (0xE00000, 0xE3FFFF, "AR"), (0xE40000, 0xE7FFFF, "BR"),
    (0xE80000, 0xE80FFF, "CL"), (0xE8C000, 0xE8CFFF, "PE"),
];

/// The block the United States sets aside for military aircraft.
const US_MILITARY: (u32, u32) = (0xADF7C8, 0xAFFFFF);

/// State of registry from the address alone.
pub fn country(aes: u32) -> &'static str {
    RANGES
        .iter()
        .find(|(lo, hi, _)| (*lo..=*hi).contains(&aes))
        .map_or("", |r| r.2)
}

/// The decompressed database and where each aircraft's line is in it.
struct Table {
    text: String,
    /// Address to (start, length) of its line.
    lines: HashMap<u32, (u32, u32)>,
}

impl Table {
    fn load(path: &Path) -> std::io::Result<Self> {
        let file = std::fs::File::open(path)?;
        let mut text = String::new();
        // The file is latin-ish in places: read bytes and convert leniently.
        let mut bytes = Vec::new();
        flate2::read::MultiGzDecoder::new(file).read_to_end(&mut bytes)?;
        text.push_str(&String::from_utf8_lossy(&bytes));
        let mut lines = HashMap::new();
        let mut start = 0usize;
        for line in text.split_inclusive('\n') {
            if let Some(hex) = line.split(';').next()
                && let Ok(aes) = u32::from_str_radix(hex, 16)
            {
                lines.insert(aes, (start as u32, line.trim_end().len() as u32));
            }
            start += line.len();
        }
        Ok(Self { text, lines })
    }

    fn get(&self, aes: u32) -> Option<Info> {
        let (start, len) = *self.lines.get(&aes)?;
        let line = &self.text[start as usize..(start + len) as usize];
        let mut f = line.split(';');
        let _address = f.next();
        let registration = f.next().unwrap_or("").to_string();
        let type_code = f.next().unwrap_or("").to_string();
        // Flags are a string of 0/1: military first.
        let military = f.next().unwrap_or("").starts_with('1');
        let description = f.next().unwrap_or("").to_string();
        let _year = f.next();
        let owner = f.next().unwrap_or("").to_string();
        Some(Info {
            registration,
            type_code,
            description,
            owner,
            military,
            country: "",
        })
    }
}

/// How the database stands.
#[derive(Debug, Clone, PartialEq)]
pub enum State {
    /// No file yet: offer to fetch it.
    Missing,
    Loading,
    Downloading,
    Ready {
        aircraft: usize,
    },
    Failed(String),
}

struct Inner {
    table: Option<Table>,
    state: State,
}

/// Shared with the threads that load and download.
#[derive(Clone)]
pub struct Lookup {
    inner: Arc<Mutex<Inner>>,
    path: PathBuf,
}

impl Lookup {
    /// `path` is where the database is, or will be put.
    pub fn new(path: PathBuf) -> Self {
        let lookup = Self {
            inner: Arc::new(Mutex::new(Inner {
                table: None,
                state: State::Missing,
            })),
            path,
        };
        if lookup.path.is_file() {
            lookup.load();
        }
        lookup
    }

    /// Where the database lives unless told otherwise: the settings folder,
    /// or `db/` beside the working directory when that already has one.
    pub fn default_path() -> PathBuf {
        if let Some(dir) = crate::data_dir() {
            return dir.join("aircraft.csv.gz");
        }
        let local = PathBuf::from("db").join("aircraft.csv.gz");
        if local.is_file() {
            return local;
        }
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_default();
        base.join("DecAero").join("aircraft.csv.gz")
    }

    pub fn state(&self) -> State {
        self.inner.lock().unwrap().state.clone()
    }

    fn load(&self) {
        self.inner.lock().unwrap().state = State::Loading;
        let (inner, path) = (self.inner.clone(), self.path.clone());
        std::thread::spawn(move || {
            let loaded = Table::load(&path);
            let mut i = inner.lock().unwrap();
            match loaded {
                Ok(t) => {
                    i.state = State::Ready {
                        aircraft: t.lines.len(),
                    };
                    i.table = Some(t);
                }
                Err(e) => i.state = State::Failed(format!("{}: {e}", path.display())),
            }
        });
    }

    /// Fetch the database (about 9 MB) and load it. Only on the user's click.
    pub fn download(&self) {
        self.inner.lock().unwrap().state = State::Downloading;
        let me = self.clone();
        std::thread::spawn(move || {
            let fetch = || -> Result<(), String> {
                let mut body = ureq::get(DATABASE_URL)
                    .call()
                    .map_err(|e| e.to_string())?
                    .into_body();
                let bytes = body
                    .with_config()
                    .limit(64 << 20)
                    .read_to_vec()
                    .map_err(|e| e.to_string())?;
                if let Some(dir) = me.path.parent() {
                    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                }
                std::fs::write(&me.path, bytes).map_err(|e| e.to_string())
            };
            match fetch() {
                Ok(()) => me.load(),
                Err(e) => me.inner.lock().unwrap().state = State::Failed(e),
            }
        });
    }

    /// Everything known about an address. The country and the United States
    /// military block come from the address itself, so they are there even
    /// without the database.
    pub fn get(&self, aes: u32) -> Info {
        let mut info = self
            .inner
            .lock()
            .unwrap()
            .table
            .as_ref()
            .and_then(|t| t.get(aes))
            .unwrap_or_default();
        info.country = country(aes);
        info.military |= (US_MILITARY.0..=US_MILITARY.1).contains(&aes);
        info
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countries_from_addresses() {
        assert_eq!(country(0x7C4A21), "AU");
        assert_eq!(country(0xA4C79B), "US");
        assert_eq!(country(0xC02ECA), "CA");
        assert_eq!(country(0x71BE46), "KR");
        assert_eq!(country(0x4BA9E2), "TR");
        assert_eq!(country(0x000001), "");
        // Ranges must not overlap.
        for (i, a) in RANGES.iter().enumerate() {
            assert!(a.0 <= a.1);
            for b in &RANGES[i + 1..] {
                assert!(a.1 < b.0 || b.1 < a.0, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn a_line_of_the_database() {
        let text = "000001;;;10;;;Miscode;\n7C531C;VH-QPA;A333;00;AIRBUS A-330-300;2003;QANTAS AIRWAYS LIMITED;\n".to_string();
        let mut lines = HashMap::new();
        lines.insert(0x7C531C, (23, 66));
        let t = Table { text, lines };
        let a = t.get(0x7C531C).expect("found");
        assert_eq!(a.registration, "VH-QPA");
        assert_eq!(a.type_code, "A333");
        assert_eq!(a.owner, "QANTAS AIRWAYS LIMITED");
        assert!(!a.military);
        assert!(
            Lookup::new(PathBuf::from("no-such-file"))
                .get(0xAE2FAC)
                .military
        );
    }
}
