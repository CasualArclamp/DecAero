// made by CasualArclamp
//! Run the engine over a recording as fast as it goes and print what it
//! found: `cargo run --release -p decaero-engine --example headless -- FILE.wav [CENTRE_HZ]`

use decaero_engine::{Engine, FileSource, Options};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("a WAV file");
    let center_hz = args.next().and_then(|a| a.parse().ok()).unwrap_or(0.0);
    let source = FileSource::open(std::path::Path::new(&path), false).expect("open the recording");
    let engine = Engine::start(
        Box::new(source),
        Options {
            realtime: false,
            center_hz,
            ..Options::default()
        },
    );
    let began = std::time::Instant::now();
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let v = engine.view();
        if v.ended {
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
