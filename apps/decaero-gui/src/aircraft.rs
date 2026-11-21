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
//!
//! **Live lookup.** When it is on, each address seen is also asked of
//! adsbdb.com (`api.adsbdb.com/v0/aircraft/HEX`: registration, type,
//! registered owner), once, in the background and no faster than one a
//! second. What it says comes before the database: it names the owner far
//! more often, and the owner decides "military" (an air force, navy, army
//! and the like), not the database's flag, which marks some airliners and
//! government transports. What was found is kept in `aircraft-live.txt`
//! beside the database, so each address is asked only once. Only the
//! 24-bit address leaves the machine.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Where an address is looked up live.
pub const LIVE_URL: &str = "https://api.adsbdb.com/v0/aircraft/";

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

/// What the live lookup knows of an address.
#[derive(Debug, Clone, PartialEq)]
enum Live {
    /// Waiting its turn, or being asked.
    Asked,
    Found(Info),
    /// The service does not know it (or could not be reached this run).
    Unknown,
}

struct Inner {
    table: Option<Table>,
    state: State,
    live: HashMap<u32, Live>,
    /// Addresses waiting to be asked, oldest first.
    queue: VecDeque<u32>,
    /// Live lookups answered, and the last error, for the panel.
    live_count: usize,
    live_error: Option<String>,
}

/// Shared with the threads that load and download.
#[derive(Clone)]
pub struct Lookup {
    inner: Arc<Mutex<Inner>>,
    path: PathBuf,
    /// Live lookup on (and its thread running).
    live_on: Arc<AtomicBool>,
    live_started: Arc<AtomicBool>,
}

/// Whether a registered owner is a military one.
fn military_owner(owner: &str) -> bool {
    let o = owner.to_ascii_lowercase();
    [
        "air force",
        "navy",
        "naval",
        "army",
        "military",
        "marine corps",
        "marines",
        "coast guard",
        "defence",
        "defense",
        "luftwaffe",
        "armée",
        "fuerza aérea",
        "fuerza aerea",
        "aeronautica militare",
        "people's liberation",
        "plaaf",
        "national guard",
    ]
    .iter()
    .any(|w| o.contains(w))
}

/// A string field of adsbdb's answer (`"name":"value"`), with the escapes
/// it uses undone. Enough for this one flat object, so no JSON crate.
fn json_field(body: &str, name: &str) -> String {
    let key = format!("\"{name}\":");
    let Some(at) = body.find(&key) else {
        return String::new();
    };
    let rest = body[at + key.len()..].trim_start();
    let Some(rest) = rest.strip_prefix('"') else {
        return String::new();
    };
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => break,
            '\\' => match chars.next() {
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
                Some('n' | 't' | 'r') => out.push(' '),
                Some(other) => out.push(other),
                None => break,
            },
            c => out.push(c),
        }
    }
    out.trim().to_string()
}

/// adsbdb's answer for one address: `None` when it does not know it.
fn parse_live(body: &str) -> Option<Info> {
    if !body.contains("\"aircraft\"") {
        return None;
    }
    let owner = json_field(body, "registered_owner");
    let manufacturer = json_field(body, "manufacturer");
    let model = json_field(body, "type");
    let description = match (manufacturer.is_empty(), model.is_empty()) {
        (false, false) => format!("{manufacturer} {model}"),
        (true, false) => model,
        (false, true) => manufacturer,
        (true, true) => String::new(),
    };
    Some(Info {
        registration: json_field(body, "registration"),
        type_code: json_field(body, "icao_type"),
        description,
        military: military_owner(&owner),
        owner,
        country: "",
    })
}

/// One line of the kept answers: `HEX;registration;type;description;owner`.
fn live_line(aes: u32, i: &Info) -> String {
    let clean = |s: &str| s.replace([';', '\n', '\r'], " ");
    format!(
        "{aes:06X};{};{};{};{}\n",
        clean(&i.registration),
        clean(&i.type_code),
        clean(&i.description),
        clean(&i.owner)
    )
}

fn read_live_line(line: &str) -> Option<(u32, Info)> {
    let mut f = line.trim_end().split(';');
    let aes = u32::from_str_radix(f.next()?, 16).ok()?;
    let registration = f.next()?.to_string();
    let type_code = f.next()?.to_string();
    let description = f.next()?.to_string();
    let owner = f.next()?.to_string();
    Some((
        aes,
        Info {
            registration,
            type_code,
            description,
            military: military_owner(&owner),
            owner,
            country: "",
        },
    ))
}

impl Lookup {
    /// `path` is where the database is, or will be put.
    pub fn new(path: PathBuf) -> Self {
        // Answers kept from earlier runs.
        let mut live = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(path.with_file_name("aircraft-live.txt")) {
            for (aes, info) in text.lines().filter_map(read_live_line) {
                live.insert(aes, Live::Found(info));
            }
        }
        let lookup = Self {
            inner: Arc::new(Mutex::new(Inner {
                table: None,
                state: State::Missing,
                live,
                queue: VecDeque::new(),
                live_count: 0,
                live_error: None,
            })),
            path,
            live_on: Arc::new(AtomicBool::new(false)),
            live_started: Arc::new(AtomicBool::new(false)),
        };
        if lookup.path.is_file() {
            lookup.load();
        }
        lookup
    }

    /// Turn the live lookup on or off. Turned on, its thread starts.
    pub fn set_live(&self, on: bool) {
        self.live_on.store(on, Ordering::Relaxed);
        if on && !self.live_started.swap(true, Ordering::Relaxed) {
            let me = self.clone();
            std::thread::Builder::new()
                .name("aircraft-live".into())
                .spawn(move || me.live_thread())
                .ok();
        }
    }

