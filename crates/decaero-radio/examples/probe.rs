// made by CasualArclamp
//! Open a radio for a few seconds, receive only, and say what came out:
//! `cargo run --release -p decaero-radio --example probe -- pluto 1545.0 [RATE_HZ]`
//!
//! It reads three seconds, retunes by 100 kHz, changes each gain stage once
//! and reads again, so a radio's whole control path is exercised without the
//! GUI. This takes the device: run it only when the radio is free.

use decaero_engine::Source;
use decaero_radio::{RadioRequest, list, open};
use std::time::Instant;

/// Read `seconds` of samples; returns (samples, mean power in dBFS, peak).
fn listen(source: &mut dyn Source, seconds: f64) -> Option<(usize, f32, f32)> {
    let want = (source.rate() * seconds) as usize;
    let block = (source.rate() / 20.0) as usize;
    let (mut got, mut power, mut peak) = (0usize, 0f64, 0f32);
    while got < want {
        let samples = source.read(block)?;
        for s in &samples {
            power += f64::from(s.norm_sqr());
            peak = peak.max(s.re.abs()).max(s.im.abs());
        }
        got += samples.len();
    }
    Some((
        got,
        10.0 * (power / got.max(1) as f64).max(1e-12).log10() as f32,
        peak,
    ))
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let radios = list();
    let Some(key) = args.first() else {
        for r in &radios {
            println!(
                "{:8} {}  {:?} MS/s",
                r.key,
                r.name,
                r.rates.iter().map(|x| x / 1e6).collect::<Vec<_>>()
            );
        }
        return;
    };
    let mhz: f64 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(1545.0);
    let info = radios
        .iter()
        .find(|r| r.key == *key)
        .expect("a radio key from the list");
    let sample_rate = args
        .get(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(info.rates[0]);
    let request = RadioRequest {
        key: key.clone(),
        center_hz: mhz * 1e6,
        sample_rate,
        gains: vec![],
        bias_tee: false,
    };
    let began = Instant::now();
    let mut source = match open(&request) {
        Ok(s) => s,
        Err(e) => {
            println!("could not open {}: {e}", info.name);
            return;
        }
    };
    println!(
        "{} open in {:.1} s",
        source.describe(),
        began.elapsed().as_secs_f64()
    );
    let control = source.control();
    let report = |what: &str, r: Option<(usize, f32, f32)>, t: f64| match r {
        Some((n, db, peak)) => {
            println!("{what:28} {n:>9} samples in {t:4.1} s  {db:6.1} dBFS  peak {peak:.3}")
        }
        None => println!("{what:28} the radio stopped"),
    };
    let t = Instant::now();
    let r = listen(&mut source, 3.0);
    report("as opened", r, t.elapsed().as_secs_f64());

    source.tune(mhz * 1e6 + 100e3);
    let t = Instant::now();
    let r = listen(&mut source, 1.0);
    report("retuned +100 kHz", r, t.elapsed().as_secs_f64());

    if let Some(control) = control {
        for g in &info.gains {
            // To the other end of its range and back.
            let other = if (g.value - g.min).abs() < (g.max - g.value).abs() {
                g.max
            } else {
                g.min
            };
            for v in [other, g.value] {
                match control.set_gain(&g.name, v) {
                    Ok(()) => {
                        let t = Instant::now();
                        let r = listen(&mut source, 1.0);
                        report(&format!("{} = {v}", g.name), r, t.elapsed().as_secs_f64());
                    }
                    Err(e) => println!("{} = {v}: {e}", g.name),
                }
            }
        }
    }
    println!("{}", source.describe());
}
