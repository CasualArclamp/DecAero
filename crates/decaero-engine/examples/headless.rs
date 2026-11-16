// made by CasualArclamp
//! Run the engine over a recording as fast as it goes and print what it
//! found: `cargo run --release -p decaero-engine --example headless -- FILE.wav [CENTRE_HZ] [--no-follow] [--add HZ:WIDTH]...`

use decaero_engine::{Command, Engine, FileSource, Options};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let path = args.first().expect("a WAV file");
    let center_hz = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(0.0);
    // `--add HZ:WIDTH` draws a decoder's box by hand, as a drag in the GUI
    // does; `--no-follow` leaves everything else alone.
    let follow = !args.iter().any(|a| a == "--no-follow");
    // Feeds out: `--json-udp HOST:PORT`, `--sbs listen:PORT|HOST:PORT`,
    // `--station NAME`.
    let value = |name: &str| {
        args.windows(2)
            .find(|w| w[0] == name)
            .map(|w| w[1].clone())
            .unwrap_or_default()
    };
    let feeds = decaero_engine::feed::FeedOptions {
        json_udp: value("--json-udp"),
        sbs: value("--sbs"),
        station: value("--station"),
    };
    // A URL is a live input, run for `--seconds N` (30 by default).
    let live = path.contains("://");
    let seconds: f64 = args
        .windows(2)
        .find(|w| w[0] == "--seconds")
        .and_then(|w| w[1].parse().ok())
        .unwrap_or(30.0);
    let source: Box<dyn decaero_engine::Source> = if live {
        let rate = args
            .windows(2)
            .find(|w| w[0] == "--rate")
            .and_then(|w| w[1].parse().ok())
            .unwrap_or(625_000.0);
        let format = args
            .windows(2)
            .find(|w| w[0] == "--iq")
            .and_then(|w| decaero_engine::net::Format::parse(&w[1]))
            .unwrap_or(decaero_engine::net::Format::I16);
        let options = decaero_engine::net::SdrppOptions {
            center_hz,
            ..Default::default()
        };
        Box::new(decaero_engine::net::open(path, rate, format, options).expect("connect"))
    } else {
        Box::new(FileSource::open(std::path::Path::new(path), false).expect("open the recording"))
    };
    let engine = Engine::start(
        source,
        Options {
            realtime: false,
            center_hz,
            follow,
            feeds,
            ..Options::default()
        },
    );
    for pair in args.windows(2).filter(|w| w[0] == "--add") {
        if let Some((hz, width)) = pair[1].split_once(':')
            && let (Ok(offset_hz), Ok(width_hz)) = (hz.parse(), width.parse())
        {
            engine.send(Command::Add {
                offset_hz,
                width_hz,
            });
        }
    }
    let began = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let v = engine.view();
        if v.ended || (live && v.seconds >= seconds) {
            // Give the workers a moment to report their last frames.
            std::thread::sleep(std::time::Duration::from_millis(500));
            break;
        }
    }
    let v = engine.view();
    println!(
        "{} · {:.1} s of input in {:.1} s · front end load {:.2}",
        v.source,
        v.seconds,
        began.elapsed().as_secs_f64(),
        v.load
    );
    let mut channels = v.channels.clone();
    channels.sort_by(|a, b| a.carrier_hz.total_cmp(&b.carrier_hz));
    for c in &channels {
        println!(
            "#{:<3} {:>10.1} Hz  {:<8} {}  Eb/N0 {:>4.1} dB  margin {:>+5.1} dB  FEC load {:>3.0}%  BER {:.1e}  frames {:>3}  CRC {}/{}  cpu {:.2}  {}",
            c.id,
            c.carrier_hz,
            c.kind.label(),
            if c.locked { "LOCK" } else { "    " },
            c.eb_n0_db,
            c.margin_db,
            c.fec_load * 100.0,
            c.pre_fec_ber,
            c.frames,
            c.crc_good,
            c.crc_good + c.crc_bad,
            c.cpu,
            c.aes.map(|a| format!("AES {a:06X}")).unwrap_or_default()
        );
    }
    let log = engine.log_after(0);
    println!(
        "{} log lines, {} messages, {} calls",
        log.len(),
        log.iter().filter(|l| l.message).count(),
        v.calls.len()
    );
    for l in log.iter().filter(|l| l.message).take(12) {
        println!(
            "  {:6.1}s #{} {} {} | {}",
            l.seconds,
            l.channel,
            l.aes.map(|a| format!("{a:06X}")).unwrap_or_default(),
            l.title,
            l.text.replace(['\r', '\n'], " ")
        );
    }
}
