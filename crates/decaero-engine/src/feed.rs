// made by CasualArclamp
//! Feeds out to other programs, the way JAERO does them (MIT; `mainwindow.cpp`
//! for the JSON, `sbs1.cpp` for the positions):
//!
//! - every ACARS message as one JSON object in a UDP datagram, in JAERO's
//!   "JSONdump" layout, which is what ACARS collectors take from JAERO;
//! - every ADS-C position as a BaseStation (SBS) `MSG,3` line over TCP, for
//!   a map such as Virtual Radar Server. Either we listen and the map
//!   connects, or we connect to the map.
//!
//! One thread owns the sockets, so a slow or dead peer never holds up the
//! receiver.

use decaero_core::adsc::{Downlink, Group};
use decaero_core::su::Acars;
use std::io::Write;
use std::net::{TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Where the feeds go. Empty strings: no feed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeedOptions {
    /// `host:port` of every collector that gets the JSON, separated by
    /// commas or spaces.
    pub json_udp: String,
    /// `listen:PORT` (or `listen:ADDRESS:PORT`) to serve SBS lines, or
    /// `host:port` to connect to.
    pub sbs: String,
    /// Station name put in the JSON, if any.
    pub station: String,
}

/// How the feeds are doing, for the status line.
#[derive(Debug, Clone, Default)]
pub struct FeedCounts {
    pub json_targets: usize,
    pub json_sent: u64,
    pub sbs_on: bool,
    pub sbs_peers: usize,
    pub sbs_sent: u64,
    /// The last thing that went wrong, empty if nothing has.
    pub error: String,
}

enum Msg {
    Configure(FeedOptions),
    Json(String),
    Sbs(String),
}

pub struct Feeds {
    tx: Option<Sender<Msg>>,
    counts: Arc<Mutex<FeedCounts>>,
    station: String,
    join: Option<JoinHandle<()>>,
}

impl Feeds {
    pub fn start(options: FeedOptions) -> Self {
        let (tx, rx) = mpsc::channel();
        let counts = Arc::new(Mutex::new(FeedCounts::default()));
        let c = counts.clone();
        let join = std::thread::Builder::new()
            .name("feeds".into())
            .spawn(move || run(rx, c))
            .ok();
        let mut feeds = Self {
            tx: Some(tx),
            counts,
            station: String::new(),
            join,
        };
        feeds.configure(options);
        feeds
    }

    pub fn configure(&mut self, options: FeedOptions) {
        self.station = options.station.trim().to_string();
        self.send(Msg::Configure(options));
    }

    pub fn counts(&self) -> FeedCounts {
        self.counts.lock().unwrap().clone()
    }

    fn send(&self, m: Msg) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(m);
        }
    }

    /// One ACARS message, with what an aircraft's text said if it came from
    /// one.
    pub fn acars(&self, link: &Link, acars: &Acars, down: Option<&Downlink>) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        self.send(Msg::Json(json(link, acars, down, &self.station, now)));
        if let Some(line) = down.and_then(|d| sbs(link.aes, d, now)) {
            self.send(Msg::Sbs(line));
        }
    }
}

