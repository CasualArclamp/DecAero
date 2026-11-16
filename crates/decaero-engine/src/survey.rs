// made by CasualArclamp
//! Carriers in an averaged spectrum: runs of bins standing out of the noise
//! floor. The idea is DecSAT's `detect_carriers` (floor from a low
//! percentile, runs above a threshold, small gaps bridged), cut down to what
//! narrow Aero carriers need.

/// One carrier seen in the spectrum.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Carrier {
    /// Offset from the centre of the input, in Hz.
    pub center_hz: f64,
    pub width_hz: f64,
    /// Peak over the floor, in dB.
    pub peak_db: f32,
}

#[derive(Debug, Clone, Default)]
pub struct Survey {
    pub floor_db: f32,
    pub carriers: Vec<Carrier>,
}

/// `spectrum_db` runs from the lowest frequency to the highest over `rate` Hz.
pub fn carriers(spectrum_db: &[f32], rate: f64, min_db: f32) -> Survey {
    let n = spectrum_db.len();
    if n < 64 {
        return Survey::default();
    }
    let bin_hz = rate / n as f64;
    // Smooth over about 150 Hz so a 600 bit/s carrier is still a clear lump.
    let half = ((75.0 / bin_hz).round() as usize).max(1);
    let smooth: Vec<f32> = (0..n)
        .map(|i| {
            let (a, b) = (i.saturating_sub(half), (i + half + 1).min(n));
            spectrum_db[a..b].iter().sum::<f32>() / (b - a) as f32
        })
        .collect();
    let mut sorted = smooth.clone();
    sorted.sort_by(f32::total_cmp);
    // The floor is a low percentile of the passband only: a filtered input (a
    // receiver's audio, a decimated SDR) has dead edges far below the noise,
    // which must not count.
    let cut = sorted[n * 9 / 10] - 15.0;
    let live = sorted.partition_point(|&v| v < cut);
    let floor_db = sorted[live + (n - live) * 3 / 10];
    let above = |i: usize| i < n && smooth[i] > floor_db + min_db;
    // Bridge gaps up to 100 Hz inside a carrier.
    let gap = ((100.0 / bin_hz).round() as usize).max(1);
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        if !above(i) {
            i += 1;
            continue;
        }
        let mut j = i;
        while above(j) || (j..j + gap).any(above) {
            j += 1;
        }
        let width_hz = (j - i) as f64 * bin_hz;
        // Narrower than 300 Hz is a spur, wider than 20 kHz is not Aero.
        if (300.0..20_000.0).contains(&width_hz) {
            let center_hz = ((i + j) as f64 / 2.0 - n as f64 / 2.0) * bin_hz;
            let peak_db = smooth[i..j].iter().copied().fold(f32::MIN, f32::max) - floor_db;
            out.push(Carrier {
                center_hz,
                width_hz,
                peak_db,
            });
        }
        i = j;
    }
    Survey {
        floor_db,
        carriers: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_two_lumps_and_ignores_a_spur() {
        let n = 8192;
        let rate = 625_000.0;
        let bin = rate / n as f64;
        let mut db = vec![-70.0f32; n];
        let mut lump = |hz: f64, width: f64, level: f32| {
            let (a, b) = (
                ((hz - width / 2.0) / bin) as i64 + n as i64 / 2,
                ((hz + width / 2.0) / bin) as i64 + n as i64 / 2,
            );
            for v in &mut db[a as usize..b as usize] {
                *v = level;
            }
        };
        lump(-100_000.0, 800.0, -57.0);
        lump(50_000.0, 6000.0, -62.0);
        lump(120_000.0, 80.0, -40.0);
        let s = carriers(&db, rate, 4.0);
        assert_eq!(s.carriers.len(), 2, "{:?}", s.carriers);
        assert!((s.carriers[0].center_hz + 100_000.0).abs() < 200.0);
        assert!((s.carriers[1].width_hz - 6000.0).abs() < 600.0);
        assert!((s.floor_db + 70.0).abs() < 0.5);
    }
}