    pub fn live(&self) -> bool {
        self.live_on.load(Ordering::Relaxed)
    }

    /// How the live lookup is doing, a few words for the panel.
    pub fn live_status(&self) -> String {
        let i = self.inner.lock().unwrap();
        let known = i
            .live
            .values()
            .filter(|l| matches!(l, Live::Found(_)))
            .count();
        match &i.live_error {
            Some(e) => format!("live lookup: {e}"),
            None => format!(
                "live: {known} known, {} waiting, {} asked this run",
                i.queue.len(),
                i.live_count
            ),
        }
    }

    /// Ask adsbdb, one address a second, while the lookup is on.
    fn live_thread(&self) {
        let keep = self.path.with_file_name("aircraft-live.txt");
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(std::time::Duration::from_secs(10)))
            .http_status_as_error(false)
            .build()
            .into();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(1100));
            if !self.live_on.load(Ordering::Relaxed) {
                continue;
            }
            let Some(aes) = self.inner.lock().unwrap().queue.pop_front() else {
                continue;
            };
            let answer = agent
                .get(&format!("{LIVE_URL}{aes:06X}"))
                .header("User-Agent", "DecAero")
                .call()
                .map_err(|e| e.to_string())
                .and_then(|r| {
                    let status = r.status().as_u16();
                    let body = r
                        .into_body()
                        .with_config()
                        .limit(1 << 16)
                        .read_to_string()
                        .map_err(|e| e.to_string())?;
                    match status {
                        200 | 404 => Ok(body),
                        429 => Err("asked too often; slowing down".to_string()),
                        s => Err(format!("HTTP {s}")),
                    }
                });
            let mut i = self.inner.lock().unwrap();
            match answer {
                Ok(body) => {
                    i.live_count += 1;
                    i.live_error = None;
                    match parse_live(&body) {
                        Some(info) => {
                            // Kept for the next run; a failed write only
                            // means it is asked again then.
                            use std::io::Write;
                            if let Some(dir) = keep.parent() {
                                let _ = std::fs::create_dir_all(dir);
                            }
                            if let Ok(mut f) = std::fs::OpenOptions::new()
                                .create(true)
                                .append(true)
                                .open(&keep)
                            {
                                let _ = f.write_all(live_line(aes, &info).as_bytes());
                            }
                            i.live.insert(aes, Live::Found(info));
                        }
                        None => {
                            i.live.insert(aes, Live::Unknown);
                        }
                    }
                }
                Err(e) => {
                    // Asked again later this run; a pause before the next.
                    i.live.remove(&aes);
                    i.live_error = Some(e);
                    drop(i);
                    std::thread::sleep(std::time::Duration::from_secs(10));
                }
            }
        }
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
    ///
    /// With the live lookup on, an address not yet asked is queued, and once
    /// answered the live fields come first, the database filling what they
    /// leave empty. "Military" is then the owner's, as long as the live
    /// answer names one.
    pub fn get(&self, aes: u32) -> Info {
        let mut i = self.inner.lock().unwrap();
        let mut info = i
            .table
            .as_ref()
            .and_then(|t| t.get(aes))
            .unwrap_or_default();
        if self.live_on.load(Ordering::Relaxed) && aes != 0 && !i.live.contains_key(&aes) {
            i.live.insert(aes, Live::Asked);
            i.queue.push_back(aes);
        }
        if let Some(Live::Found(live)) = i.live.get(&aes) {
            let pick = |a: &String, b: String| if a.is_empty() { b } else { a.clone() };
            info = Info {
                registration: pick(&live.registration, info.registration),
                type_code: pick(&live.type_code, info.type_code),
                description: pick(&live.description, info.description),
                owner: pick(&live.owner, info.owner.clone()),
                military: if live.owner.is_empty() {
                    info.military
                } else {
                    live.military
                },
                country: "",
            };
        }
        drop(i);
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
    fn a_live_answer_is_read() {
        // adsbdb's answers for 780ABB and AE2FAC (2026-11-21), shortened.
        let cathay = r#"{"response":{"aircraft":{"type":"A350 941","icao_type":"A359","manufacturer":"Airbus","mode_s":"780ABB","registration":"B-LRP","registered_owner_country_iso_name":"CN","registered_owner":"Cathay Pacific Airways","url_photo":null}}}"#;
        let a = parse_live(cathay).expect("found");
        assert_eq!(
            (a.registration.as_str(), a.type_code.as_str()),
            ("B-LRP", "A359")
        );
        assert_eq!(a.owner, "Cathay Pacific Airways");
        assert_eq!(a.description, "Airbus A350 941");
        assert!(!a.military);
        let usaf = r#"{"response":{"aircraft":{"type":"C-17A Globemaster III","icao_type":"C17","manufacturer":"Boeing","registration":"08-8200","registered_owner":"United States Air Force"}}}"#;
        assert!(parse_live(usaf).expect("found").military);
        assert!(parse_live(r#"{"response":"unknown aircraft"}"#).is_none());
        // Escapes, and a line kept and read back.
        let odd = r#"{"response":{"aircraft":{"registration":"A\"B\u00e9","icao_type":"","registered_owner":"Royal Australian Air Force"}}}"#;
        let o = parse_live(odd).unwrap();
        assert_eq!(o.registration, "A\"Bé");
        let (aes, back) = read_live_line(&live_line(0x7CF800, &o)).unwrap();
        assert_eq!(
            (aes, back.owner.as_str(), back.military),
            (0x7CF800, "Royal Australian Air Force", true)
        );
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