impl Drop for Feeds {
    fn drop(&mut self) {
        // Closing the channel ends the thread.
        self.tx = None;
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Who a message was between.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Link {
    pub aes: u32,
    pub ges: u8,
    pub q_no: u8,
    pub ref_no: u8,
    /// Sent by the aircraft (R and T channels) and not to it (P channel).
    pub from_aircraft: bool,
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// JAERO's "JSONdump" object for one message. Keys in the order Qt writes
/// them (sorted), so the text matches JAERO's for the same message.
pub fn json(
    link: &Link,
    acars: &Acars,
    down: Option<&Downlink>,
    station: &str,
    now: Duration,
) -> String {
    // JAERO tidies the text the same way before sending it.
    let mut text = acars.text.replace('\r', "\n").replace("\n\n", "\n");
    if text.ends_with('\n') {
        text.pop();
    }
    if text.starts_with('\n') {
        text.remove(0);
    }
    let text = text.replace('\n', "\n\t");
    let ack = if acars.ack == '\x15' { '!' } else { acars.ack };

    let mut a = format!(
        "\"ack\":{},\"blk_id\":{}",
        quote(&ack.to_string()),
        quote(&acars.block_id.to_string())
    );
    if let Some(flight) = down.and_then(|d| d.flight.as_deref()) {
        a.push_str(&format!(",\"flight\":{}", quote(flight)));
    }
    a.push_str(&format!(
        ",\"label\":{},\"mode\":{}",
        quote(&acars.label),
        quote(&acars.mode.to_string())
    ));
    if !text.is_empty() {
        a.push_str(&format!(",\"msg_text\":{}", quote(&text)));
    }
    a.push_str(&format!(",\"reg\":{}", quote(&acars.registration)));

    let aes = format!(
        "{{\"addr\":\"{:06X}\",\"type\":\"Aircraft Earth Station\"}}",
        link.aes
    );
    let ges = format!(
        "{{\"addr\":\"{:02X}\",\"type\":\"Ground Earth Station\"}}",
        link.ges
    );
    let (src, dst) = if link.from_aircraft {
        (&aes, &ges)
    } else {
        (&ges, &aes)
    };
    let mut out = format!(
        "{{\"app\":{{\"name\":\"DecAero\",\"ver\":\"{}\"}},\"isu\":{{\"acars\":{{{a}}},\"dst\":{dst},\"qno\":\"{:02X}\",\"refno\":\"{:02X}\",\"src\":{src}}}",
        env!("CARGO_PKG_VERSION"),
        link.q_no,
        link.ref_no
    );
    if !station.is_empty() {
        out.push_str(&format!(",\"station\":{}", quote(station)));
    }
    out.push_str(&format!(
        ",\"t\":{{\"sec\":{},\"usec\":{}}}}}",
        now.as_secs(),
        now.subsec_millis() * 1000
    ));
    out
}

/// Year, month and day of a count of days from 1970-01-01 (Howard Hinnant's
/// `civil_from_days`).
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// "2026/11/16,14:53:49.986" for milliseconds since 1970, UTC.
fn stamp(ms: u64) -> String {
    let s = ms / 1000;
    let (y, m, d) = civil((s / 86_400) as i64);
    format!(
        "{y:04}/{m:02}/{d:02},{:02}:{:02}:{:02}.{:03}",
        s / 3600 % 24,
        s / 60 % 60,
        s % 60,
        ms % 1000
    )
}

/// The BaseStation line for a report with a position (JAERO
/// `SBS1::DownlinkGroupsSlot`): the time the aircraft took the position, the
/// time now, flight, altitude, ground speed, track, position, vertical rate.
pub fn sbs(aes: u32, d: &Downlink, now: Duration) -> Option<String> {
    let Some(Group::Position {
        latitude,
        longitude,
        altitude_ft,
        past_hour_s,
        ..
    }) = d.position()
    else {
        return None;
    };
    // The report only says how far past the hour it was: the hour is the
    // one that puts it nearest to now.
    let now_ms = now.as_millis() as u64;
    let hour = 3_600_000u64;
    let mut taken = now_ms / hour * hour + (*past_hour_s * 1000.0) as u64;
    if taken > now_ms + hour / 2 {
        taken = taken.saturating_sub(hour);
    } else if taken + hour / 2 < now_ms {
        taken += hour;
    }
    let flight = d
        .groups
        .iter()
        .find_map(|g| match g {
            Group::FlightId(id) => Some(id.as_str()),
            _ => None,
        })
        .or(d.flight.as_deref())
        .unwrap_or("");
    let (speed, track, vertical) = match d.earth_reference() {
        Some(Group::EarthReference {
            true_track,
            ground_speed_kt,
            vertical_fpm,
        }) => (
            format!("{ground_speed_kt:.0}"),
            true_track.map(|t| format!("{t:.0}")).unwrap_or_default(),
            format!("{vertical_fpm:.0}"),
        ),
        _ => Default::default(),
    };
    Some(format!(
        "MSG,3,,,{aes:06X},,{},{},{flight},{altitude_ft:.0},{speed},{track},{latitude:.6},{longitude:.6},{vertical},,0,0,0,0\r\n",
        stamp(taken),
        stamp(now_ms)
    ))
}

enum Sbs {
    Off,
    Listen(TcpListener, Vec<TcpStream>),
    Connect {
        to: String,
        stream: Option<TcpStream>,
        tried: Option<Instant>,
    },
}

fn run(rx: mpsc::Receiver<Msg>, counts: Arc<Mutex<FeedCounts>>) {
    let udp = UdpSocket::bind("0.0.0.0:0").ok();
    let mut targets: Vec<std::net::SocketAddr> = Vec::new();
    let mut sbs = Sbs::Off;
    let note = |e: String| counts.lock().unwrap().error = e;
    loop {
        let msg = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        match msg {
            Some(Msg::Configure(o)) => {
                note(String::new());
                targets.clear();
                for t in o.json_udp.split([',', ' ', ';']).filter(|t| !t.is_empty()) {
                    // IPv4 only: the one socket is an IPv4 one.
                    match t
                        .to_socket_addrs()
                        .ok()
                        .and_then(|mut a| a.find(|a| a.is_ipv4()))
                    {
                        Some(a) => targets.push(a),
                        None => note(format!("JSON feed: cannot use \"{t}\" (host:port)")),
                    }
                }
                let spec = o.sbs.trim();
                sbs = if spec.is_empty() {
                    Sbs::Off
                } else if let Some(port) = spec.strip_prefix("listen:") {
                    let at = if port.contains(':') {
                        port.to_string()
                    } else {
                        format!("0.0.0.0:{port}")
                    };
                    match TcpListener::bind(&at).and_then(|l| {
                        l.set_nonblocking(true)?;
                        Ok(l)
                    }) {
                        Ok(l) => Sbs::Listen(l, Vec::new()),
                        Err(e) => {
                            note(format!("SBS feed: cannot listen on {at}: {e}"));
                            Sbs::Off
                        }
                    }
                } else {
                    Sbs::Connect {
                        to: spec.to_string(),
                        stream: None,
                        tried: None,
                    }
                };
                let mut c = counts.lock().unwrap();
                c.json_targets = targets.len();
                c.sbs_on = !matches!(sbs, Sbs::Off);
            }
            Some(Msg::Json(text)) => {
                if let Some(udp) = &udp {
                    let mut sent = 0;
                    for t in &targets {
                        match udp.send_to(text.as_bytes(), t) {
                            Ok(_) => sent += 1,
                            Err(e) => note(format!("JSON feed to {t}: {e}")),
                        }
                    }
                    if sent > 0 {
                        counts.lock().unwrap().json_sent += 1;
                    }
                }
            }
            Some(Msg::Sbs(line)) => {
                let delivered = match &mut sbs {
                    Sbs::Off => false,
                    Sbs::Listen(_, peers) => {
                        peers.retain_mut(|p| p.write_all(line.as_bytes()).is_ok());
                        !peers.is_empty()
                    }
                    Sbs::Connect { stream, .. } => {
                        if stream
                            .as_mut()
                            .is_some_and(|s| s.write_all(line.as_bytes()).is_err())
                        {
                            *stream = None;
                        }
                        stream.is_some()
                    }
                };
                if delivered {
                    counts.lock().unwrap().sbs_sent += 1;
                }
            }
            None => {}
        }
        // Keep the SBS side connected.
        let peers = match &mut sbs {
            Sbs::Off => 0,
            Sbs::Listen(listener, peers) => {
                while let Ok((stream, _)) = listener.accept() {
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
                    let _ = stream.set_nodelay(true);
                    peers.push(stream);
                }
                peers.len()
            }
            Sbs::Connect { to, stream, tried } => {
                if stream.is_none() && tried.is_none_or(|t| t.elapsed() > Duration::from_secs(5)) {
                    *tried = Some(Instant::now());
                    let found = to.to_socket_addrs().ok().and_then(|mut a| a.next());
                    match found.map(|a| TcpStream::connect_timeout(&a, Duration::from_secs(2))) {
                        Some(Ok(s)) => {
                            let _ = s.set_write_timeout(Some(Duration::from_secs(2)));
                            let _ = s.set_nodelay(true);
                            *stream = Some(s);
                            note(String::new());
                        }
                        Some(Err(e)) => note(format!("SBS feed to {to}: {e}")),
                        None => note(format!("SBS feed: cannot use \"{to}\" (host:port)")),
                    }
                }
                usize::from(stream.is_some())
            }
        };
        counts.lock().unwrap().sbs_peers = peers;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use decaero_core::adsc;
    use std::io::Read;

    fn message(text: &str) -> Acars {
        Acars {
            mode: '2',
            registration: "N705DN".into(),
            ack: '\x15',
            label: "B6".into(),
            block_id: '4',
            text: text.into(),
            more: false,
            parity_ok: true,
        }
    }

    const LINK: Link = Link {
        aes: 0xA9C1F2,
        ges: 0x82,
        q_no: 0x0A,
        ref_no: 3,
        from_aircraft: true,
    };
    // The example in JAERO's arincparse.cpp, behind a made-up header.
    const TEXT: &str = "F58ANZ0124/AKLCDYA.ADS.N705DN07EEE19454DAC7D010D21D0DEEEC44556208024029F0588C71D7884D000E13B90F00000F12C1A280001029305F1019F4";

    #[test]
    fn dates() {
        assert_eq!(stamp(0), "1970/01/01,00:00:00.000");
        // 2008-11-28 14:53:49.986 UTC.
        assert_eq!(stamp(1_227_884_029_986), "2008/11/28,14:53:49.986");
        assert_eq!(civil(19_782), (2024, 2, 29));
    }

    #[test]
    fn json_layout() {
        let a = message("line \"one\"\r\nline two\r\n");
        let j = json(
            &LINK,
            &a,
            None,
            "home",
            Duration::from_millis(1_700_000_000_250),
        );
        assert_eq!(
            j,
            format!(
                "{{\"app\":{{\"name\":\"DecAero\",\"ver\":\"{}\"}},\"isu\":{{\"acars\":{{\"ack\":\"!\",\"blk_id\":\"4\",\"label\":\"B6\",\"mode\":\"2\",\"msg_text\":\"line \\\"one\\\"\\n\\tline two\",\"reg\":\"N705DN\"}},\"dst\":{{\"addr\":\"82\",\"type\":\"Ground Earth Station\"}},\"qno\":\"0A\",\"refno\":\"03\",\"src\":{{\"addr\":\"A9C1F2\",\"type\":\"Aircraft Earth Station\"}}}},\"station\":\"home\",\"t\":{{\"sec\":1700000000,\"usec\":250000}}}}",
                env!("CARGO_PKG_VERSION")
            )
        );
    }

    #[test]
    fn sbs_line() {
        let d = adsc::parse(TEXT);
        let line = sbs(LINK.aes, &d, Duration::from_secs(1_700_000_000)).expect("a position");
        let f: Vec<&str> = line.trim_end().split(',').collect();
        assert_eq!(f.len(), 22, "{line}");
        assert_eq!(&f[..5], ["MSG", "3", "", "", "A9C1F2"]);
        assert_eq!(f[10], "NZ124");
        assert!(f[14].parse::<f64>().is_ok() && f[15].parse::<f64>().is_ok());
        // The position was taken within half an hour of now.
        assert!(f[6].starts_with("2023/11/14"));
    }

    #[test]
    fn feeds_reach_their_listeners() {
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        // A free port for the SBS side.
        let port = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let feeds = Feeds::start(FeedOptions {
            json_udp: udp.local_addr().unwrap().to_string(),
            sbs: format!("listen:127.0.0.1:{port}"),
            station: String::new(),
        });
        let mut map = None;
        for _ in 0..50 {
            if let Ok(s) = TcpStream::connect(("127.0.0.1", port)) {
                map = Some(s);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut map = map.expect("the SBS port opens");
        map.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        for _ in 0..50 {
            if feeds.counts().sbs_peers == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let a = message(TEXT);
        let d = adsc::parse(TEXT);
        feeds.acars(&LINK, &a, Some(&d));

        let mut buf = [0u8; 2048];
        let n = udp.recv(&mut buf).expect("a datagram");
        let got = std::str::from_utf8(&buf[..n]).unwrap();
        assert!(
            got.contains("\"flight\":\"NZ124\"") && got.contains("AKLCDYA"),
            "{got}"
        );
        let n = map.read(&mut buf).expect("an SBS line");
        assert!(buf[..n].starts_with(b"MSG,3,,,A9C1F2,"));
        // The count goes up just after the line has gone out.
        for _ in 0..50 {
            if feeds.counts().sbs_sent == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let c = feeds.counts();
        assert_eq!((c.json_sent, c.sbs_sent, c.json_targets), (1, 1, 1));
    }
}
